//! A pseudo-terminal from `openpty` with the child as its own session leader.
#![allow(unsafe_code)]

use std::{
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child as Process, Command, Stdio},
};

use super::WinSize;

/// The master side of a pseudo-terminal whose slave runs the child.
pub struct Pty {
    /// Output from the child.
    pub reader: File,
    /// Input to the child.
    pub writer: File,
    /// The child, reaped by whoever reads its output.
    pub child: Child,
    /// The terminal itself, kept for sizing and hanging up.
    pub console: Console,
}

/// The forked shell.
pub struct Child {
    process: Process,
}

impl Child {
    /// The child's process id; it also leads its own process group.
    pub fn id(&self) -> u32 {
        self.process.id()
    }

    /// Blocks until the child exits and reaps it.
    pub fn wait(&mut self) {
        let _ = self.process.wait();
    }
}

/// The master side, used to resize the terminal and read its process group.
pub struct Console {
    master: File,
}

impl Console {
    pub fn resize(&self, size: WinSize) -> io::Result<()> {
        let winsize = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: size.width,
            ws_ypixel: size.height,
        };
        // `request` is `c_ulong`, which is wider than the constants on macOS.
        let request = libc::TIOCSWINSZ as libc::c_ulong;
        // SAFETY: valid descriptor and winsize pointer.
        check(unsafe { libc::ioctl(self.master.as_raw_fd(), request, &winsize) }).map(drop)
    }

    /// The foreground process group of the terminal, when it has one.
    pub fn foreground(&self) -> Option<u32> {
        // SAFETY: tcgetpgrp only reads the terminal's state from a valid descriptor.
        let pgrp = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
        (pgrp > 0).then_some(pgrp as u32)
    }

    /// Asks the child's session to hang up, as closing a terminal window does.
    pub fn hang_up(&mut self, pid: u32) {
        // SAFETY: kill has no memory effects; the child leads its own process group.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGHUP);
        }
    }
}

fn check(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

impl Pty {
    /// Starts `shell` on a new pseudo-terminal of `size`, in `cwd` when given.
    pub fn spawn(shell: &str, cwd: Option<&Path>, size: WinSize) -> io::Result<Self> {
        let (mut master, mut slave) = (0, 0);
        // `openpty` wants a mutable winsize on macOS and a shared one on Linux;
        // `&mut` coerces to either.
        let mut winsize = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: size.width,
            ws_ypixel: size.height,
        };
        // SAFETY: the out-pointers are valid; name, termios are optional (null).
        check(unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                // macOS takes `*mut winsize`, Linux `*const`; a raw pointer
                // satisfies both without a mutable borrow clippy flags.
                &raw mut winsize,
            )
        })?;
        // SAFETY: openpty returned two fresh descriptors that we now own.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // Other terminals' children must not inherit this master.
        // SAFETY: plain fcntl on a valid descriptor.
        check(unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) })?;

        let mut command = Command::new(shell);
        command
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave))
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "axo");
        if let Some(cwd) = cwd.filter(|dir| dir.is_dir()) {
            command.current_dir(cwd);
        }
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(|| {
                check(libc::setsid())?;
                check(libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0))?;
                for signal in [
                    libc::SIGCHLD,
                    libc::SIGHUP,
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTERM,
                    libc::SIGALRM,
                ] {
                    libc::signal(signal, libc::SIG_DFL);
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        let master = File::from(master);
        let console = Console {
            master: master.try_clone()?,
        };
        Ok(Self {
            reader: master.try_clone()?,
            writer: master,
            child: Child { process: child },
            console,
        })
    }
}

/// The shell's working directory, for opening new terminals next to it.
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// The command of `pid`, for a title when the program sets none.
pub fn process_name(pid: u32) -> Option<String> {
    let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = name.trim_end();
    (!name.is_empty()).then(|| name.to_owned())
}
