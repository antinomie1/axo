//! A ConPTY pseudo console with the child attached to it.
#![allow(unsafe_code)]

use std::{
    ffi::{OsStr, c_void},
    fs::File,
    io,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle},
    },
    path::{Path, PathBuf},
    ptr,
    sync::{Arc, Mutex},
    thread,
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        Console::{ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole, COORD},
        Pipes::CreatePipe,
        Threading::{
            CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
            EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
            InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
            UpdateProcThreadAttribute,
            WaitForSingleObject,
        },
    },
};

use super::WinSize;

/// A ConPTY pseudo console whose client process runs the shell.
pub struct Pty {
    /// Output from the child.
    pub reader: File,
    /// Input to the child.
    pub writer: File,
    /// The child, waited for by whoever reads its output.
    pub child: Child,
    /// The pseudo console, kept for sizing and tearing the child down.
    pub console: Console,
}

/// The client process attached to the pseudo console.
pub struct Child {
    process: Option<OwnedHandle>,
    id: u32,
}

impl Child {
    /// The child's process id.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Blocks until the child exits and reaps it.
    pub fn wait(&mut self) {
        if let Some(process) = self.process.take() {
            // SAFETY: the handle is live until the end of this block.
            unsafe {
                WaitForSingleObject(process.as_raw_handle() as HANDLE, INFINITE);
                let mut code = 0;
                GetExitCodeProcess(process.as_raw_handle() as HANDLE, &mut code);
            }
        }
    }
}

/// The pseudo console handle, shared with the thread that closes it when the
/// child exits. Closing it hangs up the client, as closing a terminal window
/// does, so it closes exactly once and never twice.
pub struct Console {
    console: Arc<Mutex<Option<HPCON>>>,
}

impl Console {
    pub fn resize(&self, size: WinSize) -> io::Result<()> {
        let console = self.console.lock().unwrap();
        let Some(console) = *console else {
            return Err(io::Error::other("the pseudo console is closed"));
        };
        // SAFETY: the handle is live while the lock keeps it from closing.
        let result = unsafe { ResizePseudoConsole(console, coord(size)) };
        match result < 0 {
            true => Err(hresult(result, "ResizePseudoConsole")),
            false => Ok(()),
        }
    }

    /// ConPTY has no foreground process-group query; the title falls back to
    /// the child's own title or directory.
    pub fn foreground(&self) -> Option<u32> {
        None
    }

    /// Closing the pseudo console terminates the client process.
    pub fn hang_up(&mut self, _pid: u32) {
        self.close();
    }

    fn close(&mut self) {
        close(&self.console);
    }
}

/// Closes the pseudo console once. Outside the lock: closing waits until the
/// reader has drained the last output.
fn close(console: &Mutex<Option<HPCON>>) {
    let taken = console.lock().unwrap().take();
    if let Some(console) = taken {
        // SAFETY: the handle was live and is taken, so this runs once.
        unsafe { ClosePseudoConsole(console) };
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        self.close();
    }
}

fn coord(size: WinSize) -> COORD {
    COORD {
        X: size.cols as i16,
        Y: size.rows as i16,
    }
}

/// A failed `HRESULT`, whose low 16 bits are the `Win32` code.
fn hresult(result: i32, call: &str) -> io::Error {
    io::Error::other(format!("{call} failed ({:#06x})", result as u32 & 0xffff))
}

/// A `CreatePipe` pair: the read end and the write end.
fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write): (HANDLE, HANDLE) = (ptr::null_mut(), ptr::null_mut());
    // SAFETY: the out-pointers are valid; no security attributes or size needed.
    if unsafe { CreatePipe(&mut read, &mut write, ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreatePipe returned two fresh handles that we now own.
    unsafe {
        Ok((
            OwnedHandle::from_raw_handle(read as RawHandle),
            OwnedHandle::from_raw_handle(write as RawHandle),
        ))
    }
}

/// A NUL-terminated UTF-16 copy of `text`.
fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain([0]).collect()
}

/// The child's environment: ours with the terminal's variables set, sorted the
/// way `CreateProcessW` expects, and double-NUL terminated.
fn environment() -> Vec<u16> {
    const TERM: [(&str, &str); 3] = [
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("TERM_PROGRAM", "axo"),
    ];
    let mut vars: Vec<(String, Vec<u16>)> = Vec::new();
    for (name, value) in std::env::vars_os() {
        let name = name.to_string_lossy().into_owned();
        // `=C:` style drive variables cannot be expressed in a block.
        if name.is_empty() || name.contains('=') {
            continue;
        }
        push(&mut vars, &name, value.as_os_str());
    }
    for (name, value) in TERM {
        push(&mut vars, name, OsStr::new(value));
    }
    vars.sort_by(|a, b| a.0.cmp(&b.0));
    let mut block = Vec::with_capacity(vars.iter().map(|(_, entry)| entry.len() + 1).sum());
    for (_, entry) in vars {
        block.extend(entry);
        block.push(0);
    }
    block.push(0);
    block
}

/// Records `NAME=VALUE`, replacing an earlier entry with the same name.
fn push(vars: &mut Vec<(String, Vec<u16>)>, name: &str, value: &OsStr) {
    let key = name.to_uppercase();
    let mut entry: Vec<u16> = OsStr::new(name).encode_wide().collect();
    entry.push(u16::from(b'='));
    entry.extend(value.encode_wide());
    match vars.iter_mut().find(|(existing, _)| *existing == key) {
        Some((_, current)) => *current = entry,
        None => vars.push((key, entry)),
    }
}

/// Owns the attribute list's storage, which must outlive `CreateProcessW`, and
/// deletes the list however the call ends.
struct Attributes {
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    /// The list lives here; its pointer must stay valid until the child starts.
    _storage: Vec<usize>,
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: the list was initialized by `conpty` and is unused afterwards.
        unsafe { DeleteProcThreadAttributeList(self.list) };
    }
}

/// Creates the pseudo console and the attribute list that binds it to a child,
/// returning the console, that list and our ends of its input and output pipes.
fn conpty(size: WinSize) -> io::Result<(HPCON, Attributes, File, File)> {
    // The child reads what we write, and we read what the child writes.
    let (console_input, input) = pipe()?;
    let (output, console_output) = pipe()?;
    let mut console: HPCON = 0;
    // SAFETY: the handles are live and the pseudo console duplicates them.
    let result = unsafe {
        CreatePseudoConsole(
            coord(size),
            console_input.as_raw_handle() as HANDLE,
            console_output.as_raw_handle() as HANDLE,
            0,
            &mut console,
        )
    };
    if result < 0 {
        return Err(hresult(result, "CreatePseudoConsole"));
    }
    // The pseudo console has its own copies; ours must not keep them open.
    drop((console_input, console_output));

    let mut bytes = 0usize;
    // SAFETY: a null list is the documented size query; it reports the size.
    unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut bytes) };
    let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
    let list = storage.as_mut_ptr().cast::<c_void>();
    // SAFETY: `storage` holds at least the requested size, aligned for a pointer.
    let initialized = unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut bytes) } != 0;
    if !initialized {
        // SAFETY: the pseudo console was created above and nothing else owns it.
        unsafe { ClosePseudoConsole(console) };
        return Err(io::Error::last_os_error());
    }
    let attributes = Attributes {
        list,
        _storage: storage,
    };
    // SAFETY: the list has room for one attribute. This attribute's value is
    // the `HPCON` itself, not a pointer to it: given the handle's address, the
    // child attaches to no console and fails to start with `0xC0000142`.
    let updated = unsafe {
        UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            console as *const c_void,
            size_of::<HPCON>(),
            ptr::null_mut(),
            ptr::null(),
        )
    } != 0;
    if !updated {
        // SAFETY: as above; the list is deleted by `attributes` on return.
        unsafe { ClosePseudoConsole(console) };
        return Err(io::Error::last_os_error());
    }
    Ok((
        console,
        attributes,
        File::from(input),
        File::from(output),
    ))
}

impl Pty {
    /// Starts `shell` on a new pseudo console of `size`, in `cwd` when given.
    pub fn spawn(shell: &str, cwd: Option<&Path>, size: WinSize) -> io::Result<Self> {
        let (console, attributes, writer, reader) = conpty(size)?;

        let mut command: Vec<u16> = OsStr::new(shell).encode_wide().chain([0]).collect();
        let directory = cwd
            .filter(|dir| dir.is_dir())
            .map(|dir| wide(dir.as_os_str()));
        let environment = environment();
        let startup = STARTUPINFOEXW {
            StartupInfo: windows_sys::Win32::System::Threading::STARTUPINFOW {
                cb: size_of::<STARTUPINFOEXW>() as u32,
                // Null standard handles: the child uses the pseudo console's,
                // never ours, which a launcher may have redirected.
                dwFlags: STARTF_USESTDHANDLES,
                ..Default::default()
            },
            lpAttributeList: attributes.list,
        };
        let mut process = PROCESS_INFORMATION::default();
        // SAFETY: every pointer is valid for the call: the command line, the
        // environment block and the directory are NUL-terminated, the startup
        // info names the attribute list, and the out-struct is writable.
        let created = unsafe {
            CreateProcessW(
                ptr::null(),
                command.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                false.into(),
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ptr().cast(),
                directory
                    .as_ref()
                    .map_or(ptr::null(), |directory| directory.as_ptr()),
                &startup.StartupInfo,
                &mut process,
            )
        };
        drop(attributes);
        if created == 0 {
            let error = io::Error::last_os_error();
            // SAFETY: the pseudo console was created here and nothing else owns it.
            unsafe { ClosePseudoConsole(console) };
            return Err(error);
        }
        // SAFETY: CreateProcessW returned a fresh thread handle; the process
        // handle is owned by `Child`.
        unsafe {
            CloseHandle(process.hThread);
        }
        let console = Console {
            console: Arc::new(Mutex::new(Some(console))),
        };
        // SAFETY: as above; `Child` closes the process handle once.
        let handle = unsafe { OwnedHandle::from_raw_handle(process.hProcess as RawHandle) };
        // ConPTY keeps its output open after the client exits. Closing the
        // pseudo console then flushes the last output and ends the pipe, so
        // the reader sees the end of the output and reports the exit.
        let (exited, shared) = (handle.try_clone()?, console.console.clone());
        thread::Builder::new()
            .name("axo-pty-wait".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                // SAFETY: the duplicated process handle is owned by this thread.
                unsafe { WaitForSingleObject(exited.as_raw_handle() as HANDLE, INFINITE) };
                close(&shared);
            })?;
        Ok(Self {
            reader,
            writer,
            child: Child {
                process: Some(handle),
                id: process.dwProcessId,
            },
            console,
        })
    }
}

/// ConPTY keeps no inspectable working directory, so the shell's own report
/// (OSC 7) is the only source.
pub fn process_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

/// ConPTY has no foreground process-group query.
pub fn process_name(_pid: u32) -> Option<String> {
    None
}
