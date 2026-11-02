// SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Level-tagged console output. Quiet mode hides verbose messages.

use std::sync::atomic::{AtomicBool, Ordering};

static QUIET: AtomicBool = AtomicBool::new(false);

pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

pub fn emit(level: &str, msg: &str) {
    if level == "VRB" && is_quiet() {
        return;
    }
    println!("[{level}] {msg}");
}

#[macro_export]
macro_rules! vrb { ($($a:tt)*) => { $crate::log::emit("VRB", &format!($($a)*)) } }
#[macro_export]
macro_rules! inf { ($($a:tt)*) => { $crate::log::emit("INF", &format!($($a)*)) } }
#[macro_export]
macro_rules! wrn { ($($a:tt)*) => { $crate::log::emit("WRN", &format!($($a)*)) } }
#[macro_export]
macro_rules! err { ($($a:tt)*) => { $crate::log::emit("ERR", &format!($($a)*)) } }
#[macro_export]
macro_rules! ftl { ($($a:tt)*) => { $crate::log::emit("FTL", &format!($($a)*)) } }
