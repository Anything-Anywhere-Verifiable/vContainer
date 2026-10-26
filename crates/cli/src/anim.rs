//! A quiet single-line activity indicator. Only one ASCII mark and the elapsed
//! time change; the phase and purpose stay fixed. Non-TTY and dumb terminals
//! get stable result lines only; NO_COLOR disables styling.

use anyhow::Result;
use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind { Verify, Image, Sign, Container, Wait }

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Verify => "VERIFY", Self::Image => "IMAGE",
            Self::Sign => "SIGN", Self::Container => "CONTAINER", Self::Wait => "CLOUD",
        }
    }
}

/// Stop before the scope joins its worker, including during panic unwinding.
struct StopWorker<'a> { stop: &'a AtomicBool, worker: Option<std::thread::Thread> }
impl Drop for StopWorker<'_> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = &self.worker { worker.unpark(); }
    }
}

/// `f` must not print to stderr: the indicator owns only the current line.
pub fn step<T>(kind: Kind, msg: &str, done: impl FnOnce(&T) -> String, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let start = Instant::now();
    let term = std::io::stderr().is_terminal() && std::env::var("TERM").map_or(true, |t| t != "dumb") && std::env::var_os("NO_COLOR").is_none();
    let color = term && std::env::var_os("NO_COLOR").is_none();
    let stop = AtomicBool::new(false);
    let completed = AtomicBool::new(false);
    let result = std::thread::scope(|scope| {
        let worker = term.then(|| scope.spawn(|| {
            let mut shown = false;
            let mut tick = 0;
            while !stop.load(Ordering::Acquire) {
                if let Some(line) = frame(kind, msg, tick, start.elapsed(), terminal_columns(), color) {
                    // One write overwrites the line; keep the cursor at column
                    // zero. No screen clearing or hidden-cursor mode to undo.
                    if !write_status(&format!("\r{line}\x1b[K\r")) { break; }
                    shown = true;
                } else if shown {
                    write_status("\r\x1b[2K");
                    shown = false;
                }
                tick += 1;
                std::thread::park_timeout(Duration::from_millis(250));
            }
            if shown {
                if completed.load(Ordering::Acquire) { write_status("\r\x1b[2K"); }
                // A panic hook may have printed a diagnostic; don't erase it.
                else { write_status("\r\n"); }
            }
        }));
        let _stop = StopWorker { stop: &stop, worker: worker.as_ref().map(|w| w.thread().clone()) };
        let result = f();
        completed.store(true, Ordering::Release);
        result
    });
    if let Ok(value) = &result { eprintln!("✓ {} ({:.1}s)", done(value), start.elapsed().as_secs_f32()); }
    result
}

fn write_status(text: &str) -> bool {
    let mut stderr = std::io::stderr().lock();
    stderr.write_all(text.as_bytes()).and_then(|_| stderr.flush()).is_ok()
}

/// ASCII cells give predictable widths, including for unusual filenames or
/// terminal controls. The permanent result retains the original full text.
fn fit_ascii(message: &str, columns: usize) -> String {
    let clean: String = message.chars().map(|c| {
        if c.is_ascii_graphic() || c == ' ' { c }
        else if c.is_control() { ' ' } else { '?' }
    }).collect();
    if clean.len() <= columns { clean }
    else if columns > 3 { format!("{}...", &clean[..columns - 3]) }
    else { clean[..columns].to_string() }
}

/// Leave two columns unused to avoid right-margin automatic wrapping.
fn frame(kind: Kind, message: &str, tick: usize, elapsed: Duration, columns: usize, color: bool) -> Option<String> {
    if columns < 40 { return None; }
    let mark = [b'.', b'o', b'o', b'.'][tick % 4] as char;
    let prefix = format!("  {mark} {:<9}  ", kind.label());
    let timer = format!("  {:>4}s", elapsed.as_secs());
    let available = columns.checked_sub(2 + prefix.len() + timer.len())?;
    let message = format!("{:<available$}", fit_ascii(message, available));
    if color {
        Some(format!("\x1b[2m  {mark}\x1b[0m \x1b[1m{:<9}\x1b[0m  {message}\x1b[2m{timer}\x1b[0m", kind.label()))
    } else { Some(format!("{prefix}{message}{timer}")) }
}

fn terminal_columns() -> usize {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 { ws.ws_col as usize }
    else { 80 }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn narrow_and_changing_widths_never_wrap() {
        for columns in 0..200 {
            for seconds in [0, 9, 10, 99, 100, u64::MAX] {
                if let Some(line) = frame(Kind::Container, &"long activity message ".repeat(10), 15, Duration::from_secs(seconds), columns, false) {
                    assert!(line.is_ascii() && line.len() <= columns - 2, "{columns}: {line:?}");
                    assert!(line.chars().all(|c| !c.is_control()));
                }
            }
        }
    }
    #[test]
    fn phase_and_timer_positions_stay_fixed() {
        let first = frame(Kind::Verify, "checking evidence", 0, Duration::from_secs(9), 80, false).unwrap();
        for tick in 1..30 {
            let next = frame(Kind::Verify, "checking evidence", tick, Duration::from_secs(10), 80, false).unwrap();
            assert_eq!(first.len(), next.len());
            assert_eq!(first.find("VERIFY"), next.find("VERIFY"));
            assert_eq!(first.find("checking evidence"), next.find("checking evidence"));
        }
    }
    #[test]
    fn transient_text_cannot_inject_controls_or_wide_cells() {
        let text = fit_ascii("测试🙂\n\x1b[31mtext", 80);
        assert!(text.is_ascii() && text.chars().all(|c| !c.is_control()));
        assert_eq!(fit_ascii("message", 0), "");
        assert_eq!(fit_ascii("message", 2), "me");
    }
    #[test]
    fn no_color_frames_have_no_styling() {
        assert!(!frame(Kind::Verify, "check", 3, Duration::ZERO, 80, false).unwrap().contains('\x1b'));
        assert!(frame(Kind::Verify, "check", 3, Duration::ZERO, 80, true).unwrap().contains("\x1b[1m"));
    }
    #[test]
    fn stop_guard_joins_worker_during_unwind() {
        let stop = AtomicBool::new(false);
        let (tx, rx) = std::sync::mpsc::channel();
        let result = std::panic::catch_unwind(|| std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                while !stop.load(Ordering::Acquire) { std::thread::park_timeout(Duration::from_secs(30)); }
                tx.send(()).unwrap();
            });
            let _stop = StopWorker { stop: &stop, worker: Some(worker.thread().clone()) };
            panic!("test operation panicked");
        }));
        assert!(result.is_err());
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
}
