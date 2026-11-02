//! A real shell on ConPTY: it starts, its output
//! reaches the grid, and its exit is reported.

use std::{sync::mpsc, time::Duration};

use axo_term::{Event, Options, Session, Term, WinSize};

const SIZE: WinSize = WinSize {
    cols: 80,
    rows: 24,
    width: 0,
    height: 0,
};

fn screen(term: &Term) -> String {
    let mut out = String::new();
    for y in 0..term.rows() {
        term.grid().row(y).text(0, usize::MAX, &mut out);
        out.push('\n');
    }
    out
}

/// Runs `shell` to completion and returns what it left on the screen.
fn run(shell: &str) -> String {
    let (tx, rx) = mpsc::channel();
    let session = Session::spawn(
        &Options {
            shell,
            cwd: None,
            scrollback: 100,
        },
        SIZE,
        move |event| {
            let _ = tx.send(event);
        },
    )
    .unwrap();
    loop {
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(Event::Exit) => break,
            Ok(Event::Wakeup) => {
                session.take_dirty();
            }
            Err(_) => panic!("{shell}: no exit; screen:\n{}", screen(&session.term())),
        }
    }
    screen(&session.term())
}

#[cfg(windows)]
#[test]
fn cmd_and_powershell_start_on_conpty() {
    let cmd = run("cmd.exe /c echo axo-cmd-%TERM%");
    assert!(cmd.contains("axo-cmd-xterm-256color"), "{cmd}");
    let powershell = run("powershell.exe -NoProfile -Command Write-Output axo-ps-$env:COLORTERM");
    assert!(powershell.contains("axo-ps-truecolor"), "{powershell}");
}
