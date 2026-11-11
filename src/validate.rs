// SPDX-FileCopyrightText: 2024 mRg
// SPDX-FileCopyrightText: 2018-2022 Alex Barney
// SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Port of `ValidateNspService.Process`: validate an NSP and optionally
//! rewrite it as a Standard NSP.

use crate::log::{blue, green, grey, olive, paren, red, tag};
use crate::naming::{self, Lang, LanguageMode, NameInputs};
use crate::stop::PartFile;
use crate::titledb::TitleDb;
use crate::{ftl, inf, vrb, wrn};
use anyhow::{Result, anyhow, bail};
use indicatif::{ProgressBar, ProgressStyle};
use libhac::cnmt::{self, Cnmt, ContentAttributes, MetaType};
use libhac::keys::Keys;
use libhac::nacp::{NACP_SIZE, Nacp};
use libhac::nca::{ContentType, DistributionType, FsType, Nca, SectionIndex, TitleKeys};
use libhac::ncz::{NczReader, NczWriter};
use libhac::partition::PartitionFs;
use libhac::pfs0::Pfs0Builder;
use libhac::romfs::RomFs;
use libhac::ticket::{LicenseType, SignatureType, Ticket, TitleKeyType};
use libhac::{ReadAt, SubStorage, TitleId, npdm};
use sha2::{Digest, Sha256};
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, ErrorKind, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const COMMON_CERT_SIZE: usize = 0x700;
pub const COMMON_CERT_SHA256: &str =
    "3c4f20dca231655e90c75b3e9689e4dd38135401029ab1f2ea32d1c2573f1dfe";

pub fn is_common_cert(data: &[u8]) -> bool {
    hex::encode(Sha256::digest(data)) == COMMON_CERT_SHA256
}

/// The small files that are read into memory whole have their sizes in the
/// NSP, and a sparse file can claim any size. A ticket is 0x2C0 bytes; the
/// CNMT and the NPDM are a few KiB.
const MAX_TICKET_SIZE: u64 = 0x10000;
const MAX_META_FILE_SIZE: u64 = 0x100_0000;

/// Size of the `XS00000020` ticket signer certificate, the first one in the
/// common chain. Some update NSPs carry the chain with the two certificates
/// swapped (`XS00000020` then `CA00000003`).
const XS_CERT_SIZE: usize = 0x300;

/// Returns `data` with its two certificates swapped back into canonical
/// order, or `None` when that is not the common chain either.
fn swapped_common_cert(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() != COMMON_CERT_SIZE {
        return None;
    }
    let swapped = [&data[XS_CERT_SIZE..], &data[..XS_CERT_SIZE]].concat();
    is_common_cert(&swapped).then_some(swapped)
}

pub struct Settings {
    /// Common certificate chain, loaded from `cert_path` or taken from the
    /// first NSP that carries it.
    pub cert: OnceCell<Vec<u8>>,
    /// Where the common certificate is cached when it is taken from an NSP.
    pub cert_path: PathBuf,
    pub convert: bool,
    /// Write a Standard NSZ instead: the Standard NSP with its program and
    /// public data NCAs compressed to NCZ.
    pub nsz: bool,
    pub nsp_dir: PathBuf,
    pub dry_run: bool,
    pub language_mode: LanguageMode,
    pub force_hash: bool,
    pub keep_deltas: bool,
    pub force_convert: bool,
    pub overwrite: bool,
    pub keep_name: bool,
    pub delete_source: bool,
    pub exit_on_error: bool,
    pub check_padding: bool,
    /// Absolute path of `titledb.db`; TitleDB is used only when it exists.
    pub titledb: PathBuf,
    pub verify_title: bool,
    /// Rename the NSP in place to its Standard name, nothing else.
    pub rename: bool,
    /// Skip the NCA hash checks (meant to make `rename` fast).
    pub skip_hash: bool,
}

/// Errors and warnings keep insertion order and drop duplicates (C# HashSet).
#[derive(Default)]
struct Messages(Vec<String>);

impl Messages {
    fn add(&mut self, m: String) {
        if !self.0.contains(&m) {
            self.0.push(m);
        }
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

struct RawEntry {
    name: String,
    offset: u64,
    size: u64,
}

struct ContentFile {
    file_name: String,
    hash: [u8; 32],
    size: u64,
    content_type: cnmt::ContentEntryType,
    /// Present in the NSP only as `<nca id>.ncz`.
    compressed: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum HashMatch {
    Missing,
    Match,
    Mismatch,
}

struct NcaInfo {
    header_valid: bool,
    sections_errored: bool,
    hash_match: HashMatch,
}

impl NcaInfo {
    fn is_errored(&self) -> bool {
        self.sections_errored || !self.header_valid
    }
}

struct FsNca<'a> {
    file_name: String,
    nca_id: String,
    /// For an `.ncz` entry this is the raw NCZ: only the header, which NCZ
    /// keeps uncompressed, reads as the NCA.
    nca: Nca<SubStorage<&'a File>>,
    ncz: Option<NczReader<SubStorage<&'a File>>>,
}

impl FsNca<'_> {
    /// For an `.ncz` entry, the file name without `.ncz`: the SHA256 prefix
    /// of the decompressed NCA.
    fn ncz_target_hash(&self) -> &str {
        self.file_name
            .strip_suffix(".ncz")
            .unwrap_or(&self.file_name)
    }

    /// The NCA this entry stands for: for an `.ncz` entry, the
    /// decompressed one.
    fn plain_nca(&self, keys: &Keys) -> Result<Nca<Box<dyn ReadAt + '_>>> {
        let s: Box<dyn ReadAt + '_> = match &self.ncz {
            Some(z) => Box::new(z),
            None => Box::new(*self.nca.storage()),
        };
        Ok(Nca::open(keys, s)?)
    }

    /// Name and size of this NCA in the Standard NSP.
    fn output(&self) -> Result<(String, u64)> {
        Ok(match &self.ncz {
            Some(z) => (format!("{}.nca", self.ncz_target_hash()), z.size()?),
            None => (self.file_name.clone(), self.nca.storage().size()?),
        })
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum LookupSource {
    Control,
    FileName,
    FileTitle,
    TitleDb,
}

fn display_type(t: MetaType) -> &'static str {
    match t {
        MetaType::SYSTEM_PROGRAM => "System Program",
        MetaType::SYSTEM_DATA => "System Data",
        MetaType::SYSTEM_UPDATE => "System Update",
        MetaType::BOOT_IMAGE_PACKAGE => "Boot Image Package",
        MetaType::APPLICATION => "Game",
        MetaType::PATCH => "Update",
        MetaType::ADD_ON_CONTENT => "DLC",
        MetaType::DELTA => "Delta",
        MetaType::DATA_PATCH => "DLC Update",
        _ => "UNKNOWN",
    }
}

fn display_type_short(t: MetaType) -> &'static str {
    match t {
        MetaType::APPLICATION => "BASE",
        MetaType::PATCH => "UPD",
        MetaType::ADD_ON_CONTENT => "DLC",
        MetaType::DATA_PATCH => "DLCUPD",
        _ => "UNKNOWN",
    }
}

fn assign_priority(name: &str) -> Option<u8> {
    if name.ends_with(".cnmt.nca") {
        Some(1)
    } else if name.ends_with(".nca") {
        Some(0)
    } else if name.ends_with(".tik") {
        Some(2)
    } else if name.ends_with(".cert") {
        Some(3)
    } else {
        None
    }
}

/// Port of `NsfwUtilities.IsOrderCorrect`. The C# code compares the
/// concatenated single-digit priorities with those of a stable sort (nulls
/// first); null priorities add nothing to either string, so this holds
/// exactly when the non-null priorities are already in ascending order.
fn is_order_correct(entries: &[RawEntry]) -> bool {
    entries
        .iter()
        .filter_map(|e| assign_priority(&e.name))
        .is_sorted()
}

/// True when both paths name the same existing file, including relative
/// paths, symlinks and hard links.
#[cfg(unix)]
fn is_same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

/// True when both paths name the same existing file, including relative
/// paths and symlinks. Hard links are not detected.
#[cfg(not(unix))]
fn is_same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// LibHac `OpenFileSystem(index, ErrorOnInvalid)`: open the verified
/// storage and parse the file-system header and tables.
fn check_section_fs(nca: &Nca<impl ReadAt>, i: SectionIndex, tk: &TitleKeys) -> Result<()> {
    let storage = nca.open_section(i, tk)?;
    match nca.fs_header(i).fs_type() {
        FsType::PFS0 => PartitionFs::parse_pfs0(&storage).map(|_| ())?,
        FsType::ROMFS => RomFs::open(&storage).map(|_| ())?,
        t => bail!("Unsupported file-system type {}", t.0),
    }
    Ok(())
}

fn read_pfs_file(
    nca: &Nca<impl ReadAt>,
    i: SectionIndex,
    tk: &TitleKeys,
    pick: impl Fn(&str) -> bool,
) -> Result<Option<Vec<u8>>> {
    let storage = nca.open_section(i, tk)?;
    if nca.fs_header(i).fs_type() != FsType::PFS0 {
        bail!("Section {i} is not a partition file-system");
    }
    let pfs = PartitionFs::parse_pfs0(&storage)?;
    let matches: Vec<_> = pfs.entries().iter().filter(|e| pick(&e.name)).collect();
    match matches.as_slice() {
        [] => Ok(None),
        [e] if e.size > MAX_META_FILE_SIZE => bail!("{} is too large ({})", e.name, e.size),
        [e] => Ok(Some(storage.read_vec(e.offset, e.size as usize)?)),
        _ => bail!("Sequence contains more than one element"),
    }
}

/// Port of `NsfwUtilities.VerifyNpdm`: Some(true) valid, Some(false)
/// invalid, None unchecked.
fn verify_npdm(nca: &Nca<impl ReadAt>, tk: &TitleKeys) -> Option<bool> {
    if nca.content_type() != ContentType::PROGRAM {
        return None;
    }
    // The ExeFS is section 0.
    let exefs = SectionIndex::new(0)?;
    let npdm = match read_pfs_file(nca, exefs, tk, |n| n == "main.npdm") {
        Ok(Some(b)) => b,
        _ => return None,
    };
    match npdm::acid_modulus(&npdm) {
        Ok(m) => Some(nca.verify_signature2(&m)),
        Err(e) => {
            crate::err!("{e}");
            Some(false)
        }
    }
}

fn progress_bar(label: &str, total: u64) -> ProgressBar {
    let pb = if crate::log::show_progress() {
        ProgressBar::new(total)
    } else {
        ProgressBar::hidden()
    };
    let template = format!(
        "{label} .. {{bar:40}} {{bytes}}/{{total_bytes}} {{binary_bytes_per_sec}} {{percent:>3}}% {{eta}}"
    );
    pb.set_style(ProgressStyle::with_template(&template).unwrap_or(ProgressStyle::default_bar()));
    pb
}

fn hash_range(src: &dyn ReadAt, offset: u64, size: u64) -> Result<String> {
    let pb = progress_bar("Hashing", size);
    let mut sha = Sha256::new();
    src.read_chunks(offset, size, &mut |chunk| {
        sha.update(chunk);
        pb.inc(chunk.len() as u64);
        Ok(())
    })?;
    pb.finish_and_clear();
    Ok(hex::encode_upper(sha.finalize()))
}

/// SHA256 of the NCA that an NCZ decompresses to.
fn hash_ncz(ncz: &NczReader<impl ReadAt>) -> Result<String> {
    hash_range(ncz, 0, ncz.size()?)
}

/// How much of a solid NCZ is kept while it is checked: of its start one
/// part in 256, within these bounds, and the last bytes passed. The hash
/// levels of the first section are at its start, about one part in 512,
/// and are read again for data far behind them. A hash table that lies
/// just before its data is read after it.
const SOLID_HEAD: (u64, u64) = (0x100_0000, 0x1000_0000);
const SOLID_TAIL: usize = 0x100_0000;

/// The NCA of an `.ncz` entry, for the checks that read its sections and
/// for its hash.
///
/// A solid NCZ decodes from its start again for every read that goes
/// back. So it is passed front to back once: the bytes up to what a check
/// asks for are hashed on the way, and the start and the last bytes are
/// kept. A read behind that, which the kept parts do not hold, still
/// works, at the price of one more decoding. A block NCZ is read where the
/// checks ask.
struct Unpacked<'a, R> {
    ncz: &'a NczReader<R>,
    size: u64,
    pass: RefCell<Pass>,
}

struct Pass {
    /// Every byte before this offset was passed.
    pos: u64,
    /// The bytes from offset 0 that are kept, `head_max` at most.
    head: Vec<u8>,
    head_max: u64,
    /// The bytes up to `pos` that are kept: `SOLID_TAIL` to twice that.
    tail: Vec<u8>,
    /// `None` when the hash is not wanted.
    sha: Option<Sha256>,
    pb: ProgressBar,
}

impl Pass {
    /// Pass the bytes up to offset `to`.
    fn advance(&mut self, src: &impl ReadAt, to: u64) -> libhac::Result<()> {
        let mut buf = vec![0u8; to.saturating_sub(self.pos).min(0x100000) as usize];
        while self.pos < to {
            let n = (to - self.pos).min(buf.len() as u64) as usize;
            src.read_at(self.pos, &mut buf[..n])?;
            self.take(&buf[..n]);
        }
        Ok(())
    }

    /// Account for the bytes at `pos`, which were just read.
    fn take(&mut self, bytes: &[u8]) {
        let room = self.head_max.saturating_sub(self.pos);
        let keep = (bytes.len() as u64).min(room) as usize;
        self.head.extend_from_slice(&bytes[..keep]);
        if self.head_max > 0 {
            self.tail.extend_from_slice(bytes);
            if self.tail.len() > 2 * SOLID_TAIL {
                self.tail.drain(..self.tail.len() - SOLID_TAIL);
            }
        }
        if let Some(sha) = &mut self.sha {
            sha.update(bytes);
        }
        self.pb.inc(bytes.len() as u64);
        self.pos += bytes.len() as u64;
    }
}

impl Drop for Pass {
    fn drop(&mut self) {
        self.pb.finish_and_clear();
    }
}

impl<'a, R: ReadAt> Unpacked<'a, R> {
    fn new(ncz: &'a NczReader<R>, hash: bool) -> Result<Self> {
        let size = ncz.size()?;
        let label = if hash { "Hashing" } else { "Checking" };
        Ok(Unpacked {
            ncz,
            size,
            pass: RefCell::new(Pass {
                pos: 0,
                head: Vec::new(),
                head_max: if ncz.is_solid() {
                    (size / 256).clamp(SOLID_HEAD.0, SOLID_HEAD.1)
                } else {
                    0
                },
                tail: Vec::new(),
                sha: hash.then(Sha256::new),
                // Reads of a block NCZ skip the pass, so without the hash
                // nothing moves this bar.
                pb: if hash || ncz.is_solid() {
                    progress_bar(label, size)
                } else {
                    ProgressBar::hidden()
                },
            }),
        })
    }

    /// Upper-case hex SHA256 of the whole NCA.
    fn hash(&self) -> Result<String> {
        let mut pass = self.pass.borrow_mut();
        pass.advance(self.ncz, self.size)?;
        let sha = pass.sha.take().ok_or_else(|| anyhow!("no hash taken"))?;
        Ok(hex::encode_upper(sha.finalize()))
    }
}

impl<R: ReadAt> ReadAt for Unpacked<'_, R> {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> libhac::Result<()> {
        let in_range = offset
            .checked_add(buf.len() as u64)
            .is_some_and(|end| end <= self.size);
        // A read out of range gets its error from the NCZ.
        if !self.ncz.is_solid() || !in_range {
            return self.ncz.read_at(offset, buf);
        }
        let mut pass = self.pass.borrow_mut();
        let mut done = 0;
        while done < buf.len() {
            let at = offset + done as u64;
            let rest = &mut buf[done..];
            let kept = pass.head.len() as u64;
            done += if at < kept {
                let n = (kept - at).min(rest.len() as u64) as usize;
                rest[..n].copy_from_slice(&pass.head[at as usize..][..n]);
                n
            } else if at < pass.pos {
                let n = (pass.pos - at).min(rest.len() as u64) as usize;
                let from = usize::try_from(pass.pos - at)
                    .ok()
                    .and_then(|back| pass.tail.len().checked_sub(back));
                match from {
                    Some(from) => rest[..n].copy_from_slice(&pass.tail[from..][..n]),
                    // Passed and not kept.
                    None => self.ncz.read_at(at, &mut rest[..n])?,
                }
                n
            } else {
                pass.advance(self.ncz, at)?;
                self.ncz.read_at(at, rest)?;
                pass.take(rest);
                rest.len()
            };
        }
        Ok(())
    }

    fn size(&self) -> libhac::Result<u64> {
        Ok(self.size)
    }
}

/// What the Standard NSP does with one CNMT content entry.
enum Planned {
    /// Index into the NSP's NCAs.
    Add(usize),
    SkipDelta(String),
    DeltaNotFound(String),
    Missing(String),
    MissingNcz(String),
}

fn plan_content(content_files: &[ContentFile], ncas: &[FsNca], keep_deltas: bool) -> Vec<Planned> {
    content_files
        .iter()
        .map(|cf| {
            let is_delta = cf.content_type == cnmt::ContentEntryType::DELTA_FRAGMENT;
            let name = cf.file_name.clone();
            if !keep_deltas && is_delta {
                return Planned::SkipDelta(name);
            }
            if cf.compressed {
                let stem = cf.file_name.strip_suffix(".nca").unwrap_or(&cf.file_name);
                let ncz = format!("{stem}.ncz");
                return match ncas.iter().position(|n| n.file_name == ncz) {
                    Some(i) => Planned::Add(i),
                    None => Planned::MissingNcz(ncz),
                };
            }
            match ncas.iter().position(|n| n.file_name == cf.file_name) {
                Some(i) => Planned::Add(i),
                None if is_delta => Planned::DeltaNotFound(name),
                None => Planned::Missing(name),
            }
        })
        .collect()
}

/// NCAs of the Standard NSP in file order: content NCAs, then the meta NCA.
fn output_order(plan: &[Planned], meta_idx: usize) -> Vec<usize> {
    let content = plan.iter().filter_map(|p| {
        if let Planned::Add(i) = p {
            Some(*i)
        } else {
            None
        }
    });
    content.chain([meta_idx]).collect()
}

/// Ticket and certificate written after the NCAs, when the title uses title-key crypto.
/// `None` when a ticket is written but no common certificate is available.
fn tail_files(ticket: Option<&Ticket>, cert: Option<&Vec<u8>>) -> Option<Vec<(String, Vec<u8>)>> {
    let Some(t) = ticket else {
        return Some(Vec::new());
    };
    let rid = hex::encode(t.rights_id.0);
    Some(vec![
        (format!("{rid}.tik"), t.to_bytes()),
        (format!("{rid}.cert"), cert?.clone()),
    ])
}

const NO_CERT: &str = "No common certificate available. Process an NSP with a valid ticket certificate first, or supply one with --cert.";

/// Keeps the common certificate taken from an NSP for the rest of the run and
/// saves it to `cert_path` when that file does not exist yet.
fn cache_common_cert(settings: &Settings, cert: Vec<u8>) {
    let cert = settings.cert.get_or_init(|| cert);
    if settings.dry_run || settings.cert_path.exists() {
        return;
    }
    let written = settings
        .cert_path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&settings.cert_path, cert));
    match written {
        Ok(()) => inf!(
            "Common certificate cached to {}",
            olive(settings.cert_path.display())
        ),
        Err(e) => wrn!(
            "Failed to cache common certificate to {}: {e}",
            olive(settings.cert_path.display())
        ),
    }
}

/// A Standard NSP being written next to its target as `<target>.part`.
/// The NCAs are copied and hashed in one pass; the ticket and certificate,
/// which may still be regenerated, are appended by `finish`. Dropping it
/// before `finish` deletes the partial file.
struct PartialNsp {
    out: Option<BufWriter<File>>,
    /// After `out`, so that the file is closed before it is deleted.
    part: PartFile,
    /// Files before the tail, with their written sizes. For an NSZ the
    /// sizes are known only once the files are written, so `finish`
    /// writes the header again.
    files: Vec<(String, u64)>,
    tail_sizes: Vec<u64>,
    /// Upper-case hex SHA256 of each written NCA, by the name of its entry
    /// in the source. For an `.ncz` entry it is that of the decompressed NCA.
    hashes: HashMap<String, String>,
    /// Offset and size, in the partial file, of each NCA that was written
    /// decompressed from an `.ncz` entry, by the name of that entry.
    copies: HashMap<String, (u64, u64)>,
}

impl PartialNsp {
    fn start(
        target: &Path,
        keys: &Keys,
        ncas: &[FsNca],
        order: &[usize],
        tail: &[(String, Vec<u8>)],
        nsz: Option<&TitleKeys>,
    ) -> Result<Self> {
        let tmp = crate::with_suffix(target, ".part");
        // Create first: an invalid target fails before any NCA is read.
        // Never an existing file, also with `--overwrite`, and never
        // through a link: it can be what another run for the same title is
        // writing, and that run would give its name to this file.
        let (part, file) = PartFile::open(
            tmp.clone(),
            OpenOptions::new().read(true).write(true).create_new(true),
        )
        .map_err(|e| match e.kind() {
            ErrorKind::AlreadyExists => anyhow!(
                "{} exists: another run is writing it, or a run was killed. Delete it if no run is active.",
                tmp.display()
            ),
            _ => e.into(),
        })?;
        let mut p = PartialNsp {
            out: Some(BufWriter::with_capacity(0x100000, file)),
            part,
            files: Vec::new(),
            tail_sizes: tail.iter().map(|t| t.1.len() as u64).collect(),
            hashes: HashMap::new(),
            copies: HashMap::new(),
        };
        // Each writer is planned once: planning reads the AesCtrEx tables,
        // which for a solid NCZ input means decoding up to them.
        let plain = order
            .iter()
            .map(|&i| ncas[i].plain_nca(keys))
            .collect::<Result<Vec<_>>>()?;
        let mut writers = Vec::new();
        let mut total = 0;
        for (&i, nca) in order.iter().zip(&plain) {
            let (mut name, size) = ncas[i].output()?;
            let w = match nsz {
                Some(tk) => ncz_writer(nca, tk)?,
                None => None,
            };
            if w.is_some() {
                name = format!("{}.ncz", name.strip_suffix(".nca").unwrap_or(&name));
            }
            p.files.push((name, size));
            writers.push(w);
            total += size;
        }

        let header = p.header(tail);
        let out = p.out.as_mut().expect("started");
        out.write_all(&header)?;
        let mut at = header.len() as u64;
        let label = if nsz.is_some() {
            "Compressing"
        } else {
            "Writing"
        };
        let pb = progress_bar(label, total);
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        for (k, &i) in order.iter().enumerate() {
            let mut sha = Sha256::new();
            // Every NCA byte goes here once, in order.
            let mut seen = |chunk: &[u8]| {
                sha.update(chunk);
                pb.inc(chunk.len() as u64);
            };
            p.files[k].1 = match &writers[k] {
                Some(w) => w.write(out, threads, &mut seen)?,
                None => {
                    let src = plain[k].storage();
                    let size = src.size()?;
                    src.read_chunks(0, size, &mut |chunk| {
                        seen(chunk);
                        out.write_all(chunk)?;
                        Ok(())
                    })?;
                    size
                }
            };
            let hash = sha.finalize();
            if ncas[i].ncz.is_some() {
                let current = hex::encode_upper(&hash[..16]);
                let target = ncas[i].ncz_target_hash().to_uppercase();
                if current != target {
                    bail!(
                        "Current hash ({current}) does not match original hash ({target}). File is corrupted."
                    );
                }
            }
            p.hashes
                .insert(ncas[i].file_name.clone(), hex::encode_upper(hash));
            if ncas[i].ncz.is_some() && writers[k].is_none() {
                p.copies
                    .insert(ncas[i].file_name.clone(), (at, p.files[k].1));
            }
            at += p.files[k].1;
        }
        // The copies are read back through the same file.
        out.flush()?;
        pb.finish_and_clear();
        Ok(p)
    }

    /// The NCA that was written for the `.ncz` entry `name`, read back from
    /// the output. `None` when the output holds it compressed.
    fn copy_of(&self, name: &str) -> Option<SubStorage<&File>> {
        let &(offset, size) = self.copies.get(name)?;
        SubStorage::new(self.out.as_ref()?.get_ref(), offset, size).ok()
    }

    fn finish(mut self, target: &Path, tail: &[(String, Vec<u8>)]) -> Result<()> {
        if tail
            .iter()
            .map(|t| t.1.len() as u64)
            .ne(self.tail_sizes.iter().copied())
        {
            bail!("ticket or certificate size changed after the header was written");
        }
        let mut out = self.out.take().expect("started");
        // On Windows, reading a copy back moved the file position.
        out.seek(SeekFrom::End(0))?;
        for (_, bytes) in tail {
            out.write_all(bytes)?;
        }
        out.seek(SeekFrom::Start(0))?;
        out.write_all(&self.header(tail))?;
        // A write that the disk refuses later (full, quota, network) shows
        // up here. Without it the file gets its name, and with `-Z` the
        // source goes, while part of the data never arrives.
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        self.part.rename(target)?;
        sync_parent(target);
        Ok(())
    }

    /// The PFS0 header of the files and `tail`.
    fn header(&self, tail: &[(String, Vec<u8>)]) -> Vec<u8> {
        let mut layout = Pfs0Builder::new();
        for (n, size) in &self.files {
            layout.add_file(n.as_str(), *size);
        }
        for (n, b) in tail {
            layout.add_file(n.as_str(), b.len() as u64);
        }
        layout.to_bytes()
    }
}

/// The NCZ writer of `nca` when nsz would compress it: program and public
/// data NCAs only.
fn ncz_writer<'a, S: ReadAt>(nca: &'a Nca<S>, tk: &TitleKeys) -> Result<Option<NczWriter<'a, S>>> {
    let ct = nca.content_type();
    if ct != ContentType::PROGRAM && ct != ContentType::PUBLIC_DATA {
        return Ok(None);
    }
    Ok(NczWriter::new(nca, tk)?)
}

/// Make the new name of `path` last through a power cut. Best effort: not
/// every filesystem can sync a directory.
#[cfg(unix)]
fn sync_parent(path: &Path) {
    if let Some(dir) = path.parent()
        && let Ok(dir) = File::open(dir)
    {
        let _ = dir.sync_all();
    }
}

/// Windows cannot open a directory as a file.
#[cfg(not(unix))]
fn sync_parent(_path: &Path) {}

/// Refuses output paths over `limit` UTF-16 units on Windows, as the
/// original tool does (254 for conversion, 258 for rename). std writes
/// longer paths fine (it adds the `\\?\` prefix), but unless long paths are
/// enabled system wide and per application, most other programs (Explorer,
/// certutil, PowerShell 5.1) cannot open the result.
/// Elsewhere the filesystem decides: a name it rejects fails the create of
/// the `.part` file and is reported then.
#[cfg(windows)]
fn path_too_long(target: &Path, limit: usize) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    let n = target.as_os_str().encode_wide().count();
    (n > limit).then(|| format!("Path too long for Windows ({n})"))
}

#[cfg(not(windows))]
fn path_too_long(_target: &Path, _limit: usize) -> Option<String> {
    None
}

/// File name for the renamed NSP: the Standard name plus the source
/// extension, so an NSZ stays an NSZ (the original always used `.nsp`).
fn rename_file_name(file_name: &str, output_name: &str) -> String {
    let is_nsz = Path::new(file_name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("nsz"));
    format!("{output_name}.{}", if is_nsz { "nsz" } else { "nsp" })
}

/// RENAME: move the NSP to its Standard name in the same directory.
fn rename(settings: &Settings, nsp_path: &Path, file_name: &str, output_name: &str) -> i32 {
    let new_name = rename_file_name(file_name, output_name);
    if file_name == new_name {
        inf!("Renaming skipped. Nothing to do. Filename matches already.");
        return 0;
    }
    if !naming::is_plain_file_name(&new_name) {
        ftl!("Invalid output file name ({new_name}).");
        return 1;
    }
    let target = nsp_path.with_file_name(&new_name);
    if let Some(msg) = path_too_long(
        &std::path::absolute(&target).unwrap_or_else(|_| target.clone()),
        258,
    ) {
        crate::err!("{msg}");
        return 1;
    }
    // A case-only rename on a case-insensitive filesystem finds the source.
    // Any other existing entry (including a dangling symlink, a symlink to
    // the source or a hard link of it, which rename would leave in place)
    // needs --overwrite.
    let case_only =
        file_name.to_lowercase() == new_name.to_lowercase() && is_same_file(&target, nsp_path);
    if target.symlink_metadata().is_ok() && !settings.overwrite && !case_only {
        crate::err!(
            "File with the same name already exists. ({new_name}). Use {} to overwrite an existing file.",
            grey("--overwrite")
        );
        return 2;
    }
    if settings.dry_run {
        inf!("{} -> Rename FROM: {}", tag("DRYRUN"), olive(file_name));
        inf!("{} ->   Rename TO: {}", tag("DRYRUN"), olive(&new_name));
        return 0;
    }
    if let Err(e) = std::fs::rename(nsp_path, &target) {
        crate::err!("Failed to rename file. {e}");
        return 1;
    }
    inf!("Renamed TO: {}", olive(output_name));
    0
}

pub fn process(settings: &Settings, keys: &Keys, nsp_path: &Path) -> i32 {
    match process_inner(settings, keys, nsp_path) {
        Ok(code) => code,
        Err(e) => {
            ftl!("{e:#}");
            1
        }
    }
}

fn process_inner(settings: &Settings, keys: &Keys, nsp_path: &Path) -> Result<i32> {
    if !nsp_path.is_file() {
        ftl!("File not found: {}", olive(nsp_path.display()));
        return Ok(1);
    }
    let file_name = nsp_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut errors = Messages::default();
    let mut warnings = Messages::default();
    let mut can_proceed = true;
    let mut generate_new_ticket = false;
    let mut bad_padding = false;
    let mut copy_new_cert = false;
    let mut has_loose_files = false;
    let mut has_sparse_ncas = false;

    inf!("Validating NSP : {}", olive(&file_name));
    if settings.convert {
        let mut extra = String::new();
        if settings.dry_run {
            extra = format!(" ({})", olive("Dry Run"));
        }
        if settings.force_convert {
            extra = format!(" ({})", olive("Force"));
        }
        if settings.delete_source {
            extra += &format!(" ({})", olive("Delete Source"));
        }
        inf!("Output Mode <- {}{extra}", green("CONVERT"));
    }

    let titledb = if settings.titledb.is_file() {
        let db = TitleDb::open(&settings.titledb)?;
        if settings.verify_title {
            vrb!(
                "{} <- {} {}",
                olive("TitleDB"),
                green("Found"),
                paren(settings.titledb.display())
            );
        }
        Some(db)
    } else {
        if settings.verify_title {
            wrn!(
                "{} <- {} {}",
                olive("TitleDB"),
                red("Not Found"),
                paren(settings.titledb.display())
            );
        }
        None
    };
    if settings.rename {
        let extra = if settings.dry_run {
            format!(" ({})", olive("Dry Run"))
        } else {
            String::new()
        };
        inf!("Output Mode <- {}{extra}", green("RENAME"));
    }

    let mut phase = olive("Validate File Structure");
    let file = File::open(nsp_path)?;
    let file_size = file.metadata()?.len();
    let whole = SubStorage::new(&file, 0, file_size)?;

    let mut head = [0u8; 12];
    if file_size < 12 || whole.read_at(0, &mut head).is_err() || &head[..4] != b"PFS0" {
        ftl!("Cannot mount file-system. Invalid NSP file.");
        return Ok(1);
    }
    let entry_count = i32::from_le_bytes(head[4..8].try_into().unwrap()) as i64;
    let string_table_size = i32::from_le_bytes(head[8..12].try_into().unwrap()) as i64;
    let header_size = 0x10 + entry_count * 0x18 + string_table_size;
    let aligned_header_size = (header_size + 0x1F) & !0x1F;
    if header_size != aligned_header_size {
        bad_padding = true;
        errors.add(format!(
            "{phase} <- NSP file-header is not aligned correctly. {}",
            paren(format_args!("{header_size:08X}=>{aligned_header_size:08X}"))
        ));
    }

    if file_size >= 57 {
        let tail = whole.read_vec(file_size - 57, 57)?;
        if tail[0] != 125 && tail[2] != 1 && tail[4] != 1 {
            bad_padding = true;
            errors.add(format!(
                "{phase} <- NSP has incorrect padding at the end of the file."
            ));
        }
    }

    phase = olive("Validate File-System");

    let mut raw_entries: Vec<RawEntry> = Vec::new();
    let mut title_keys = TitleKeys::new();
    let mut ticket: Option<Ticket> = None;
    let mut ncas: Vec<FsNca> = Vec::new();
    let mut main_nca: Option<usize> = None;
    let mut meta_nca: Option<usize> = None;
    let mut metadata: Option<Cnmt> = None;

    let scan = (|| -> Result<Option<i32>> {
        let pfs = PartitionFs::parse_pfs0(&whole)?;
        for e in pfs.entries() {
            let name = &e.name;
            let range = SubStorage::new(&file, e.offset, e.size)?;

            if name.ends_with(".tik") {
                let t = Ticket::parse(&range.read_vec(0, e.size.min(MAX_TICKET_SIZE) as usize)?)?;
                import_ticket(t, keys, &mut title_keys, &mut ticket)?;
                vrb!("{phase} <- Ticket imported {}", paren(name));
            }

            if name.ends_with(".cert") {
                if e.size as usize != COMMON_CERT_SIZE {
                    wrn!(
                        "{phase} <- Certificate size is incorrect. Expected 0x700 bytes. {}",
                        paren(name)
                    );
                    copy_new_cert = true;
                } else {
                    let data = range.read_vec(0, e.size as usize)?;
                    let is_common = is_common_cert(&data);
                    if !is_common {
                        wrn!(
                            "{phase} <- Certificate does not match common certificate SHA256. {}",
                            paren(name)
                        );
                        copy_new_cert = true;
                    }
                    if settings.cert.get().is_none() {
                        let cert = if is_common {
                            Some(data)
                        } else {
                            swapped_common_cert(&data)
                        };
                        if let Some(cert) = cert {
                            cache_common_cert(settings, cert);
                        }
                    }
                }
            }

            let is_ncz = name.ends_with(".ncz");
            if name.ends_with(".nca") || is_ncz {
                let nca = match Nca::open(keys, range) {
                    Ok(n) => n,
                    Err(err) => {
                        ftl!("{phase} <- Error opening NCA ({name}) - {err}");
                        return Ok(Some(1));
                    }
                };
                let nca_id = name
                    .get(..32)
                    .ok_or_else(|| anyhow!("short NCA name"))?
                    .to_string();
                if ncas.iter().any(|n| n.nca_id.eq_ignore_ascii_case(&nca_id)) {
                    bail!("duplicate NCA id");
                }
                let ncz = if is_ncz {
                    let reader = NczReader::open(SubStorage::new(&file, e.offset, e.size)?)?;
                    warnings.add(format!(
                        "{phase} <- Compressed NCA ({}) detected.",
                        blue("NCZ")
                    ));
                    Some(reader)
                } else {
                    None
                };
                let ct = nca.content_type();
                if main_nca.is_none()
                    && (ct == ContentType::PROGRAM || ct == ContentType::PUBLIC_DATA)
                {
                    main_nca = Some(ncas.len());
                }
                let is_meta = name.ends_with(".cnmt.nca");
                if is_meta {
                    let cnmt = read_pfs_file(&nca, nca.data_section_index(), &title_keys, |n| {
                        n.ends_with(".cnmt")
                    });
                    // Keys older than the title: say so, the file is not corrupt.
                    if let Err(e) = &cnmt
                        && matches!(e.downcast_ref(), Some(libhac::Error::MissingKey(_)))
                    {
                        let key_gen = nca.key_generation();
                        let rev = key_gen.master_key_revision();
                        ftl!(
                            "{phase} <- The keys file lacks the keys of key generation {key_gen} (key_area_key_*_{rev:02x}, titlekek_{rev:02x})."
                        );
                        return Ok(Some(1));
                    }
                    let cnmt = cnmt?.ok_or_else(|| anyhow!("Sequence contains no elements"))?;
                    let parsed = Cnmt::parse(&cnmt)?;
                    if parsed.attributes.contains(ContentAttributes::COMPACTED) {
                        has_sparse_ncas = true;
                        inf!("{phase} <- Sparse NCAs detected.");
                    }
                    metadata = Some(parsed);
                    meta_nca = Some(ncas.len());
                }
                ncas.push(FsNca {
                    file_name: name.clone(),
                    nca_id,
                    nca,
                    ncz,
                });
            }

            if assign_priority(name).is_none() && !is_ncz {
                has_loose_files = true;
            }
            if raw_entries.iter().any(|r| &r.name == name) {
                bail!("duplicate entry");
            }
            raw_entries.push(RawEntry {
                name: name.clone(),
                offset: e.offset,
                size: e.size,
            });
        }
        Ok(None)
    })();
    match scan {
        Ok(Some(code)) => return Ok(code),
        Ok(None) => {}
        Err(_) => {
            ftl!("{phase} <- Corrupt file-system entries. Unable to load.");
            ftl!("NSP Validation failed.");
            return Ok(1);
        }
    }

    if ticket.is_none() {
        vrb!("{phase} <- No valid tickets found.");
    }

    phase = olive("Validate Padding");
    let table_size: i64 = raw_entries.iter().map(|e| e.name.len() as i64 + 1).sum();
    let aligned_table_size = (table_size + 0x1F) & !0x1F;
    if table_size == aligned_table_size && aligned_table_size == string_table_size {
        bad_padding = true;
        errors.add(format!("{phase} <- NSP header needs additional padding."));
    }

    if settings.check_padding {
        vrb!("String Table Size           : {table_size}");
        vrb!("String Table Size (Aligned) : {aligned_table_size}");
        vrb!("String Table Size (Header)  : {string_table_size}");
        if bad_padding {
            ftl!("{phase} <- {}", red("Failed"));
            return Ok(1);
        }
        inf!("{phase} <- {}", green("Passed"));
        return Ok(0);
    }

    // NspStructure.Build(): titles keyed by NCA program id, control.nacp loaded.
    let mut titles: Vec<(TitleId, Nacp)> = Vec::new();
    if let Some(cnmt) = &metadata {
        for ce in &cnmt.content_entries {
            let id = hex::encode_upper(ce.nca_id.0);
            let Some(n) = ncas.iter().find(|n| n.nca_id.eq_ignore_ascii_case(&id)) else {
                continue;
            };
            let title_id = n.nca.program_id();
            let pos = match titles.iter().position(|t| t.0 == title_id) {
                Some(p) => p,
                None => {
                    titles.push((title_id, Nacp::empty()));
                    titles.len() - 1
                }
            };
            if ce.content_type == cnmt::ContentEntryType::CONTROL {
                let storage = n
                    .nca
                    .open_section(n.nca.data_section_index(), &title_keys)?;
                let romfs = RomFs::open(&storage)?;
                let file = romfs
                    .find("/control.nacp")?
                    .ok_or_else(|| anyhow!("control.nacp not found in {}", n.file_name))?;
                // The rest of a larger file is not looked at.
                let size = file.size.min(NACP_SIZE as u64) as usize;
                titles[pos].1 = Nacp::from_bytes(&storage.read_vec(file.offset, size)?);
            }
        }
    }

    phase = olive("Open File-System");
    vrb!("{phase} <- Loaded correctly.");

    phase = olive("Validate Metadata (CNMT)");
    let Some(cnmt) = metadata else {
        ftl!("Failed to open CNMT.");
        return Ok(1);
    };

    let title_id = cnmt.title_id.to_string();
    let base_title_id = cnmt.application_title_id().to_string();
    if title_id != base_title_id && cnmt.meta_type == MetaType::APPLICATION {
        errors.add(format!(
            "{phase} - TitleID Mis-match. Expected {base_title_id}, found {title_id}"
        ));
    }
    let title_version = format!("v{}", cnmt.title_version);
    let title_type = cnmt.meta_type;
    let is_dlc = matches!(title_type, MetaType::ADD_ON_CONTENT | MetaType::DATA_PATCH);

    if !matches!(
        title_type,
        MetaType::PATCH
            | MetaType::APPLICATION
            | MetaType::DELTA
            | MetaType::ADD_ON_CONTENT
            | MetaType::DATA_PATCH
    ) {
        ftl!("{phase} - Unsupported content type {title_type}");
        return Ok(1);
    }

    let mut content_files: Vec<ContentFile> = Vec::new();
    let mut delta_count = 0;
    for ce in &cnmt.content_entries {
        let nca_id = hex::encode_upper(ce.nca_id.0);
        let file_name = format!("{}.nca", nca_id.to_lowercase());
        if nca_id != hex::encode_upper(&ce.hash[..16]) {
            ftl!("{phase} - Hash part should match NCA ID ({nca_id}).");
            ftl!("NSP Validation failed.");
            return Ok(1);
        }
        let ncz = ncas
            .iter()
            .filter(|n| n.nca_id.eq_ignore_ascii_case(&nca_id))
            .find_map(|n| n.ncz.as_ref());
        let mut compressed = false;
        let size = match (raw_entries.iter().find(|r| r.name == file_name), ncz) {
            (Some(r), _) => Some(r.size),
            (None, _) if ce.content_type == cnmt::ContentEntryType::DELTA_FRAGMENT => {
                delta_count += 1;
                None
            }
            (None, Some(z)) => {
                compressed = true;
                // The size it decodes to, which its own header states.
                Some(z.size()?)
            }
            (None, None) => {
                errors.add(format!(
                    "{phase} - NSP file-system is missing content file : {file_name}"
                ));
                can_proceed = false;
                None
            }
        };
        if size.is_some_and(|s| s != ce.size) {
            errors.add(format!(
                "{phase} - NSP file-system contains files with sizes that do not match the CNMT."
            ));
            can_proceed = false;
        }
        if content_files.iter().any(|c| c.file_name == file_name) {
            bail!("An item with the same key has already been added. Key: {file_name}");
        }
        content_files.push(ContentFile {
            file_name,
            hash: ce.hash,
            size: ce.size,
            content_type: ce.content_type,
            compressed,
        });
    }

    if title_type == MetaType::DATA_PATCH {
        wrn!("DataPatch detected. This is a pure DLC update. This has not been spotted before!");
    }

    let is_update = matches!(title_type, MetaType::PATCH | MetaType::DATA_PATCH);
    vrb!(
        "{} <- {}{}",
        olive("NSP Type"),
        display_type(title_type),
        if is_update {
            format!(" ({title_version})")
        } else {
            String::new()
        }
    );

    let Some((_, control)) = titles.first() else {
        bail!("Sequence contains no elements (no CNMT content found in NSP)");
    };

    phase = olive("Validate Main NCA");
    let Some(main_idx) = main_nca else {
        ftl!("{phase} - Failed to open Main NCA.");
        ftl!("NSP Validation failed.");
        return Ok(1);
    };
    let main = &ncas[main_idx].nca;
    let rights_id_display = if main.rights_id().is_zero() {
        "EMPTY".into()
    } else {
        main.rights_id().to_string()
    };
    let key_gen = main.key_generation();
    let has_title_key_crypto = !main.rights_id().is_zero();
    if main.distribution_type() != DistributionType::DOWNLOAD {
        ftl!("{phase} - Unsupported distribution type : GameCard");
        return Ok(1);
    }

    let mut possible_dlc_unlocker = false;
    if ticket.is_some() && !has_title_key_crypto && is_dlc {
        errors.add(format!(
            "{phase} - NSP has ticket but no title key crypto. This is possibly a Homebrew DLC unlocker. Conversion would lose the ticket + cert."
        ));
        possible_dlc_unlocker = true;
        can_proceed = false;
    }
    if ticket.is_none() && has_title_key_crypto {
        errors.add(format!(
            "{phase} - NSP is TitleKey encrypted but no valid ticket found."
        ));
        can_proceed = false;
    }

    let mut title_key_encrypted: Option<[u8; 16]> = None;
    let mut title_key_decrypted: Option<[u8; 16]> = None;
    let mut is_ticket_signature_valid = false;
    let mut is_normalised_signature = false;
    let mut master_key_rev = 0u8;

    if let (true, Some(t)) = (has_title_key_crypto, &ticket) {
        phase = olive("Validate Ticket");
        if t.signature_type != SignatureType::RSA2048_SHA256 {
            ftl!(
                "{phase} - Unsupported ticket signature type {:#X}",
                t.signature_type.0
            );
            return Ok(1);
        }
        // Each failed check needs a regenerated ticket.
        let is_old_ticket_crypto = key_gen.0 < 3;
        let checks = [
            (
                t.sect_header_offset != 0x2C0,
                "Section Records Offset is incorrect.".to_string(),
            ),
            (
                t.license_type != LicenseType::PERMANENT,
                "Incorrect license-type found.".into(),
            ),
            (
                t.title_key_type != TitleKeyType::COMMON,
                "Personal ticket type found.".into(),
            ),
            (
                t.ticket_id != 0,
                format!("Ticket has ticket ID set ({}).", t.ticket_id),
            ),
            (
                t.property_mask != 0,
                format!(
                    "Ticket has property mask set ({}).",
                    property_flags(t.property_mask)
                ),
            ),
            (
                t.account_id != 0,
                format!("Ticket has account ID set ({})", t.account_id),
            ),
            (
                t.device_id != 0,
                format!("Ticket has device ID set ({})", t.device_id),
            ),
            (
                !is_old_ticket_crypto && t.crypto_type != t.rights_id.key_generation(),
                format!(
                    "Ticket has mis-matched crypto settings ({} vs {})",
                    t.crypto_type,
                    t.rights_id.key_generation()
                ),
            ),
            (
                is_old_ticket_crypto && t.crypto_type.0 != 0,
                "Ticket crypto should be set to zero (Keygen < 3)".into(),
            ),
        ];
        for (failed, msg) in checks {
            if failed {
                errors.add(format!("{phase} - {msg}"));
                generate_new_ticket = true;
            }
        }

        title_key_encrypted = title_keys.get(&t.rights_id).copied();
        match main.decrypted_title_key(&title_keys) {
            Ok(k) => title_key_decrypted = Some(k),
            Err(libhac::Error::MissingTitleKey(name)) => {
                crate::err!("{phase} - Failed to decrypt title key. Missing NCA title key.");
                let ticket_rights = t.rights_id.to_string();
                if !name.eq_ignore_ascii_case(&ticket_rights) {
                    crate::err!(
                        "{phase} - The ticket in this NSP does not match the title key of the main NCA {}",
                        paren(format_args!("{name}/{ticket_rights}"))
                    );
                }
                ftl!("NSP Validation failed.");
                return Ok(1);
            }
            Err(e) => return Err(e.into()),
        }

        is_normalised_signature =
            t.signature.iter().all(|&b| b == 0xFF) && t.signature.len() == 0x100;
        if matches!(title_type, MetaType::APPLICATION | MetaType::ADD_ON_CONTENT) {
            is_ticket_signature_valid = true;
            if !is_normalised_signature {
                errors.add(format!("{phase} - Ticket signature is not normalised."));
                generate_new_ticket = true;
            }
        } else {
            let Some(cert) = settings.cert.get() else {
                ftl!("{phase} - {NO_CERT}");
                return Ok(1);
            };
            is_ticket_signature_valid = t.verify_signature(cert);
        }
        if !is_ticket_signature_valid {
            errors.add(format!("{phase} - Ticket signature is invalid."));
            can_proceed = false;
        }
        master_key_rev = key_gen.master_key_revision();
    }

    // DISPLAY TITLE LOOKUP
    let mut langs: Vec<Lang> = Vec::new();
    let mut control_title: Option<String> = None;
    for (i, t) in control.titles().into_iter().enumerate() {
        if let Some(name) = t {
            langs.push(Lang::ALL[i]);
            control_title.get_or_insert(name);
        }
    }
    let (mut display_title, mut source) = match control_title {
        Some(t) => (t, LookupSource::Control),
        None => (
            file_name.replace('_', " ").replace(".nsp", ""),
            LookupSource::FileName,
        ),
    };
    if settings.keep_name {
        if file_name.contains('(') {
            display_title = file_name.split('(').next().unwrap().trim().to_string();
            source = LookupSource::FileTitle;
        } else if file_name.contains('[') {
            display_title = file_name.split('[').next().unwrap().trim().to_string();
            source = LookupSource::FileTitle;
        } else {
            display_title = file_name.replace(".nsp", "");
            source = LookupSource::FileName;
        }
    }

    let mut display_parent_title: Option<String> = None;
    let mut distribution_region = String::new();
    let mut release_date: Option<String> = None;
    if let Some(db) = &titledb {
        let lookup_id = if title_id != base_title_id {
            &base_title_id
        } else {
            &title_id
        };
        if settings.verify_title || source == LookupSource::FileName {
            let title_db_title = if is_dlc {
                display_parent_title = db
                    .look_up_title(&base_title_id)?
                    .map(|t| naming::remove_brackets(&naming::clean_title(&t)));
                db.look_up_title(&title_id)?
                    .map(|t| naming::clean_title(&t))
            } else {
                db.look_up_title(lookup_id)?
            };
            if let Some(mut t) = title_db_title.filter(|t| !settings.keep_name && !t.is_empty()) {
                let mut src = LookupSource::TitleDb;
                if source == LookupSource::Control {
                    // Prefer the control title when the TitleDB one is not English
                    // (first UTF-16 unit above 'z').
                    let first = |s: &str| s.encode_utf16().next().unwrap_or(0);
                    if (first(&t) > 122 && first(&display_title) < 123)
                        || naming::eq_ignore_case(&t, &display_title)
                    {
                        t = display_title.clone();
                        src = LookupSource::Control;
                    }
                }
                display_title = naming::remove_brackets(&t);
                source = src;
            }
        }
        release_date = db.look_up_release_date(lookup_id)?;
        let region = db.look_up_region(lookup_id)?;
        if region != "UNKNOWN" {
            distribution_region = region;
        }
    }

    if source == LookupSource::FileName && is_dlc && file_name.contains('[') {
        let parts: Vec<&str> = file_name
            .split('[')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        if parts.len() > 1 {
            display_title = parts[0].to_string();
            // .NET char.IsDigit: Unicode decimal digits (Nd) only.
            static DIGIT: std::sync::LazyLock<regex::Regex> =
                std::sync::LazyLock::new(|| regex::Regex::new(r"^\p{Nd}").unwrap());
            if !DIGIT.is_match(parts[1]) && parts.len() > 2 {
                display_title += " - ";
                display_title += parts[1].replace(']', "").replace("dlc", "DLC").trim();
            }
        }
        if display_title.contains('(') && display_title.contains(')') {
            static PAREN: std::sync::LazyLock<regex::Regex> =
                std::sync::LazyLock::new(|| regex::Regex::new(r"\((.*?)\)").unwrap());
            if let Some(m) = PAREN.find(&display_title) {
                let m = m.as_str().to_string();
                display_title = display_title
                    .replace(&m, "")
                    .replace("  ", " ")
                    .trim()
                    .to_string();
            }
        }
    }

    let mut parent_languages: Vec<Lang> = Vec::new();
    let mut display_parent_languages = "UNKNOWN".to_string();
    if is_dlc && let Some(db) = &titledb {
        let languages = db.look_up_languages(&base_title_id)?;
        if !languages.is_empty() {
            let mut distinct: Vec<&str> = Vec::new();
            for l in &languages {
                if !distinct.contains(&l.as_str()) {
                    distinct.push(l);
                }
            }
            parent_languages = distinct
                .iter()
                .map(|l| match *l {
                    "ja" => Lang::Japanese,
                    "fr" => Lang::French,
                    "de" => Lang::German,
                    "it" => Lang::Italian,
                    "es" => Lang::Spanish,
                    "zh" => Lang::SimplifiedChinese,
                    "ko" => Lang::Korean,
                    "nl" => Lang::Dutch,
                    "pt" => Lang::Portuguese,
                    "ru" => Lang::Russian,
                    _ => Lang::AmericanEnglish,
                })
                .collect();
            display_parent_languages = languages
                .iter()
                .map(|l| {
                    let mut c = l.chars();
                    c.next()
                        .map(|f| f.to_uppercase().chain(c).collect::<String>())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(",");
        }
    }

    let mut display_version = "UNKNOWN".to_string();
    let dv = control.display_version();
    if !dv.is_empty() {
        display_version = dv.trim().to_string();
        if display_version.starts_with(['v', 'V', 'b']) {
            display_version = display_version[1..].to_string();
        }
    }
    let is_demo = control.is_demo();
    let is_retail_display = control.is_retail_display();

    let output_name = naming::build_output_name(&NameInputs {
        language_mode: settings.language_mode,
        titles: &langs,
        display_title: &display_title,
        display_version: &display_version,
        is_patch: title_type == MetaType::PATCH,
        title_version: &title_version,
        display_type_short: display_type_short(title_type),
        title_id: &title_id,
        is_demo,
        is_dlc,
        display_parent_title: display_parent_title.as_deref(),
        parent_languages: &parent_languages,
        display_parent_languages: &display_parent_languages,
        distribution_region: &distribution_region,
        keep_name: settings.keep_name,
    });

    let output_file = format!("{output_name}.{}", if settings.nsz { "nsz" } else { "nsp" });
    if settings.convert && !naming::is_plain_file_name(&output_file) {
        ftl!("Invalid output file name ({output_file}).");
        return Ok(1);
    }
    let target_name = settings.nsp_dir.join(&output_file);
    if settings.convert && target_name.exists() && !settings.overwrite {
        ftl!(
            "File already exists. ({}). Use {} to overwrite an existing file.",
            target_name.display(),
            grey("--overwrite")
        );
        return Ok(2);
    }

    // VALIDATE NCAS
    phase = olive("Validate NCAs");
    if ncas.is_empty() {
        ftl!("{phase} - No NCAs found.");
        ftl!("NSP Validation failed.");
        return Ok(1);
    }

    let plan = plan_content(&content_files, &ncas, settings.keep_deltas);
    let meta_idx = meta_nca.expect("CNMT was read from a meta NCA");
    let is_file_order_correct = is_order_correct(&raw_entries);

    // Everything that decides whether the NSP gets converted, except the
    // NCA checks below. When conversion is already likely, write the output
    // now so each NCA is read once, for both its hash and the copy.
    // Failed checks later drop the partial file.
    let too_long = path_too_long(&target_name, 254);
    let signed: Vec<bool> = ncas
        .iter()
        .map(|n| n.nca.verify_header_signature())
        .collect();
    let would_convert = settings.convert
        && !settings.dry_run
        && can_proceed
        && !(settings.exit_on_error && !errors.is_empty())
        && (settings.force_convert
            || settings.nsz
            || generate_new_ticket
            || has_loose_files
            || !is_file_order_correct
            || copy_new_cert
            || bad_padding)
        && !plan
            .iter()
            .any(|p| matches!(p, Planned::Missing(_) | Planned::MissingNcz(_)))
        && too_long.is_none()
        && !(target_name == nsp_path || is_same_file(&target_name, nsp_path))
        // The sizes of what gets written were checked against the CNMT,
        // which counts only when it comes from a signed meta NCA.
        && signed.iter().all(|&s| s);
    let mut pending = None;
    if would_convert {
        let order = output_order(&plan, meta_idx);
        // On failure the normal write path below retries and reports the error.
        if let Some(tail) = tail_files(
            ticket.as_ref().filter(|_| has_title_key_crypto),
            settings.cert.get(),
        ) {
            pending = PartialNsp::start(
                &target_name,
                keys,
                &ncas,
                &order,
                &tail,
                settings.nsz.then_some(&title_keys),
            )
            .ok();
        }
    }

    let has_compressed_nca = ncas.iter().any(|n| n.ncz.is_some());
    let meta_signed = signed[meta_idx];
    let mut nca_files: Vec<NcaInfo> = Vec::new();
    for (fs_nca, &header_valid) in ncas.iter().zip(&signed) {
        let nca = &fs_nca.nca;
        let name = &fs_nca.file_name;
        // The CNMT lists an NCZ under the name of its NCA, in lower case:
        // the NCZ was matched to its CNMT entry in any case above.
        let listed = match fs_nca.ncz {
            Some(_) => format!("{}.nca", fs_nca.ncz_target_hash().to_lowercase()),
            None => name.clone(),
        };
        let listed = content_files.iter().find(|c| c.file_name == listed);
        // The sections of an NCZ open only on the NCA it decompresses to.
        // Decompressing takes time by the size the NCZ states, so it is
        // done only when that is the size in a CNMT from a signed meta NCA:
        // every other case is reported as an error of its own. Nor is it
        // done with `--skip-hash`, which is there to save that time.
        let size_listed = |z: &NczReader<_>| listed.is_some_and(|c| z.size().ok() == Some(c.size));
        let written = pending.as_ref().and_then(|p| p.hashes.get(name));
        let unpack = fs_nca
            .ncz
            .as_ref()
            .filter(|z| !settings.skip_hash && meta_signed && size_listed(z));
        // A conversion to NSP has decompressed it already: its copy in the
        // output is read, not the NCZ a second time.
        let copy = unpack.and(pending.as_ref()).and_then(|p| p.copy_of(name));
        let unpacked = match (unpack, &copy) {
            (Some(z), None) => Some(Unpacked::new(z, written.is_none())?),
            _ => None,
        };
        let plain = match (&copy, &unpacked) {
            (Some(c), _) => Some(Nca::open(keys, Box::new(c) as Box<dyn ReadAt + '_>)?),
            (None, Some(u)) => Some(Nca::open(keys, Box::new(u) as Box<dyn ReadAt + '_>)?),
            (None, None) => None,
        };
        let readable = fs_nca.ncz.is_none() || plain.is_some();
        let mut info = NcaInfo {
            header_valid,
            sections_errored: false,
            hash_match: HashMatch::Missing,
        };
        // The sections are checked from the last to the first, and the NPDM
        // (in section 0) after them: that is their order in an NCA, where
        // the ExeFS comes last, and a solid NCZ is passed front to back.
        let mut failed = Vec::new();
        let sections: Vec<_> = SectionIndex::all().collect();
        for &i in sections.iter().rev() {
            if !nca.can_open_section(i, &title_keys) {
                continue;
            }
            if nca.fs_header(i).is_patch_section() {
                continue;
            }
            if !readable {
                // The sections of an NCZ that stays compressed cannot
                // open. With `--skip-hash` that is what was asked for.
                info.sections_errored = !settings.skip_hash && !has_sparse_ncas;
                continue;
            }
            let opened = match &plain {
                Some(plain) => check_section_fs(plain, i, &title_keys),
                None => check_section_fs(nca, i, &title_keys),
            };
            if opened.is_err() && !has_sparse_ncas {
                info.sections_errored = true;
                failed.push(i);
            }
        }
        let npdm_validity = match &plain {
            _ if has_sparse_ncas => None,
            Some(plain) => verify_npdm(plain, &title_keys),
            None => verify_npdm(nca, &title_keys),
        };

        if !info.header_valid {
            errors.add(format!("{phase} - {name} <- Header signature is invalid."));
            can_proceed = false;
        }
        let ct = nca.content_type();
        match npdm_validity {
            // verify_npdm returns Some only for Program NCAs.
            Some(false) => {
                errors.add(format!("{phase} - {name} <- NPDM signature is invalid."));
                can_proceed = false;
            }
            None if ct == ContentType::PROGRAM && !has_sparse_ncas && readable => {
                errors.add(format!("{phase} - {name} <- Error opening NPDM."));
            }
            _ => {}
        }
        for i in failed.into_iter().rev() {
            let type_name = if ct == ContentType::DATA {
                "Delta"
            } else {
                ct.name().unwrap_or("Unknown")
            };
            errors.add(format!(
                "{phase} - {name} ({type_name}) (Section {i}) <- Error opening file-system"
            ));
            can_proceed = false;
        }

        if settings.skip_hash {
            warnings.add(format!("{phase} - Skipped hash check on : {name}."));
            nca_files.push(info);
            continue;
        }

        let headers_valid = nca_files.iter().all(|n| n.header_valid);
        // The sections of an NCZ that stays compressed do not tell whether
        // its hash is worth a check.
        let hashable = if readable {
            !info.is_errored()
        } else {
            info.header_valid && meta_signed
        };
        if settings.force_hash || (hashable && headers_valid) {
            if settings.force_hash {
                wrn!("{phase} - Forcing hash check.");
            }
            let raw = raw_entries
                .iter()
                .find(|r| &r.name == name)
                .expect("NCA is a raw entry");
            if raw.size == 0 {
                continue;
            }
            // A compressed meta NCA has no entry in the CNMT, like any other
            // unlisted file.
            let is_meta = ct == ContentType::META && fs_nca.ncz.is_none();
            if !is_meta && listed.is_none() {
                bail!("The given key '{name}' was not present in the dictionary.");
            }
            let nca_hash = match (written, &fs_nca.ncz) {
                (Some(h), _) => h.clone(),
                (None, None) => hash_range(&whole, raw.offset, raw.size)?,
                // A size that is not the one in the CNMT was reported with
                // the CNMT, and no hash can match it.
                (None, Some(z)) if !size_listed(z) => {
                    nca_files.push(info);
                    continue;
                }
                // What counts is the NCA that the NCZ decompresses to.
                (None, Some(z)) => {
                    match unpacked.as_ref().map_or_else(|| hash_ncz(z), |u| u.hash()) {
                        Ok(h) => h,
                        Err(e) => {
                            errors.add(format!("{phase} - {name} <- Error decompressing NCZ: {e}"));
                            can_proceed = false;
                            nca_files.push(info);
                            continue;
                        }
                    }
                }
            };
            let expected: String;
            if is_meta {
                // Cut by characters: a byte cut can split a non-ASCII name and panic.
                expected = name
                    .chars()
                    .take(name.chars().count().saturating_sub(9))
                    .collect();
                info.hash_match = if nca_hash
                    .to_lowercase()
                    .starts_with(&expected.to_lowercase())
                {
                    HashMatch::Match
                } else {
                    HashMatch::Mismatch
                };
            } else {
                let cf = listed.expect("checked above");
                expected = hex::encode_upper(cf.hash);
                info.hash_match = if nca_hash.eq_ignore_ascii_case(&expected) {
                    HashMatch::Match
                } else {
                    HashMatch::Mismatch
                };
            }
            if info.hash_match == HashMatch::Mismatch {
                errors.add(format!(
                    "{phase} - Hash mismatch for NCA {name} - Expected {expected}, got {nca_hash}"
                ));
                can_proceed = false;
            }
        }
        nca_files.push(info);
    }

    if !is_file_order_correct {
        errors.add(format!(
            "{} <- {}",
            olive("NCA File Order"),
            red("Non-standard")
        ));
    }

    let header_signature_valid = nca_files.iter().all(|n| n.header_valid);
    let is_standard_nsp = can_proceed
        && !generate_new_ticket
        && !has_loose_files
        && is_file_order_correct
        && !copy_new_cert
        && !bad_padding;

    if settings.skip_hash && !settings.rename {
        inf!(
            "{} <- {}",
            olive("Validation Complete"),
            olive("Unknown - Hashing skipped.")
        );
    } else if errors.is_empty()
        && header_signature_valid
        && !copy_new_cert
        && nca_files.iter().all(|n| !n.is_errored())
        && nca_files
            .iter()
            .all(|n| n.hash_match != HashMatch::Mismatch)
    {
        inf!(
            "{} <- {}",
            olive("Validation Complete"),
            green("No problems found")
        );
        inf!(
            "{} <- {}",
            olive("Is Standard NSP? "),
            if is_standard_nsp {
                green("Yes")
            } else {
                red("No")
            }
        );
    } else {
        inf!(
            "{} <- {}",
            olive("Validation Complete"),
            red("Problems found")
        );
    }
    for w in &warnings.0 {
        wrn!("{w}");
    }
    for e in &errors.0 {
        crate::err!("{e}");
    }

    if !crate::log::is_quiet() {
        // Colored "Yes" for a set flag, plain "No" otherwise.
        let flag =
            |b: bool, paint: fn(&'static str) -> String| if b { paint("Yes") } else { "No".into() };
        let valid = |b: bool| if b { green("Valid") } else { red("Invalid") };
        let pass = |ok: bool, good: &str, bad: &str| if ok { green(good) } else { red(bad) };
        let mut rows: Vec<(&str, String)> = vec![(
            "Title",
            format!("{} (From {source:?})", olive(&display_title)),
        )];
        if let Some(p) = display_parent_title
            .as_deref()
            .filter(|p| is_dlc && !p.is_empty())
        {
            rows.push(("Parent Title", format!("{} (From {source:?})", olive(p))));
        }
        if !langs.is_empty() {
            rows.push((
                "Languages",
                langs.iter().map(|l| l.full()).collect::<Vec<_>>().join(","),
            ));
            rows.push((
                "Languages (Short)",
                naming::languages_short(&langs).join(","),
            ));
        }
        if !distribution_region.trim().is_empty() {
            rows.push(("Distribution Region", distribution_region.clone()));
        }
        if is_dlc && display_parent_languages != "UNKNOWN" {
            rows.push((
                "Parent Languages",
                format!("{display_parent_languages} (From TitleDb)"),
            ));
        }
        rows.push(("Languages Output", format!("{:?}", settings.language_mode)));
        rows.push(("Title ID", title_id.clone()));
        if title_id != base_title_id {
            rows.push(("Base Title ID", base_title_id.clone()));
        }
        rows.push(("Title Type", display_type(title_type).into()));
        rows.push((
            "Title Version",
            if title_version == "v0" {
                "BASE (v0)".into()
            } else {
                title_version.clone()
            },
        ));
        rows.push(("Is Interactive Display ?", flag(is_retail_display, olive)));
        rows.push(("Is Demo ?", flag(is_demo, olive)));
        rows.push(("Is DLC Unlocker ?", flag(possible_dlc_unlocker, red)));
        if let Some(d) = release_date
            .as_ref()
            .filter(|_| title_type == MetaType::APPLICATION)
        {
            rows.push(("Release Date", d.clone()));
        }
        rows.push(("Has Sparse NCAs ?", flag(has_sparse_ncas, olive)));
        rows.push(("Is Main NCA Compressed ?", flag(has_compressed_nca, blue)));
        rows.push(("Key Generation", key_gen.to_string()));
        rows.push(("NSP Version", display_version.clone()));
        rows.push(("Rights ID", rights_id_display.clone()));
        rows.push(("Header Validity", valid(header_signature_valid)));
        rows.push((
            "NCA Validity",
            valid(nca_files.iter().all(|n| !n.is_errored())),
        ));
        let nca_count = raw_entries
            .iter()
            .filter(|r| r.name.ends_with(".nca"))
            .count();
        let deltas = if delta_count > 0 {
            format!(" + {delta_count} Missing Deltas")
        } else {
            String::new()
        };
        rows.push((
            "Raw File Count",
            format!("{} ({nca_count} NCAs{deltas})", raw_entries.len()),
        ));
        rows.push(("Has loose files ?", pass(!has_loose_files, "No", "Yes")));
        rows.push((
            "NCA File Order",
            pass(is_file_order_correct, "Correct", "Non-Standard"),
        ));
        rows.push(("File Padding", pass(!bad_padding, "Correct", "Incorrect")));
        if let (Some(enc), Some(dec)) = (title_key_encrypted, title_key_decrypted) {
            rows.push(("TitleKey (Enc)", hex::encode_upper(enc)));
            rows.push(("TitleKey (Dec)", hex::encode_upper(dec)));
            rows.push((
                "Ticket Signature",
                if is_normalised_signature && title_type == MetaType::APPLICATION {
                    green("Normalised")
                } else {
                    valid(is_ticket_signature_valid)
                },
            ));
            rows.push((
                "Ticket Properties",
                pass(!generate_new_ticket, "Passed", "Non-Standard"),
            ));
            rows.push(("MasterKey Revision", master_key_rev.to_string()));
        }
        rows.push(("Output Name", olive(&output_name)));
        let width = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
        crate::log::line("");
        for (k, v) in rows {
            crate::log::line(&format!("  {k:<width$} : {v}"));
        }
        crate::log::line("");
    }

    if !errors.is_empty() && settings.exit_on_error {
        return Ok(1);
    }
    if !can_proceed {
        ftl!("NSP Validation failed.");
        return Ok(1);
    }
    if settings.rename {
        // Windows cannot rename a file that is still open.
        drop(ncas);
        drop(file);
        return Ok(rename(settings, nsp_path, &file_name, &output_name));
    }
    if !settings.convert {
        return Ok(0);
    }

    if let Some(t) = &ticket
        && has_title_key_crypto
        && (!is_ticket_signature_valid || generate_new_ticket)
    {
        let mut signature = vec![0xFFu8; 0x100];
        if is_ticket_signature_valid && is_update {
            signature = t.signature.clone();
        }
        if !is_ticket_signature_valid && is_update {
            crate::err!("Unable to update ticket for an update where ticket signature is invalid.");
        } else {
            let enc = title_key_encrypted.ok_or_else(|| anyhow!("Missing encrypted title key"))?;
            ticket = Some(Ticket::create(master_key_rev, t.rights_id, &enc, signature));
            inf!("Generated new normalised ticket.");
        }
    }

    // CONVERT
    if is_standard_nsp && !settings.force_convert && !settings.nsz {
        inf!("File is already in Standard NSP format. Skipping conversion.");
        return Ok(0);
    }
    if settings.dry_run {
        inf!("{} -> Would create: {}", tag("DRYRUN"), olive(&output_file));
    }

    for p in &plan {
        match p {
            Planned::SkipDelta(name) => inf!(
                "{} -> Skipping delta fragment: {}",
                tag("SKIP"),
                olive(name)
            ),
            Planned::Missing(name) => {
                crate::err!(
                    "Failed to locate NCA file {} in the NSP file-system.",
                    olive(name)
                );
                return Ok(1);
            }
            Planned::MissingNcz(name) => {
                crate::err!(
                    "Failed to locate NCZ file {} in the NSP file-system.",
                    olive(name)
                );
                return Ok(1);
            }
            Planned::DeltaNotFound(name) => {
                inf!(
                    "{} -> Delta fragment {} not found in the NSP file-system. Skipping.",
                    tag("SKIP"),
                    olive(name)
                )
            }
            Planned::Add(i) => {
                if settings.dry_run {
                    inf!(
                        "{} -> Would add: {}",
                        tag("DRYRUN"),
                        olive(&ncas[*i].file_name)
                    );
                }
            }
        }
    }
    if settings.dry_run {
        inf!(
            "{} -> Would add: {}",
            tag("DRYRUN"),
            olive(&ncas[meta_idx].file_name)
        );
    }

    let Some(tail) = tail_files(
        ticket.as_ref().filter(|_| has_title_key_crypto),
        settings.cert.get(),
    ) else {
        ftl!("{phase} - {NO_CERT}");
        return Ok(1);
    };
    if settings.dry_run {
        for (name, _) in &tail {
            inf!("{} -> Would add: {}", tag("DRYRUN"), olive(name));
        }
    }

    if let Some(msg) = too_long {
        crate::err!("{msg}");
        return Ok(1);
    }
    if target_name == nsp_path || is_same_file(&target_name, nsp_path) {
        crate::err!("Trying to save converted file to the same location as the input file.");
        return Ok(1);
    }
    if target_name.exists() && !settings.overwrite {
        crate::err!(
            "File already exists. ({}). Use {} to overwrite an existing file.",
            target_name.display(),
            grey("--overwrite")
        );
        return Ok(2);
    }
    if settings.dry_run {
        if settings.delete_source {
            inf!("{} -> Would delete: {}", tag("DRYRUN"), olive(&file_name));
        }
        return Ok(0);
    }

    let kind = if settings.nsz { "NSZ" } else { "NSP" };
    inf!("Building Standard {kind} => {}", olive(&output_name));
    let result = match pending {
        Some(p) => p.finish(&target_name, &tail),
        None => PartialNsp::start(
            &target_name,
            keys,
            &ncas,
            &output_order(&plan, meta_idx),
            &tail,
            settings.nsz.then_some(&title_keys),
        )
        .and_then(|p| p.finish(&target_name, &tail)),
    };
    if let Err(e) = result {
        crate::err!("Failed to convert file. {e}");
        return Ok(1);
    }
    inf!("Converted: {}", olive(&output_file));

    if settings.delete_source {
        drop(file);
        if let Err(e) = std::fs::remove_file(nsp_path) {
            crate::err!("Failed to delete file. {e}");
            return Ok(1);
        }
        inf!("Cleaned  : {}", olive(&file_name));
    }

    Ok(0)
}

/// .NET `[Flags]` enum formatting of `FixedPropertyFlags`.
fn property_flags(mask: u32) -> String {
    const NAMES: [&str; 6] = [
        "PreInstall",
        "SharedTitle",
        "AllowAllContent",
        "DeviceLinkIndependent",
        "Volatile",
        "ELicenseRequired",
    ];
    if mask & !0x3F != 0 {
        return mask.to_string();
    }
    let names: Vec<&str> = (0..6)
        .filter(|b| mask & (1 << b) != 0)
        .map(|b| NAMES[b])
        .collect();
    names.join(", ")
}

/// Port of `ValidateNspService.ImportTicket`.
fn import_ticket(
    t: Ticket,
    keys: &Keys,
    title_keys: &mut TitleKeys,
    current: &mut Option<Ticket>,
) -> Result<()> {
    let phase = olive("Import tickets");
    if t.rights_id.is_zero() {
        wrn!("{phase} - Empty Rights ID. Skipping");
        return Ok(());
    }
    let Some(key) = t.title_key(keys) else {
        wrn!("{phase} - TitleKey not found. Skipping");
        return Ok(());
    };
    match title_keys.get(&t.rights_id) {
        Some(existing) if *existing != key => bail!("External key already registered"),
        _ => {
            title_keys.insert(t.rights_id, key);
        }
    }
    if current.is_some() {
        wrn!("{phase} - Multiple tickets found. Using first.");
        return Ok(());
    }
    *current = Some(t);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_keeps_extension() {
        assert_eq!(rename_file_name("x.nsp", "Game [01]"), "Game [01].nsp");
        assert_eq!(rename_file_name("x.nsz", "Game [01]"), "Game [01].nsz");
        assert_eq!(rename_file_name("x.NSZ", "Game [01]"), "Game [01].nsz");
        assert_eq!(rename_file_name("x.nsx", "Game [01]"), "Game [01].nsp");
        // Already named: rename() compares this with the source name.
        assert_eq!(
            rename_file_name("Game [01].nsp", "Game [01]"),
            "Game [01].nsp"
        );
    }

    #[test]
    fn swapped_common_cert_rejects_other_data() {
        let mut x: u32 = 0x1234_5678;
        let data: Vec<u8> = (0..COMMON_CERT_SIZE)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        assert_eq!(swapped_common_cert(&data), None);
        assert_eq!(swapped_common_cert(&data[..0x6FF]), None);
        assert_eq!(swapped_common_cert(&[]), None);
    }
}
