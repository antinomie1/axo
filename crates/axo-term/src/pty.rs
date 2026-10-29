//! A pseudo-terminal with a child shell.
#![allow(unsafe_code)]

use std::{
    fs::File,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
};

/// The terminal size reported to the child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WinSize {
    pub cols: u16,
    pub rows: u16,
    pub width: u16,
    pub height: u16,
}

impl WinSize {
    fn raw(self) -> libc::winsize {
        libc::winsize {
            ws_row: self.rows,
            ws_col: self.cols,
            ws_xpixel: self.width,
            ws_ypixel: self.height,
        }
    }
}

/// The master side of a pseudo-terminal whose slave runs `child`.
pub struct Pty {
    pub master: File,
    pub child: Child,
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
        let winsize = size.raw();
        // SAFETY: the out-pointers are valid; name, termios are optional (null).
        check(unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &winsize,
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
                check(libc::ioctl(0, libc::TIOCSCTTY, 0))?;
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
        Ok(Self {
            master: File::from(master),
            child,
        })
    }

    pub fn resize(master: &File, size: WinSize) -> io::Result<()> {
        let winsize = size.raw();
        // SAFETY: valid descriptor and winsize pointer.
        check(unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &winsize) }).map(drop)
    }
}

/// Asks the child's session to hang up, as closing a terminal window does.
pub fn hang_up(pid: u32) {
    // SAFETY: kill has no memory effects; the child leads its own process group.
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGHUP);
    }
}
