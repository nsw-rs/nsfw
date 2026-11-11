// SPDX-FileCopyrightText: 2024 mRg
// SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>
//
// SPDX-License-Identifier: MIT OR Apache-2.0

mod build_titledb;
mod diacritics;
mod log;
mod naming;
mod query;
mod stop;
mod titledb;
mod validate;

use clap::{Parser, Subcommand};
use naming::LanguageMode;
use std::collections::HashSet;
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
    /// Look up a title ID in TitleDB.
    #[command(visible_alias = "q")]
    Query(QueryArgs),
    /// Build titledb.db from the blawar/titledb JSON files.
    #[command(name = "build-titledb")]
    BuildTitleDb(BuildTitleDbArgs),
}

#[derive(clap::Args)]
struct QueryArgs {
    /// Path to titledb.db file.
    #[arg(long = "titledb", default_value = "./titledb/titledb.db")]
    titledb: PathBuf,
    /// Title ID to search for.
    #[arg(value_name = "TITLEID")]
    title_id: String,
}

#[derive(clap::Args)]
struct BuildTitleDbArgs {
    /// Path to TitleDB jsons directory.
    #[arg(long = "titledbdir", default_value = "./titledb")]
    titledbdir: PathBuf,
    /// Clean database before rebuilding.
    #[arg(short = 'c', long = "clean")]
    clean: bool,
}

#[derive(clap::Args)]
struct ValidateArgs {
    /// Path to NSW keys file.
    #[arg(short = 'k', long = "keys", default_value = "~/.switch/prod.keys")]
    keys: PathBuf,
    /// Path to 0x700-byte long common certificate chain file (created from the NSP if missing).
    #[arg(short = 'c', long = "cert", default_value = "~/.switch/common.cert")]
    cert: PathBuf,
    /// Convert to Standardised NSP.
    #[arg(short = 's', long = "standard", conflicts_with = "rename")]
    standard: bool,
    /// Convert to Standardised NSZ: the Standard NSP with its program and
    /// public data NCAs compressed to NCZ (implies --standard).
    #[arg(short = 'S', long = "nsz", conflicts_with = "rename")]
    nsz: bool,
    /// Rename NSP to match TitleDB. No other actions performed.
    #[arg(short = 'r', long = "rename")]
    rename: bool,
    /// When re-naming files, skip NCA hash validation.
    #[arg(long = "skip-hash", conflicts_with_all = ["standard", "nsz"])]
    skip_hash: bool,
    /// Path to standardised NSP output directory.
    #[arg(short = 'o', long = "nspdir", default_value = "./nsp")]
    nspdir: PathBuf,
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
    titledb: PathBuf,
    /// Verify title against TitleDB.
    #[arg(short = 'y', long = "verify")]
    verify: bool,
    /// Check headers and padding of NCA files.
    #[arg(short = 'H', long = "check-padding")]
    check_padding: bool,
    /// Paths to NSP files or directories.
    #[arg(required = true)]
    nsp: Vec<PathBuf>,
}

/// Expand a leading `~` component: `~user` and a file named `~x.nsp` are
/// left alone. Windows usually has no HOME, so fall back to USERPROFILE.
/// Without either the run ends: a directory named `~` is never meant.
fn expand_home(p: &Path) -> PathBuf {
    let Ok(rest) = p.strip_prefix("~") else {
        return p.to_path_buf();
    };
    let home = ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|h| !h.is_empty());
    let Some(home) = home else {
        eprintln!("Error: cannot expand '{}': HOME is not set.", p.display());
        std::process::exit(1);
    };
    let mut home = PathBuf::from(home);
    if !rest.as_os_str().is_empty() {
        home.push(rest);
    }
    home
}

/// `path` with `suffix` added to its file name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(suffix);
    p.into()
}

/// .NET `Path.GetFullPath`: make the path absolute and resolve `.` and `..`
/// lexically, so the output path length matches the C# tool.
fn full_path(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
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
/// The extension matches in any case on every system (.NET does that on
/// Windows and macOS only).
///
/// Like .NET `Directory.EnumerateFiles`, symlinked directories are followed.
/// A directory is listed once, by the first path that reaches it, so a
/// symlink loop ends and a link to a directory of the tree adds nothing.
/// A directory that cannot be listed is reported and left out: the result
/// is then `false`.
fn collect_nsps(dir: &Path, out: &mut Vec<PathBuf>) -> bool {
    let start = out.len();
    let complete = collect_nsps_in(dir, &mut HashSet::new(), out);
    out[start..].sort_by_cached_key(|p| ordinal_ignore_case_key(p));
    complete
}

fn collect_nsps_in(dir: &Path, seen: &mut HashSet<PathBuf>, out: &mut Vec<PathBuf>) -> bool {
    let entries = std::fs::canonicalize(dir).and_then(|real| {
        if !seen.insert(real) {
            return Ok(Vec::new());
        }
        std::fs::read_dir(dir)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<Vec<_>>>()
    });
    let mut entries = match entries {
        Ok(e) => e,
        Err(e) => {
            let dir = dir.display().to_string();
            eprintln!("Warning: skipping {}: {e}", log::plain(&dir));
            return false;
        }
    };
    // In name order, so that the path a directory is first reached by does
    // not depend on the order the system lists them in.
    entries.sort();
    let mut complete = true;
    for path in entries {
        if path.is_dir() {
            complete &= collect_nsps_in(&path, seen, out);
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(pos) = name.rfind('.') {
            let ext = name[pos + 1..].to_lowercase();
            if ext.starts_with("ns") && ext.chars().count() <= 3 {
                out.push(path);
            }
        }
    }
    complete
}

/// .NET `StringComparer.OrdinalIgnoreCase` sort key: simple (single char)
/// uppercase mapping, compared as UTF-16 code units.
fn ordinal_ignore_case_key(p: &Path) -> Vec<u16> {
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
    upper.encode_utf16().collect()
}

fn validate(args: ValidateArgs) -> u8 {
    let keys_path = expand_home(&args.keys);
    let cert_path = expand_home(&args.cert);
    let nsp_dir = full_path(&expand_home(&args.nspdir));

    // One file argument keeps the single file mode of the C# tool; anything
    // else (a directory, or several paths) runs in batch mode.
    let one_arg = args.nsp.len() == 1;
    let mut single = None;
    let mut collection = Vec::new();
    let mut missing = false;
    for arg in &args.nsp {
        let nsp = expand_home(arg);
        if nsp.is_dir() {
            // The files of the directories that can be listed are still
            // processed, and the run ends with an error code.
            if !collect_nsps(&nsp, &mut collection) {
                if args.exit_on_error {
                    return 1;
                }
                missing = true;
            }
        } else if nsp.is_file() {
            if one_arg {
                single = Some(nsp);
            } else {
                collection.push(nsp);
            }
        } else {
            eprintln!("Error: NSP file '{}' does not exist.", nsp.display());
            // One path keeps the C# behavior. With several, the others are
            // still processed and the run ends with an error code.
            if one_arg || args.exit_on_error {
                return 1;
            }
            missing = true;
        }
    }
    // Overlapping arguments (a directory and a file inside it, or the same
    // path twice) and links to a file must not process a file twice: a
    // second pass would fail on an output that exists, or on a source that
    // `-Z` already deleted.
    let mut seen = HashSet::new();
    collection.retain(|p| seen.insert(std::fs::canonicalize(p).unwrap_or_else(|_| p.clone())));
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
    if cert
        .as_deref()
        .is_some_and(|c| !validate::is_common_cert(c))
    {
        eprintln!("Error: Common cert '{}' is invalid.", cert_path.display());
        return 1;
    }
    if single.is_none() && collection.is_empty() {
        eprintln!("Error: No NSP files found.");
        return 1;
    }
    // Not before a run that writes nothing: a dry run, or one without files.
    if convert
        && !args.dryrun
        && !nsp_dir.is_dir()
        && let Err(e) = std::fs::create_dir_all(&nsp_dir)
    {
        eprintln!("Error: failed to create {}: {e}", nsp_dir.display());
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
        rename: args.rename,
        skip_hash: args.skip_hash,
    };

    log::set_progress(!args.quiet);
    stop::on_signal();
    if let Some(nsp) = single {
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
    log::line(&format!("Processing {total} NSPs .."));
    // The worst code over all files, so a failure is never hidden by a later
    // success. A failure (1) is worse than an output that exists (2).
    let rank = |code: i32| match code {
        0 => 0,
        2 => 1,
        _ => 2,
    };
    let mut worst = i32::from(missing);
    for (i, f) in files.iter().enumerate() {
        let code = validate::process(&settings, &keys, f);
        if args.exit_on_error && code != 0 {
            ftl!("Exiting on first error..");
            return code as u8;
        }
        if rank(code) > rank(worst) {
            worst = code;
        }
        log::line(&format!("----- [{}/{total}] -----", i + 1));
    }
    worst as u8
}

fn query(args: QueryArgs) -> u8 {
    if args.title_id.chars().count() != 16 {
        eprintln!("Error: Title ID must be 16 characters long.");
        return 1;
    }
    // The ID goes into a LIKE pattern, where `%` and `_` match anything.
    if !args.title_id.chars().all(|c| c.is_ascii_hexdigit()) {
        eprintln!("Error: Title ID must be hexadecimal.");
        return 1;
    }
    let titledb = full_path(&expand_home(&args.titledb));
    if !titledb.is_file() {
        eprintln!("Error: TitleDB file {} does not exist.", titledb.display());
        return 1;
    }
    match query::run(&titledb, &args.title_id) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Error: {e:#}");
            1
        }
    }
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
        Command::Query(args) => ExitCode::from(query(args)),
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

        std::fs::write(root.join("a/z.NSP"), b"z").unwrap();
        std::fs::write(root.join("a/skip.nspx"), b"s").unwrap();

        let mut out = Vec::new();
        assert!(collect_nsps(&root.join("a"), &mut out));
        out.sort();
        let a = root.join("a");
        // `other` is listed through `linkdir`, and not again through `loop`.
        let found = [a.join("linkdir/y.nsp"), a.join("x.nsp"), a.join("z.NSP")];
        assert_eq!(out, found);

        // A directory that cannot be listed is left out, the rest stays.
        use std::os::unix::fs::PermissionsExt;
        let locked = root.join("a/locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root lists it anyway.
        let denied = std::fs::read_dir(&locked).is_err();
        let mut out = Vec::new();
        let complete = collect_nsps(&root.join("a"), &mut out);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        out.sort();
        assert_eq!(complete, !denied);
        assert_eq!(out, found);
    }
}
