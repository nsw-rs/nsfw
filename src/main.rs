// SPDX-FileCopyrightText: 2024 mRg
// SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>
//
// SPDX-License-Identifier: MIT OR Apache-2.0

mod build_titledb;
mod diacritics;
mod log;
mod naming;
mod titledb;
mod validate;

use clap::{Parser, Subcommand};
use naming::LanguageMode;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "nsfw",
    version,
    about = "Nintendo Switch NSP validation and Standard NSP conversion"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate NSP files, optionally converting them to Standard NSP.
    #[command(visible_alias = "v")]
    Validate(ValidateArgs),
    /// Build titledb.db from the blawar/titledb JSON files.
    #[command(name = "build-titledb")]
    BuildTitleDb(BuildTitleDbArgs),
}

#[derive(clap::Args)]
struct BuildTitleDbArgs {
    /// Path to TitleDB jsons directory.
    #[arg(long = "titledbdir", default_value = "./titledb")]
    titledbdir: String,
    /// Clean database before rebuilding.
    #[arg(short = 'c', long = "clean")]
    clean: bool,
}

#[derive(clap::Args)]
struct ValidateArgs {
    /// Path to NSW keys file.
    #[arg(short = 'k', long = "keys", default_value = "~/.switch/prod.keys")]
    keys: String,
    /// Path to 0x700-byte long common certificate chain file (created from the NSP if missing).
    #[arg(short = 'c', long = "cert", default_value = "~/.switch/common.cert")]
    cert: String,
    /// Convert to Standardised NSP.
    #[arg(short = 's', long = "standard")]
    standard: bool,
    /// Convert to Standardised NSZ: the Standard NSP with its program and
    /// public data NCAs compressed to NCZ (implies --standard).
    #[arg(short = 'S', long = "nsz")]
    nsz: bool,
    /// Path to standardised NSP output directory.
    #[arg(short = 'o', long = "nspdir", default_value = "./nsp")]
    nspdir: String,
    /// Prints actions to be performed but does not execute any of them.
    #[arg(short = 'd', long = "dryrun")]
    dryrun: bool,
    /// Do not include any languages in output filename.
    #[arg(long = "nl")]
    no_languages: bool,
    /// Use short language codes in output filename.
    #[arg(long = "sl")]
    short_languages: bool,
    /// Set output level to 'quiet'. Minimal display for details.
    #[arg(short = 'q', long = "quiet")]
    quiet: bool,
    /// Force hash verification of bad NCA files (where header validation has already failed).
    #[arg(long = "force-hash")]
    force_hash: bool,
    /// When creating a standard NSP, include Delta Fragment files.
    #[arg(long = "keep-deltas")]
    keep_deltas: bool,
    /// Force conversion of NSP to standard NSP (even if already in standard format).
    #[arg(long = "force-convert")]
    force_convert: bool,
    /// Overwrite any existing files.
    #[arg(long = "overwrite")]
    overwrite: bool,
    /// Keep original name when converting to standard NSP.
    #[arg(long = "keep-name")]
    keep_name: bool,
    /// Delete source NSP file after conversion.
    #[arg(short = 'Z', long = "delete-source")]
    delete_source: bool,
    /// Exit batch processing on first error.
    #[arg(short = 'e', long = "exit-on-error")]
    exit_on_error: bool,
    /// Process a certain number of files and then exit.
    #[arg(short = 'b', long = "batch", default_value_t = 0)]
    batch: usize,
    /// Skip a certain number of files before processing.
    #[arg(long = "skip", default_value_t = 0)]
    skip: usize,
    /// Path to titledb.db file.
    #[arg(long = "titledb", default_value = "./titledb/titledb.db")]
    titledb: String,
    /// Verify title against TitleDB.
    #[arg(short = 'y', long = "verify")]
    verify: bool,
    /// Check headers and padding of NCA files.
    #[arg(short = 'H', long = "check-padding")]
    check_padding: bool,
    /// Path to NSP file or directory.
    nsp: String,
}

/// Expand a leading `~` only; a `~` elsewhere in the path is a literal.
/// Windows usually has no HOME, so fall back to USERPROFILE.
fn expand_home(p: &str) -> String {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"));
    match (p.strip_prefix('~'), home) {
        (Some(rest), Ok(home)) => format!("{home}{rest}"),
        _ => p.to_string(),
    }
}

/// .NET `Path.GetFullPath`: make the path absolute and resolve `.` and `..`
/// lexically, so the output path length matches the C# tool.
fn full_path(p: &str) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| PathBuf::from(p));
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// Recursively collect files matching `*.ns?`, sorted case-insensitively.
///
/// Like .NET `Directory.EnumerateFiles`, symlinked directories are followed.
/// A directory that resolves to one of its own ancestors is skipped, so a
/// symlink loop ends instead of recursing until the stack overflows.
fn collect_nsps(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    collect_nsps_in(dir, &mut Vec::new(), out)
}

fn collect_nsps_in(
    dir: &Path,
    ancestors: &mut Vec<PathBuf>,
    out: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    let real = std::fs::canonicalize(dir)?;
    if ancestors.contains(&real) {
        return Ok(());
    }
    ancestors.push(real);
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_nsps_in(&path, ancestors, out)?;
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(pos) = name.rfind('.') {
            let ext = &name[pos + 1..];
            if ext.starts_with("ns") && ext.chars().count() <= 3 {
                out.push(path);
            }
        }
    }
    ancestors.pop();
    Ok(())
}

fn validate(args: ValidateArgs) -> u8 {
    let keys_path = PathBuf::from(expand_home(&args.keys));
    let cert_path = PathBuf::from(expand_home(&args.cert));
    let nsp = PathBuf::from(expand_home(&args.nsp));
    let nsp_dir = full_path(&expand_home(&args.nspdir));

    let mut collection = Vec::new();
    if nsp.is_dir() {
        if let Err(e) = collect_nsps(&nsp, &mut collection) {
            eprintln!("Error: failed to list {}: {e}", nsp.display());
            return 1;
        }
        // .NET StringComparer.OrdinalIgnoreCase: simple (single char)
        // uppercase mapping, compared as UTF-16 code units.
        collection.sort_by_cached_key(|p| {
            let upper: String = p
                .to_string_lossy()
                .chars()
                .map(|c| {
                    let mut u = c.to_uppercase();
                    match (u.next(), u.next()) {
                        (Some(x), None) => x,
                        _ => c,
                    }
                })
                .collect();
            upper.encode_utf16().collect::<Vec<u16>>()
        });
    } else if !nsp.is_file() {
        eprintln!("Error: NSP file '{}' does not exist.", nsp.display());
        return 1;
    }
    if !keys_path.is_file() {
        eprintln!("Error: Keys file '{}' does not exist.", keys_path.display());
        return 1;
    }
    let cert = match std::fs::read(&cert_path) {
        Ok(c) => Some(c),
        // Taken from the first NSP that carries it and cached there.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!(
                "Error: failed to read certificate file '{}': {e}",
                cert_path.display()
            );
            return 1;
        }
    };
    let convert = args.standard || args.nsz;
    if convert
        && !nsp_dir.is_dir()
        && let Err(e) = std::fs::create_dir_all(&nsp_dir)
    {
        eprintln!("Error: failed to create {}: {e}", nsp_dir.display());
        return 1;
    }
    if cert
        .as_deref()
        .is_some_and(|c| !validate::is_common_cert(c))
    {
        eprintln!("Error: Common cert '{}' is invalid.", cert_path.display());
        return 1;
    }
    if nsp.is_dir() && collection.is_empty() {
        eprintln!("Error: No NSP files found.");
        return 1;
    }

    let keys = match libhac::Keys::load(&keys_path) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("Error: {:#}", anyhow::Error::from(e));
            return 1;
        }
    };
    for e in keys.skipped() {
        eprintln!("Warning: {e}, skipping");
    }

    let mut language_mode = LanguageMode::Full;
    if args.no_languages {
        language_mode = LanguageMode::None;
    }
    if args.short_languages {
        language_mode = LanguageMode::Short;
    }
    let settings = validate::Settings {
        cert: cert.map(std::cell::OnceCell::from).unwrap_or_default(),
        cert_path,
        convert,
        nsz: args.nsz,
        nsp_dir,
        dry_run: args.dryrun,
        language_mode,
        force_hash: args.force_hash,
        keep_deltas: args.keep_deltas,
        force_convert: args.force_convert,
        overwrite: args.overwrite,
        keep_name: args.keep_name,
        delete_source: args.delete_source,
        exit_on_error: args.exit_on_error,
        check_padding: args.check_padding,
        titledb: full_path(&expand_home(&args.titledb)),
        verify_title: args.verify,
    };

    log::set_progress(!args.quiet);
    if collection.is_empty() {
        log::set_quiet(args.quiet);
        let code = validate::process(&settings, &keys, &nsp);
        return code as u8;
    }

    // Batch mode is always quiet.
    log::set_quiet(true);
    let mut files: Vec<PathBuf> = collection.into_iter().skip(args.skip).collect();
    if args.batch > 0 {
        files.truncate(args.batch);
    }
    let total = files.len();
    println!("Processing {total} NSPs ..");
    let mut code = 0;
    for (i, f) in files.iter().enumerate() {
        code = validate::process(&settings, &keys, f);
        if args.exit_on_error && code != 0 {
            ftl!("Exiting on first error..");
            return code as u8;
        }
        println!("----- [{}/{total}] -----", i + 1);
    }
    code as u8
}

fn build_titledb(args: BuildTitleDbArgs) -> u8 {
    let dir = full_path(&expand_home(&args.titledbdir));
    if !dir.is_dir() {
        eprintln!("Error: TitleDB directory {} does not exist.", dir.display());
        return 1;
    }
    if !dir.join("languages.json").is_file() {
        eprintln!(
            "Error: TitleDB directory {} does not contain languages.json.",
            dir.display()
        );
        return 1;
    }
    match build_titledb::run(&dir, args.clean) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Error: {e:#}");
            1
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate(args) => ExitCode::from(validate(args)),
        Command::BuildTitleDb(args) => ExitCode::from(build_titledb(args)),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn collect_nsps_symlinks() {
        use std::os::unix::fs::symlink;
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/collect_nsps_symlinks");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();
        std::fs::write(root.join("a/x.nsp"), b"x").unwrap();
        std::fs::write(root.join("other/y.nsp"), b"y").unwrap();
        symlink("..", root.join("a/loop")).unwrap();
        symlink("../other", root.join("a/linkdir")).unwrap();

        let mut out = Vec::new();
        collect_nsps(&root.join("a"), &mut out).unwrap();
        out.sort();
        let a = root.join("a");
        assert_eq!(
            out,
            [
                a.join("linkdir/y.nsp"),
                a.join("loop/other/y.nsp"),
                a.join("x.nsp"),
            ]
        );
    }
}
