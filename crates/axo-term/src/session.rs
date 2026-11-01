//! A running terminal: shell, reader and writer threads, and the shared state.

use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

use crate::{
    pty::{Console, Pty, WinSize, process_cwd, process_name},
    term::Term,
};

/// What the reader thread reports to the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// New output was applied; sent once until [`Session::take_dirty`].
    Wakeup,
    /// The child exited.
    Exit,
}

pub struct Options<'a> {
    pub shell: &'a str,
    pub cwd: Option<&'a Path>,
    pub scrollback: usize,
}

/// One shell on its own pseudo-terminal.
///
/// Output is parsed on the reader thread, so the UI only locks the [`Term`]
/// to draw it. Wakeups coalesce on a flag: while one is pending, more output
/// sends nothing, so a flood costs one UI wakeup per drawn frame.
pub struct Session {
    term: Arc<Mutex<Term>>,
    dirty: Arc<AtomicBool>,
    input: mpsc::Sender<Vec<u8>>,
    /// The pseudo-terminal's own side: sizing, the foreground group and
    /// hanging up. The child and its pipes live in the threads below.
    console: Console,
    pid: u32,
}

impl Session {
    pub fn spawn(
        options: &Options<'_>,
        size: WinSize,
        notify: impl Fn(Event) + Send + 'static,
    ) -> io::Result<Self> {
        let Pty {
            mut reader,
            mut writer,
            mut child,
            console,
        } = Pty::spawn(options.shell, options.cwd, size)?;
        let pid = child.id();
        let term = Arc::new(Mutex::new(Term::new(
            size.cols as usize,
            size.rows as usize,
            options.scrollback,
        )));
        let dirty = Arc::new(AtomicBool::new(false));
        let (input, queue) = mpsc::channel::<Vec<u8>>();

        thread::Builder::new()
            .name("axo-pty-write".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                for bytes in queue {
                    if writer.write_all(&bytes).is_err() {
                        break;
                    }
                }
            })?;

        let (shared, flag, responses) = (term.clone(), dirty.clone(), input.clone());
        thread::Builder::new()
            .name("axo-pty-read".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                let mut buf = vec![0u8; 128 * 1024];
                loop {
                    let n = match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    };
                    let reply = {
                        let mut term = shared.lock().unwrap();
                        term.advance(&buf[..n]);
                        term.take_responses()
                    };
                    if let Some(reply) = reply {
                        let _ = responses.send(reply);
                    }
                    // Coalesced: one wakeup stays pending until the UI paints.
                    if !flag.swap(true, Ordering::AcqRel) {
                        notify(Event::Wakeup);
                    }
                }
                child.wait();
                notify(Event::Exit);
            })?;

        Ok(Self {
            term,
            dirty,
            input,
            console,
            pid,
        })
    }

    /// Locks the terminal state; keep the guard short, the reader waits on it.
    pub fn term(&self) -> MutexGuard<'_, Term> {
        self.term.lock().unwrap()
    }

    /// Clears and returns the pending-output flag; the next output wakes again.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.input.send(bytes.into());
    }

    pub fn resize(&self, size: WinSize) {
        {
            let mut term = self.term();
            term.resize(size.cols as usize, size.rows as usize);
            term.set_cell_size(
                u32::from(size.width) / u32::from(size.cols).max(1),
                u32::from(size.height) / u32::from(size.rows).max(1),
            );
        }
        let _ = self.console.resize(size);
    }

    /// The shell's working directory, for opening new terminals next to it.
    /// The shell's own report (OSC 7) wins over asking the platform, because a
    /// shell may sit anywhere while its process keeps the directory it started
    /// in.
    pub fn cwd(&self) -> Option<PathBuf> {
        let reported = self.term().cwd().map(PathBuf::from);
        reported.or_else(|| process_cwd(self.pid))
    }

    /// The command in the foreground, and whether it is the shell itself
    /// (idle at its prompt). Only Unix can answer; elsewhere the title falls
    /// back to the child's own title or directory.
    pub fn foreground(&self) -> Option<(String, bool)> {
        let pgrp = self.console.foreground()?;
        let name = process_name(pgrp)?;
        Some((name, pgrp == self.pid))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.console.hang_up(self.pid);
    }
}
