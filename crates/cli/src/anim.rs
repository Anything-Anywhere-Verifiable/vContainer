//! An animated status on stderr while the CLI waits, drawn in plain ASCII.
//! While a session is verified, a padlock's shackle drops and locks; while
//! the boot image is uploaded and created, a file fills with lines; other
//! steps show a one-line spinner. Each step then leaves one `✓` line with its
//! result and duration. The animation runs only on a terminal large enough
//! for it (otherwise the one-line spinner, or nothing off a terminal, where
//! only the result lines are printed).

use anyhow::Result;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Verify,
    Image,
    Sign,
    Container,
    Wait,
}

/// Padlock frames: open, shackle dropping, locked.
const LOCK: [[&str; 6]; 3] = [
    ["   .--.   ", "   |  |   ", "   |      ", " .-'----. ", " |  ()  | ", " '------' "],
    ["          ", "   .--.   ", "   |  |   ", " .-'----. ", " |  ()  | ", " '------' "],
    ["          ", "   .--.   ", "   |  |   ", " .-'--'-. ", " |  ()  | ", " '------' "],
];
/// File frames: lines being written.
const FILE: [[&str; 6]; 4] = [
    ["  _____   ", " |     \\  ", " |      | ", " |      | ", " |      | ", " |______| "],
    ["  _____   ", " |     \\  ", " | ---  | ", " |      | ", " |      | ", " |______| "],
    ["  _____   ", " |     \\  ", " | ---  | ", " | ---- | ", " |      | ", " |______| "],
    ["  _____   ", " |     \\  ", " | ---  | ", " | ---- | ", " | --   | ", " |______| "],
];
const SPIN: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const ART_WIDTH: usize = 10;

/// The drawing at tick `i` (100 ms each), with the row the message goes on.
fn art(kind: Kind, i: usize) -> Option<(&'static [&'static str; 6], usize)> {
    match kind {
        // Open for 0.4 s, dropping for 0.2 s, locked for 0.4 s.
        Kind::Verify => Some((&LOCK[[0, 0, 0, 0, 1, 1, 2, 2, 2, 2][i % 10]], 4)),
        Kind::Image => Some((&FILE[(i / 3) % 4], 3)),
        _ => None,
    }
}

/// Runs `f`, animating `msg` meanwhile; on success prints `done` and the time
/// taken. `f` must not write to stderr.
pub fn step<T>(kind: Kind, msg: &str, done: impl FnOnce(&T) -> String, f: impl FnOnce() -> Result<T>) -> Result<T> {
    const RUNNING: u8 = 0;
    const OK: u8 = 1;
    const FAILED: u8 = 2;
    let start = Instant::now();
    let state = AtomicU8::new(RUNNING);
    let term = std::io::stderr().is_terminal() && std::env::var("TERM").map_or(true, |t| t != "dumb");
    let (cols, rows) = terminal_size();
    // A drawing needs room beside it and above it; else one line.
    let drawn = term && cols >= 40 && rows >= 10;
    let msg = fit(msg, cols.saturating_sub((if drawn { ART_WIDTH + 2 } else { 0 }) + 10));
    let frame = |i: usize, art: Option<(&[&str; 6], usize)>| -> String {
        let status = format!("{} {msg} {}s", SPIN[i % SPIN.len()], start.elapsed().as_secs());
        match art.filter(|_| drawn) {
            Some((lines, row)) => {
                lines.iter().enumerate().map(|(r, l)| if r == row { format!("{l}  {status}") } else { l.to_string() }).collect::<Vec<_>>().join("\n")
            }
            None => status,
        }
    };
    let result = std::thread::scope(|scope| {
        if term {
            scope.spawn(|| {
                let mut height = 0;
                let mut draw = |text: String| {
                    // Back to the first line of the last frame, clear to the end, redraw.
                    let up = if height > 1 { format!("\x1b[{}A", height - 1) } else { String::new() };
                    eprint!("{up}\r\x1b[J{text}");
                    let _ = std::io::stderr().flush();
                    height = text.lines().count();
                };
                for i in 0.. {
                    if state.load(Ordering::Relaxed) != RUNNING {
                        break;
                    }
                    draw(frame(i, art(kind, i)));
                    std::thread::sleep(Duration::from_millis(100));
                }
                if kind == Kind::Verify && state.load(Ordering::Relaxed) == OK && drawn {
                    draw(frame(0, Some((&LOCK[2], 4)))); // show it locked
                    std::thread::sleep(Duration::from_millis(300));
                }
                draw(String::new());
            });
        }
        let r = f();
        state.store(if r.is_ok() { OK } else { FAILED }, Ordering::Relaxed);
        r
    });
    if let Ok(t) = &result {
        eprintln!("✓ {} ({:.1}s)", done(t), start.elapsed().as_secs_f32());
    }
    result
}

/// stderr's terminal size, (80, 24) when unknown.
fn terminal_size() -> (usize, usize) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        (ws.ws_col as usize, ws.ws_row as usize)
    } else {
        (80, 24)
    }
}

/// `msg` cut to at most `max` characters, so no frame line wraps (a wrapped
/// line would break redrawing in place).
fn fit(msg: &str, max: usize) -> String {
    let max = max.max(8);
    if msg.chars().count() <= max { msg.to_string() } else { msg.chars().take(max - 1).chain(['…']).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawings_are_aligned_ascii() {
        for frame in LOCK.iter().chain(FILE.iter()) {
            for line in frame {
                assert!(line.is_ascii() && line.len() == ART_WIDTH, "{line:?}");
            }
        }
    }
}
