// SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Level-tagged console output. Quiet mode hides verbose messages.
//!
//! Colors use 256-color codes and are emitted only when stdout is a color
//! terminal (`NO_COLOR` turns them off, `CLICOLOR_FORCE` forces them on).

use std::fmt::Display;
use std::sync::atomic::{AtomicBool, Ordering};

static QUIET: AtomicBool = AtomicBool::new(false);

pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

const OLIVE: u8 = 3;
const GREY: u8 = 8;
const GREEN: u8 = 2;
const RED: u8 = 9;
const BLUE: u8 = 12;
const SKY: u8 = 39;
const YELLOW: u8 = 11;
const MAROON: u8 = 1;

fn paint(code: u8, s: impl Display) -> String {
    console::style(s).color256(code).for_stdout().to_string()
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
    println!("[{}] {msg}", paint(code, level));
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
