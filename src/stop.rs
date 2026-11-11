// SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ending the run before its time: on Ctrl-C or a termination signal, and
//! when stdout is closed. The output that is being written is deleted first.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

static PARTIAL: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The partial output of the conversion in progress, which an early end
/// deletes. It is held from the creation of the file until it is set here,
/// and over the rename that completes it: an early end then never falls in
/// between.
fn partial() -> MutexGuard<'static, Option<PathBuf>> {
    PARTIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// An output being written under a temporary name. An early end deletes
/// it, and so does dropping this before [`PartFile::rename`].
pub struct PartFile(Option<PathBuf>);

impl PartFile {
    /// Open `path` with `options`, which are expected to create it.
    pub fn open(path: PathBuf, options: &OpenOptions) -> std::io::Result<(Self, File)> {
        let mut partial = partial();
        let file = options.open(&path)?;
        *partial = Some(path.clone());
        Ok((PartFile(Some(path)), file))
    }

    /// Give the file its final name, `to`.
    pub fn rename(mut self, to: &Path) -> std::io::Result<()> {
        let mut partial = partial();
        let result = self.0.as_ref().map_or(Ok(()), |p| std::fs::rename(p, to));
        if result.is_ok() {
            self.0 = None;
            *partial = None;
        }
        // Released before a failed rename drops `self`, which deletes.
        drop(partial);
        result
    }
}

impl Drop for PartFile {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let mut partial = partial();
            let _ = std::fs::remove_file(p);
            *partial = None;
        }
    }
}

/// Delete the partial output and end the process.
pub fn exit(code: i32) -> ! {
    // Kept locked until the process is gone, so that the conversion cannot
    // go on with a file that was just deleted.
    let mut partial = partial();
    if let Some(p) = partial.take() {
        let _ = std::fs::remove_file(p);
    }
    std::process::exit(code)
}

/// From here on Ctrl-C, SIGTERM and SIGHUP end the run through [`exit`].
pub fn on_signal() {
    // 130 is what a shell reports for a process ended by Ctrl-C.
    if let Err(e) = ctrlc::set_handler(|| exit(130)) {
        eprintln!("Warning: an interrupted conversion will leave its partial file: {e}");
    }
}
