//! A terminal receipt whose states are driven by the verifier's actual checks.
//! Wide color terminals redraw a fixed evidence board; other terminals keep
//! stable rows. Colors and group states advance only on real verifier events.

use std::io::{IsTerminal, Write};
use std::time::Instant;
use anyhow::Result;
use vcontainer_core::{Evidence, State};
use crate::{state::Policy, verify::{self, Check, Progress, Verified}};

fn columns() -> usize {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
        usize::from(ws.ws_col)
    } else { 80 }
}

fn tty() -> bool {
    std::io::stderr().is_terminal() && std::env::var("TERM").map_or(true, |t| t != "dumb")
}
fn paint(text: &str, sgr: &str) -> String {
    if tty() && columns() >= 40 && std::env::var_os("NO_COLOR").is_none() { format!("\x1b[{sgr}m{text}\x1b[0m") }
    else { text.into() }
}

fn label(check: Check) -> &'static str {
    match check {
        Check::Intel => "Intel TDX",
        Check::Session => "Fresh session",
        Check::Platform => "Google Cloud",
        Check::Boot => "Measured boot",
        Check::Ledger => "Owner + ledger",
        Check::Launch => "Signed launch",
        Check::Container => "Image manifest",
    }
}
fn group(check: Check) -> &'static str {
    match check {
        Check::Intel | Check::Session | Check::Platform => "MACHINE  /  identity & freshness",
        Check::Boot | Check::Ledger => "BOOT  /  your trusted environment",
        Check::Launch | Check::Container => "CONTAINER  /  your requested workload",
    }
}

// Terminal-origin data is sanitized before presentation. The values displayed
// here (digests, IDs and verifier messages) are ASCII; non-ASCII words are
// conservatively counted as two cells so they cannot push a row into wrapping.
fn cells(s: &str) -> usize { s.chars().map(|c| if c.is_ascii() || matches!(c, '✓' | '×') { 1 } else { 2 }).sum() }
fn wrapped(text: &str, width: usize) -> Vec<String> {
    let text = verify::untrusted(text.as_bytes());
    let width = width.max(1);
    let mut lines = vec![String::new()];
    for word in text.split_whitespace() {
        if !lines.last().unwrap().is_empty() {
            if cells(lines.last().unwrap()) + 1 + cells(word) <= width {
                lines.last_mut().unwrap().push(' ');
            } else { lines.push(String::new()); }
        }
        for ch in word.chars() {
            let size = if ch.is_ascii() { 1 } else { 2 };
            if cells(lines.last().unwrap()) + size > width { lines.push(String::new()); }
            lines.last_mut().unwrap().push(ch);
        }
    }
    lines
}
fn row(mark: &str, name: &str, text: &str, color: &str) {
    let width = columns().saturating_sub(2);
    if width < 58 {
        eprintln!("  {} {}", paint(mark, color), paint(name, "1"));
        for line in wrapped(text, width.saturating_sub(4)) { eprintln!("    {line}"); }
    } else {
        let prefix = format!("  {mark} {name:<16} ");
        let indent = " ".repeat(cells(&prefix));
        for (i, line) in wrapped(text, width.saturating_sub(cells(&prefix))).iter().enumerate() {
            if i == 0 { eprintln!("  {} {:<16} {line}", paint(mark, color), name); }
            else { eprintln!("{indent}{line}"); }
        }
    }
}


const BOARD_WIDTH: usize = 78;
const BOARD_HEIGHT: usize = 13;
const CHECKS: [Check; 7] = [Check::Intel, Check::Session, Check::Platform, Check::Boot, Check::Ledger, Check::Launch, Check::Container];

#[derive(Clone, Debug)]
enum CheckState {
    Pending,
    Checking,
    Passed(String),
    Skipped(String),
    Failed(String),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupState { Pending, Checking, Verified, Failed, NotLaunched }
impl GroupState {
    fn text(self) -> &'static str {
        match self {
            Self::Pending => "Pending", Self::Checking => "Checking",
            Self::Verified => "Verified", Self::Failed => "Failed",
            Self::NotLaunched => "Not launched",
        }
    }
    fn color(self) -> &'static str {
        match self {
            Self::Pending | Self::NotLaunched => "2;37",
            Self::Checking => "1;36", Self::Verified => "1;32", Self::Failed => "1;31",
        }
    }
}

fn check_index(check: Check) -> usize { CHECKS.iter().position(|value| *value == check).unwrap() }
fn board_group(rows: &[CheckState], container: bool) -> GroupState {
    if rows.iter().any(|row| matches!(row, CheckState::Failed(_))) { GroupState::Failed }
    else if rows.iter().any(|row| matches!(row, CheckState::Checking)) { GroupState::Checking }
    else if rows.iter().all(|row| matches!(row, CheckState::Passed(_))) { GroupState::Verified }
    else if container && rows.iter().all(|row| matches!(row, CheckState::Passed(_) | CheckState::Skipped(_))) {
        GroupState::NotLaunched
    } else if rows.iter().any(|row| matches!(row, CheckState::Skipped(_))) { GroupState::Pending }
    else if rows.iter().any(|row| matches!(row, CheckState::Passed(_))) { GroupState::Checking }
    else { GroupState::Pending }
}

/// The live board uses ASCII summaries so every frame has the same cell count.
/// Final receipt fields retain complete original values.
fn preview_text(text: &str, width: usize) -> String {
    let safe: String = verify::untrusted(text.as_bytes()).chars()
        .map(|ch| if ch.is_ascii() { ch } else { '?' }).collect();
    if safe.len() <= width { safe }
    else if width > 3 { format!("{}...", &safe[..width - 3]) }
    else { safe[..width].to_string() }
}
fn style(text: &str, sgr: &str, color: bool) -> String {
    if color { format!("\x1b[{sgr}m{text}\x1b[0m") } else { text.to_string() }
}

struct Board {
    rows: [CheckState; 7],
    shown: bool,
    live: bool,
}
impl Board {
    fn new() -> Self { Self { rows: std::array::from_fn(|_| CheckState::Pending), shown: false, live: true } }
    fn update(&mut self, event: &Progress) {
        match event {
            Progress::Checking(check) => self.rows[check_index(*check)] = CheckState::Checking,
            Progress::Passed(check, text) => self.rows[check_index(*check)] = CheckState::Passed(text.clone()),
            Progress::Skipped(check, text) => self.rows[check_index(*check)] = CheckState::Skipped(text.clone()),
        }
    }
    fn frame(&self, color: bool) -> Vec<String> {
        let groups = [board_group(&self.rows[..3], false), board_group(&self.rows[3..5], false), board_group(&self.rows[5..], true)];
        let titles = ["MACHINE", "BOOT", "CONTAINER"];
        let sources = ["Intel + Google", "your release", "signed launch"];
        let mut lines = Vec::with_capacity(BOARD_HEIGHT);
        for row in 0..5 {
            let mut line = "  ".to_string();
            for index in 0..3 {
                let cell = match row {
                    0 | 4 => format!("+{}+", "-".repeat(20)),
                    1 => format!("|{:^20}|", titles[index]),
                    2 => format!("|{:^20}|", sources[index]),
                    _ => format!("|{:^20}|", groups[index].text()),
                };
                line.push_str(&style(&cell, groups[index].color(), color));
                if index < 2 { line.push_str(&style(if row == 2 { " --> " } else { "     " }, "2;36", color)); }
            }
            lines.push(line);
        }
        lines.push(" ".repeat(BOARD_WIDTH));
        for (check, state) in CHECKS.iter().zip(&self.rows) {
            let (mark, summary, color_code) = match state {
                CheckState::Pending => (".", "pending", "2;37"),
                CheckState::Checking => (">", "checking evidence", "1;36"),
                CheckState::Passed(text) => ("✓", text.as_str(), "32"),
                CheckState::Skipped(text) => ("-", text.as_str(), "2;37"),
                CheckState::Failed(text) => ("×", text.as_str(), "1;31"),
            };
            let prefix = format!("  {mark} {:<16}  ", label(*check));
            let available = BOARD_WIDTH - cells(&prefix);
            let summary = preview_text(summary, available);
            let line = format!("{prefix}{summary}{}", " ".repeat(available - summary.len()));
            lines.push(style(&line, color_code, color));
        }
        lines
    }
    fn render(&mut self) -> bool {
        if !self.live || !board_supported() {
            // A resize can reflow the old region. Relinquish it permanently
            // rather than moving up by an invalid number of physical rows.
            self.live = false;
            return false;
        }
        let mut buffer = String::new();
        if self.shown { buffer.push_str(&format!("\x1b[{BOARD_HEIGHT}A")); }
        for line in self.frame(true) {
            buffer.push('\r');
            buffer.push_str(&line);
            buffer.push_str("\x1b[K\n");
        }
        let mut stderr = std::io::stderr().lock();
        if stderr.write_all(buffer.as_bytes()).and_then(|_| stderr.flush()).is_err() {
            self.live = false;
            return false;
        }
        self.shown = true;
        true
    }
}
fn board_supported() -> bool {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let rows = if unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_row > 0 {
        usize::from(ws.ws_row)
    } else { 24 };
    tty() && columns() >= 80 && rows > BOARD_HEIGHT && std::env::var_os("NO_COLOR").is_none()
}

/// Connection coordinates are presentation data, not evidence fingerprints.
fn connection_label(text: &str) -> String {
    let text = verify::untrusted(text.as_bytes());
    let mut result = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch.is_ascii_digit() {
            let mut token = ch.to_string();
            while chars.peek().is_some_and(|c| c.is_ascii_digit() || *c == '.') { token.push(chars.next().unwrap()); }
            if token.parse::<std::net::Ipv4Addr>().is_ok() { result.push_str("[redacted IP]"); }
            else { result.push_str(&token); }
        } else { result.push(ch); }
    }
    result
}

pub fn verify(label_text: &str, ev: &Evidence, nonce: &[u8; 32], policy: &Policy) -> Result<Verified> {
    if !tty() { return verify::verify(ev, nonce, policy); }
    eprintln!("\n  {}", paint("vcontainer / verify", "1"));
    for line in wrapped(&connection_label(label_text), columns().saturating_sub(6)) { eprintln!("  {line}"); }
    if board_supported() { verify_board(ev, nonce, policy) } else { verify_stable(ev, nonce, policy) }
}

fn verify_board(ev: &Evidence, nonce: &[u8; 32], policy: &Policy) -> Result<Verified> {
    let start = Instant::now();
    let mut board = Board::new();
    board.render();
    let mut current = None;
    let mut count = 0;
    let mut progress = |event: Progress| {
        let passed = matches!(&event, Progress::Passed(_, _));
        match &event {
            Progress::Checking(check) => current = Some(*check),
            Progress::Passed(_, _) => { count += 1; current = None; }
            Progress::Skipped(_, _) => current = None,
        }
        board.update(&event);
        let rendered = board.render();
        if !rendered {
            match &event {
                Progress::Passed(check, detail) => row("✓", label(*check), detail, "32"),
                Progress::Skipped(check, detail) => row("-", label(*check), detail, "2"),
                Progress::Checking(_) => {}
            }
        }
        // A short read hold only after a check really passed. No timer can
        // turn an unchecked item green, and stable paths never sleep.
        if passed && rendered { std::thread::sleep(std::time::Duration::from_millis(100)); }
    };
    let result = verify::verify_with_progress(ev, nonce, policy, &mut progress);
    drop(progress);
    match &result {
        Ok(_) => eprintln!("\n  {}  {count} checks passed / {:.2}s", paint("CHECKS PASSED", "1;32"), start.elapsed().as_secs_f32()),
        Err(_) => {
            if let Some(check) = current {
                let reason = failure_reason(check);
                board.rows[check_index(check)] = CheckState::Failed(reason.into());
                if !board.render() { row("×", label(check), reason, "1;31"); }
            }
            eprintln!("\n  {}  No successful verification result.", paint("BLOCKED", "1;31"));
        }
    }
    result.map_err(|error| match current {
        Some(check) => error.context(format!("{} check failed", label(check))),
        None => error,
    })
}

fn failure_reason(check: Check) -> &'static str {
    match check {
        Check::Intel => "FAILED - quote signature, collateral or TCB policy rejected",
        Check::Session => "FAILED - quote is not bound to our nonce and status",
        Check::Platform => "FAILED - machine identity or platform policy rejected",
        Check::Boot => "FAILED - measured boot does not match your release policy",
        Check::Ledger => "FAILED - recorded ledger or owner does not match",
        Check::Launch => "FAILED - recorded launch does not match your request",
        Check::Container => "FAILED - container manifest check rejected",
    }
}

fn verify_stable(ev: &Evidence, nonce: &[u8; 32], policy: &Policy) -> Result<Verified> {
    let start = Instant::now();
    let mut current = None;
    let mut previous_group = "";
    let mut count = 0;
    let mut active = false;
    let mut progress = |event| {
        match event {
            Progress::Checking(check) => {
                current = Some(check);
                if group(check) != previous_group {
                    eprintln!("\n  {}", paint(group(check), "1"));
                    previous_group = group(check);
                }
                if tty() && columns() >= 40 && std::env::var_os("NO_COLOR").is_none() {
                    let message = format!("  . {}  checking evidence", label(check));
                    eprint!("{}", paint(&wrapped(&message, columns().saturating_sub(2))[0], "2"));
                    let _ = std::io::stderr().flush();
                    active = true;
                }
            }
            Progress::Passed(check, detail) => {
                if active { eprint!("\r\x1b[2K"); active = false; }
                count += 1;
                row("✓", label(check), &detail, "32");
                current = None;
            }
            Progress::Skipped(check, detail) => {
                if active { eprint!("\r\x1b[2K"); active = false; }
                if group(check) != previous_group {
                    eprintln!("\n  {}", paint(group(check), "1"));
                    previous_group = group(check);
                }
                row("-", label(check), &detail, "2");
                current = None;
            }
        }
    };
    let result = verify::verify_with_progress(ev, nonce, policy, &mut progress);
    if active { eprint!("\r\x1b[2K"); }
    match &result {
        Ok(_) => eprintln!("\n  {}  {count} checks passed / {:.2}s", paint("CHECKS PASSED", "1;32"), start.elapsed().as_secs_f32()),
        Err(_) => {
            if let Some(check) = current {
                let reason = failure_reason(check);
                row("×", label(check), reason, "1;31");
            }
            eprintln!("\n  {}  No successful verification result.", paint("BLOCKED", "1;31"));
        }
    }
    result.map_err(|error| match current {
        Some(check) => error.context(format!("{} check failed", label(check))),
        None => error,
    })
}

fn field(name: &str, value: &str, width: usize) {
    if width < 40 {
        for line in wrapped(name, width.max(1)) { eprintln!("{line}"); }
        for line in wrapped(value, width.max(1)) { eprintln!("{line}"); }
        return;
    }
    let prefix = format!("  {name:<10} ");
    let available = width.saturating_sub(prefix.len()).max(8);
    for (i, line) in wrapped(value, available).iter().enumerate() {
        if i == 0 { eprintln!("{prefix}{line}"); }
        else { eprintln!("{}{line}", " ".repeat(prefix.len())); }
    }
}
fn fingerprint(name: &str, value: &str, width: usize) {
    if width < 40 { field(name, value, width); return; }
    let chunk = width.saturating_sub(14).min(48).max(8);
    for (i, piece) in value.as_bytes().chunks(chunk).enumerate() {
        let piece = std::str::from_utf8(piece).expect("hex fingerprint");
        eprintln!("  {:<10} {}", if i == 0 { name } else { "" }, paint(piece, "36"));
    }
}

/// Compact final evidence, with complete RTMR1/index/manifest values. Longer
/// release metadata remains available with --details and in exported policy.
pub fn receipt(v: &Verified, policy: &Policy, details: bool) {
    let width = columns().saturating_sub(2);
    let accepted = v.boot_measurement.accepted_rtmr1.iter()
        .find(|value| **value == v.boot_measurement.attested_rtmr1);
    eprintln!("\n  {}", paint("BOOT MEASUREMENT  /  all 48 bytes match", "1"));
    field("release", &policy.release.image, width);
    if let Some(expected) = accepted {
        fingerprint("policy", expected, width);
        fingerprint("quote", &v.boot_measurement.attested_rtmr1, width);
    }
    if let Some(config) = &v.config {
        eprintln!("\n  {}", paint("RECORDED CONTAINER", "1"));
        if let Some((image, digest)) = config.image.rsplit_once('@') {
            field("image", image, width);
            field("pinned", digest, width);
        } else { field("image", &config.image, width); }
        if let Some(claim) = v.claims.iter().find_map(|c| c.strip_prefix("container sealed: manifest ")) {
            field("manifest", claim, width);
        }
        field("config", &format!("env={:?} secrets={:?}", config.env, config.secrets), width);
        if policy.launch.is_none() { field("policy", "Launch is not pinned; this is the recorded configuration.", width); }
    }
    let result = match (v.status.state, v.status.exit_code) {
        (State::Waiting, _) => "Waiting - verified VM is ready for a launch".to_string(),
        (State::Exited, Some(code)) => format!("Exited - exit code {code}"),
        (state, _) => format!("{state:?}"),
    };
    eprintln!("\n  {}", paint("ATTESTATION VERIFIED", "1;32"));
    let state_color = if matches!(v.status.state, State::Failed) || v.status.exit_code.is_some_and(|c| c != 0) { "1;31" } else { "1;32" };
    for line in wrapped(&format!("state: {result}"), width.saturating_sub(2)) { eprintln!("  {}", paint(&line, state_color)); }
    if let Some(error) = &v.status.error { field("error", error, width); }
    if details {
        eprintln!("\n  {}", paint("FULL VERIFICATION DETAILS", "1"));
        for claim in &v.claims { for line in wrapped(claim, width.saturating_sub(4)) { eprintln!("    {line}"); } }
        field("project", &policy.release.project, width);
        field("PE SHA384", &policy.release.pe_authenticode_sha384, width);
        field("GPT SHA384", &policy.release.gpt_event_sha384, width);
        if let Some(commit) = &policy.release.commit { field("commit", commit, width); }
    }
    eprintln!();
}

pub fn gate(message: &str) {
    eprintln!("\n  {}", paint("VERIFY -> OWNER COMMIT -> PULL & RUN", "1"));
    for line in wrapped(message, columns().saturating_sub(6)) { eprintln!("  {line}"); }
    eprintln!();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evidence_lines_preserve_values_and_fit() {
        let hash = "0123456789abcdef".repeat(6);
        for width in [8, 20, 48, 64, 80, 100] {
            let lines = wrapped(&hash, width);
            assert!(lines.iter().all(|line| cells(line) <= width));
            assert_eq!(lines.concat(), hash);
        }
    }
    #[test]
    fn network_text_cannot_inject_terminal_controls() {
        let lines = wrapped("image\x1b[2J\n\u{202e}bad", 30);
        assert!(lines.iter().all(|line| !line.chars().any(|c| c.is_control() || c == '\u{202e}')));
    }

    #[test]
    fn structured_frames_are_fixed_height_and_safe_at_80_columns() {
        let mut board = Board::new();
        let states = [CheckState::Pending, CheckState::Checking,
            CheckState::Passed("long actual evidence detail ".repeat(8)),
            CheckState::Skipped("no launch recorded".into()),
            CheckState::Failed("a rejected policy".into())];
        for state in states {
            for row in &mut board.rows { *row = state.clone(); }
            let frame = board.frame(false);
            assert_eq!(frame.len(), BOARD_HEIGHT);
            assert!(frame.iter().all(|line| cells(line) == BOARD_WIDTH));
            assert!(frame.iter().all(|line| !line.chars().any(char::is_control)));
            assert!(frame.iter().all(|line| cells(line) < 80));
        }
    }
    #[test]
    fn evidence_groups_never_verify_skipped_or_unchecked_rows() {
        let mut board = Board::new();
        assert_eq!(board_group(&board.rows[..3], false), GroupState::Pending);
        board.update(&Progress::Checking(Check::Intel));
        assert_eq!(board_group(&board.rows[..3], false), GroupState::Checking);
        for check in [Check::Intel, Check::Session, Check::Platform] {
            board.update(&Progress::Passed(check, "passed".into()));
        }
        assert_eq!(board_group(&board.rows[..3], false), GroupState::Verified);
        board.update(&Progress::Skipped(Check::Launch, "no launch".into()));
        assert_eq!(board_group(&board.rows[5..], true), GroupState::Pending);
        board.update(&Progress::Skipped(Check::Container, "no exec".into()));
        assert_eq!(board_group(&board.rows[5..], true), GroupState::NotLaunched);
        board.rows[3] = CheckState::Failed("bad boot".into());
        assert_eq!(board_group(&board.rows[3..5], false), GroupState::Failed);
    }
    #[test]
    fn connection_coordinates_are_redacted_without_changing_other_labels() {
        assert_eq!(connection_label("demo / 34.44.123.4 / us-central1-a"), "demo / [redacted IP] / us-central1-a");
        assert_eq!(connection_label("http://34.44.123.4:7700"), "http://[redacted IP]:7700");
        assert_eq!(connection_label("release vcontainer-24e25d8234a7"), "release vcontainer-24e25d8234a7");
    }
}
