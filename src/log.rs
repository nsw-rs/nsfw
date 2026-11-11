// SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Level-tagged console output. Quiet mode hides verbose messages.
//!
//! Colors use 256-color codes and are emitted only when stdout is a color
//! terminal (`NO_COLOR` turns them off, `CLICOLOR_FORCE` forces them on).
//!
//! Everything goes out through [`line`] or [`part`], which make control
//! characters visible: file names, titles and TitleDB text come from
//! outside, and must not move the cursor, clear a line, set the window
//! title or start a line of their own.

use std::borrow::Cow;
use std::fmt::{Display, Write as _};
use std::io::Write as _;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

static QUIET: AtomicBool = AtomicBool::new(false);

pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

/// Progress bars are separate from quiet: batch mode hides verbose lines but
/// still shows progress unless `-q` was given.
static PROGRESS: AtomicBool = AtomicBool::new(true);

pub fn set_progress(show: bool) {
    PROGRESS.store(show, Ordering::Relaxed);
}

pub fn show_progress() -> bool {
    PROGRESS.load(Ordering::Relaxed)
}

const OLIVE: u8 = 3;
const GREY: u8 = 8;
const GREEN: u8 = 2;
const RED: u8 = 9;
const BLUE: u8 = 12;
const SKY: u8 = 39;
const YELLOW: u8 = 11;
const MAROON: u8 = 1;

const PALETTE: [u8; 8] = [OLIVE, GREY, GREEN, RED, BLUE, SKY, YELLOW, MAROON];
const RESET: &str = "\x1b[0m";

fn color(code: u8) -> String {
    format!("\x1b[38;5;{code}m")
}

fn paint(code: u8, s: impl Display) -> String {
    if console::colors_enabled() {
        format!("{}{s}{RESET}", color(code))
    } else {
        s.to_string()
    }
}

/// Length of the color sequence of [`paint`] that `s` starts with, if any.
fn own_escape(s: &str) -> Option<usize> {
    if s.starts_with(RESET) {
        return Some(RESET.len());
    }
    // Built once: this runs for every escape of every painted line.
    static COLORS: LazyLock<[String; 8]> = LazyLock::new(|| PALETTE.map(color));
    COLORS
        .iter()
        .find(|c| s.starts_with(c.as_str()))
        .map(|c| c.len())
}

/// A tab only moves to the right, and TitleDB descriptions have them.
fn is_unsafe(c: char) -> bool {
    c.is_control() && c != '\t'
}

/// `s` with its control characters written out (`\u{1b}`, `\n`). With
/// `colors` the sequences of [`paint`] are kept, and nothing else is: text
/// from outside can at most take one of those colors.
fn sanitize(s: &str, colors: bool) -> Cow<'_, str> {
    if !s.contains(is_unsafe) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 16);
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        let own = if colors && c == '\x1b' {
            own_escape(rest)
        } else {
            None
        };
        let n = match own {
            Some(n) => {
                out.push_str(&rest[..n]);
                n
            }
            None if is_unsafe(c) => {
                let _ = write!(out, "{}", c.escape_default());
                c.len_utf8()
            }
            None => {
                out.push(c);
                c.len_utf8()
            }
        };
        rest = &rest[n..];
    }
    Cow::Owned(out)
}

fn write_out(s: &str, end: &str) {
    let mut out = std::io::stdout().lock();
    let s = sanitize(s, console::colors_enabled());
    // Nobody reads the verdicts once stdout is closed (`nsfw v dir | head`),
    // and a run that goes on in silence could still convert and delete.
    if let Err(e) = write!(out, "{s}{end}").and_then(|()| out.flush()) {
        // A full disk under `nsfw v dir > log` is worth a word. With
        // `2>&1` stderr fails the same way, and `eprintln!` would panic.
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            let _ = writeln!(std::io::stderr(), "Error: cannot write the output: {e}");
        }
        crate::stop::exit(1);
    }
}

/// `s` for a message on stderr: control characters written out, no colors.
pub fn plain(s: &str) -> Cow<'_, str> {
    sanitize(s, false)
}

/// Print one line on stdout.
pub fn line(s: &str) {
    write_out(s, "\n");
}

/// Print the start of a line on stdout, which [`line`] ends.
pub fn part(s: &str) {
    write_out(s, "");
}

pub fn olive(s: impl Display) -> String {
    paint(OLIVE, s)
}

pub fn grey(s: impl Display) -> String {
    paint(GREY, s)
}

pub fn green(s: impl Display) -> String {
    paint(GREEN, s)
}

pub fn red(s: impl Display) -> String {
    paint(RED, s)
}

pub fn blue(s: impl Display) -> String {
    paint(BLUE, s)
}

/// Green status tag in brackets, e.g. `[DONE]`.
pub fn tag(s: &str) -> String {
    format!("[{}]", green(s))
}

/// Grey detail in parentheses, e.g. `(file.tik)`.
pub fn paren(s: impl Display) -> String {
    grey(format_args!("({s})"))
}

pub fn emit(level: &str, msg: &str) {
    if level == "VRB" && is_quiet() {
        return;
    }
    let code = match level {
        "VRB" => GREY,
        "INF" => SKY,
        "WRN" => YELLOW,
        "ERR" => RED,
        _ => MAROON,
    };
    line(&format!("[{}] {msg}", paint(code, level)));
}

#[macro_export]
macro_rules! vrb { ($($a:tt)*) => { if !$crate::log::is_quiet() { $crate::log::emit("VRB", &format!($($a)*)) } } }
#[macro_export]
macro_rules! inf { ($($a:tt)*) => { $crate::log::emit("INF", &format!($($a)*)) } }
#[macro_export]
macro_rules! wrn { ($($a:tt)*) => { $crate::log::emit("WRN", &format!($($a)*)) } }
#[macro_export]
macro_rules! err { ($($a:tt)*) => { $crate::log::emit("ERR", &format!($($a)*)) } }
#[macro_export]
macro_rules! ftl { ($($a:tt)*) => { $crate::log::emit("FTL", &format!($($a)*)) } }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_written_out() {
        assert_eq!(sanitize("plain\t[01] \u{e9}", true), "plain\t[01] \u{e9}");
        assert_eq!(sanitize("a\tb\x07", true), "a\tb\\u{7}");
        // Clear the line, cursor up, window title, a forged second line.
        assert_eq!(
            sanitize("a\x1b[2K\x1b[1A\x1b]0;t\x07b\r\n[INF] ok\u{9b}", true),
            "a\\u{1b}[2K\\u{1b}[1A\\u{1b}]0;t\\u{7}b\\r\\n[INF] ok\\u{9b}"
        );
    }

    #[test]
    fn only_own_colors_pass() {
        let painted = format!("{}Yes{RESET} x", color(GREEN));
        assert_eq!(sanitize(&painted, true), painted);
        // Without colors nothing was painted, so every sequence is foreign.
        assert_eq!(sanitize(&painted, false), "\\u{1b}[38;5;2mYes\\u{1b}[0m x");
        // Not in the palette: black, conceal, another kind of sequence.
        assert_eq!(sanitize("\x1b[38;5;0m", true), "\\u{1b}[38;5;0m");
        assert_eq!(sanitize("\x1b[8m", true), "\\u{1b}[8m");
        assert_eq!(sanitize("\x1b[38;5;2;1m", true), "\\u{1b}[38;5;2;1m");
    }
}
