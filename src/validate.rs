// SPDX-FileCopyrightText: 2024 mRg
// SPDX-FileCopyrightText: 2018-2022 Alex Barney
// SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>
//
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Port of `ValidateNspService.Process`: validate an NSP and optionally
//! rewrite it as a Standard NSP.

use crate::naming::{self, Lang, LanguageMode, NameInputs};
use crate::titledb::TitleDb;
use crate::{ftl, inf, vrb, wrn};
use anyhow::{Result, anyhow, bail};
use indicatif::{ProgressBar, ProgressStyle};
use libhac::cnmt::{self, Cnmt, ContentAttributes, MetaType};
use libhac::keys::Keys;
use libhac::nacp::Nacp;
use libhac::nca::{ContentType, DistributionType, FsType, Nca, SectionIndex, TitleKeys};
use libhac::ncz::NczReader;
use libhac::partition::PartitionFs;
use libhac::pfs0::Pfs0Builder;
use libhac::romfs::RomFs;
use libhac::ticket::{LicenseType, SignatureType, Ticket, TitleKeyType};
use libhac::{ReadAt, SubStorage, TitleId, npdm};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

pub const COMMON_CERT_SIZE: usize = 0x700;
pub const COMMON_CERT_SHA256: &str =
    "3c4f20dca231655e90c75b3e9689e4dd38135401029ab1f2ea32d1c2573f1dfe";

pub fn is_common_cert(data: &[u8]) -> bool {
    hex::encode(Sha256::digest(data)) == COMMON_CERT_SHA256
}

pub struct Settings {
    pub cert: Vec<u8>,
    pub convert: bool,
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
    let pb = if crate::log::is_quiet() {
        ProgressBar::hidden()
    } else {
        ProgressBar::new(total)
    };
    let template = format!("{label} .. {{bar:40}} {{percent:>3}}% {{eta}}");
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
fn tail_files(ticket: Option<&Ticket>, cert: &[u8]) -> Vec<(String, Vec<u8>)> {
    let Some(t) = ticket else {
        return Vec::new();
    };
    let rid = hex::encode(t.rights_id.0);
    vec![
        (format!("{rid}.tik"), t.to_bytes()),
        (format!("{rid}.cert"), cert.to_vec()),
    ]
}

/// A Standard NSP being written next to its target as `<target>.part`.
/// The NCAs are copied and hashed in one pass; the ticket and certificate,
/// which may still be regenerated, are appended by `finish`. Dropping it
/// before `finish` deletes the partial file.
struct PartialNsp {
    tmp: PathBuf,
    out: Option<BufWriter<File>>,
    tail_sizes: Vec<u64>,
    /// Upper-case hex SHA256 of each written NCA, by file name.
    hashes: HashMap<String, String>,
}

impl PartialNsp {
    fn start(
        target: &Path,
        src: &dyn ReadAt,
        ncas: &[FsNca],
        order: &[usize],
        tail: &[(String, Vec<u8>)],
    ) -> Result<Self> {
        let mut tmp = target.as_os_str().to_owned();
        tmp.push(".part");
        let mut p = PartialNsp {
            tmp: tmp.into(),
            out: None,
            tail_sizes: tail.iter().map(|t| t.1.len() as u64).collect(),
            hashes: HashMap::new(),
        };
        let mut layout = Pfs0Builder::new();
        let mut total = 0;
        for &i in order {
            let (name, size) = ncas[i].output()?;
            layout.add_file(name.as_str(), size);
            total += size;
        }
        for (n, b) in tail {
            layout.add_file(n.as_str(), b.len() as u64);
        }

        let out = p
            .out
            .insert(BufWriter::with_capacity(0x100000, File::create(&p.tmp)?));
        out.write_all(&layout.to_bytes())?;
        let pb = progress_bar("Writing", total);
        for &i in order {
            let mut sha = Sha256::new();
            let mut copy = |chunk: &[u8]| {
                sha.update(chunk);
                out.write_all(chunk)?;
                pb.inc(chunk.len() as u64);
                Ok(())
            };
            if let Some(z) = &ncas[i].ncz {
                z.read_chunks(0, z.size()?, &mut copy)?;
                let current = hex::encode_upper(&sha.finalize()[..16]);
                let target = ncas[i].ncz_target_hash().to_uppercase();
                if current != target {
                    bail!(
                        "Current hash ({current}) does not match original hash ({target}). File is corrupted."
                    );
                }
                continue;
            }
            let r = ncas[i].nca.storage();
            src.read_chunks(r.offset(), r.size()?, &mut copy)?;
            p.hashes
                .insert(ncas[i].file_name.clone(), hex::encode_upper(sha.finalize()));
        }
        pb.finish_and_clear();
        Ok(p)
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
        for (_, bytes) in tail {
            out.write_all(bytes)?;
        }
        out.into_inner().map_err(|e| e.into_error())?;
        std::fs::rename(&self.tmp, target)?;
        Ok(())
    }
}

impl Drop for PartialNsp {
    fn drop(&mut self) {
        // After a successful rename the file is gone and this fails harmlessly.
        let _ = std::fs::remove_file(&self.tmp);
    }
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
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
        ftl!("File not found: {}", nsp_path.display());
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

    inf!("Validating NSP : {file_name}");
    if settings.convert {
        let mut extra = String::new();
        if settings.dry_run {
            extra = " (Dry Run)".into();
        }
        if settings.force_convert {
            extra = " (Force)".into();
        }
        if settings.delete_source {
            extra += " (Delete Source)";
        }
        inf!("Output Mode <- CONVERT{extra}");
    }

    let titledb = if settings.titledb.is_file() {
        let db = TitleDb::open(&settings.titledb)?;
        if settings.verify_title {
            vrb!("TitleDB <- Found ({})", settings.titledb.display());
        }
        Some(db)
    } else {
        if settings.verify_title {
            wrn!("TitleDB <- Not Found ({})", settings.titledb.display());
        }
        None
    };

    let mut phase = "Validate File Structure";
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
            "{phase} <- NSP file-header is not aligned correctly. ({header_size:08X}=>{aligned_header_size:08X})"
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

    phase = "Validate File-System";

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
                let t = Ticket::parse(&range.read_vec(0, e.size as usize)?)?;
                import_ticket(t, keys, &mut title_keys, &mut ticket)?;
                vrb!("{phase} <- Ticket imported ({name})");
            }

            if name.ends_with(".cert") {
                if e.size as usize != COMMON_CERT_SIZE {
                    wrn!(
                        "{phase} <- Certificate size is incorrect. Expected 0x700 bytes. ({name})"
                    );
                    copy_new_cert = true;
                } else if !is_common_cert(&range.read_vec(0, e.size as usize)?) {
                    wrn!(
                        "{phase} <- Certificate does not match common certificate SHA256. ({name})"
                    );
                    copy_new_cert = true;
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
                    warnings.add(format!("{phase} <- Compressed NCA (NCZ) detected."));
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
                    })?
                    .ok_or_else(|| anyhow!("Sequence contains no elements"))?;
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

    phase = "Validate Padding";
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
            ftl!("{phase} <- Failed");
            return Ok(1);
        }
        inf!("{phase} <- Passed");
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
                titles[pos].1 = Nacp::from_bytes(&romfs.read_file(&storage, &file)?);
            }
        }
    }

    phase = "Open File-System";
    vrb!("{phase} <- Loaded correctly.");

    phase = "Validate Metadata (CNMT)";
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
        let compressed = match raw_entries.iter().find(|r| r.name == file_name) {
            None if ce.content_type == cnmt::ContentEntryType::DELTA_FRAGMENT => {
                delta_count += 1;
                false
            }
            None if ncas
                .iter()
                .any(|n| n.ncz.is_some() && n.nca_id.eq_ignore_ascii_case(&nca_id)) =>
            {
                true
            }
            None => {
                errors.add(format!(
                    "{phase} - NSP file-system is missing content file : {file_name}"
                ));
                can_proceed = false;
                false
            }
            Some(r) => {
                if r.size != ce.size {
                    errors.add(format!("{phase} - NSP file-system contains files with sizes that do not match the CNMT."));
                    can_proceed = false;
                }
                false
            }
        };
        if content_files.iter().any(|c| c.file_name == file_name) {
            bail!("An item with the same key has already been added. Key: {file_name}");
        }
        content_files.push(ContentFile {
            file_name,
            hash: ce.hash,
            content_type: ce.content_type,
            compressed,
        });
    }

    if title_type == MetaType::DATA_PATCH {
        wrn!("DataPatch detected. This is a pure DLC update. This has not been spotted before!");
    }

    let is_update = matches!(title_type, MetaType::PATCH | MetaType::DATA_PATCH);
    vrb!(
        "NSP Type <- {}{}",
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

    phase = "Validate Main NCA";
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
    if key_gen.0 > 18 {
        ftl!("Unsupported key generation ({key_gen}). Contact author to update!");
        return Ok(1);
    }
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
        phase = "Validate Ticket";
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
                        "{phase} - The ticket in this NSP does not match the title key of the main NCA ({name}/{ticket_rights})"
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
            is_ticket_signature_valid = t.verify_signature(&settings.cert);
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

    let output_file = format!("{output_name}.nsp");
    if settings.convert && !naming::is_plain_file_name(&output_file) {
        ftl!("Invalid output file name ({output_file}).");
        return Ok(1);
    }
    let target_name = settings.nsp_dir.join(output_file);
    if settings.convert && target_name.exists() && !settings.overwrite {
        ftl!(
            "File already exists. ({}). Use --overwrite to overwrite an existing file.",
            target_name.display()
        );
        return Ok(2);
    }

    // VALIDATE NCAS
    phase = "Validate NCAs";
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
    let target_str = target_name.to_string_lossy().into_owned();
    let would_convert = settings.convert
        && !settings.dry_run
        && can_proceed
        && !(settings.exit_on_error && !errors.is_empty())
        && (settings.force_convert
            || generate_new_ticket
            || has_loose_files
            || !is_file_order_correct
            || copy_new_cert
            || bad_padding)
        && !plan
            .iter()
            .any(|p| matches!(p, Planned::Missing(_) | Planned::MissingNcz(_)))
        && utf16_len(&target_str) <= 254
        && !(target_name == nsp_path || is_same_file(&target_name, nsp_path));
    let mut pending = None;
    if would_convert {
        let order = output_order(&plan, meta_idx);
        let tail = tail_files(
            ticket.as_ref().filter(|_| has_title_key_crypto),
            &settings.cert,
        );
        // On failure the normal write path below retries and reports the error.
        pending = PartialNsp::start(&target_name, &whole, &ncas, &order, &tail).ok();
    }

    let has_compressed_nca = ncas.iter().any(|n| n.ncz.is_some());
    let mut nca_files: Vec<NcaInfo> = Vec::new();
    for fs_nca in &ncas {
        let nca = &fs_nca.nca;
        let npdm_validity = if !has_sparse_ncas {
            verify_npdm(nca, &title_keys)
        } else {
            None
        };
        let mut info = NcaInfo {
            header_valid: nca.verify_header_signature(),
            sections_errored: false,
            hash_match: HashMatch::Missing,
        };
        let name = &fs_nca.file_name;

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
            None if ct == ContentType::PROGRAM && !has_sparse_ncas && !has_compressed_nca => {
                errors.add(format!("{phase} - {name} <- Error opening NPDM."));
            }
            _ => {}
        }

        for i in SectionIndex::all() {
            if !nca.can_open_section(i, &title_keys) {
                continue;
            }
            if nca.fs_header(i).is_patch_section() {
                continue;
            }
            if check_section_fs(nca, i, &title_keys).is_err() && !has_sparse_ncas {
                info.sections_errored = true;
                // The sections of an NCZ are compressed, so they never open.
                if fs_nca.ncz.is_some() {
                    continue;
                }
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
        }

        let headers_valid = nca_files.iter().all(|n| n.header_valid);
        if settings.force_hash || (!info.is_errored() && headers_valid) {
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
            let nca_hash = match pending.as_ref().and_then(|p| p.hashes.get(name)) {
                Some(h) => h.clone(),
                None => hash_range(&whole, raw.offset, raw.size)?,
            };
            let expected: String;
            if ct == ContentType::META {
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
                let Some(cf) = content_files.iter().find(|c| &c.file_name == name) else {
                    bail!("The given key '{name}' was not present in the dictionary.");
                };
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
        errors.add("NCA File Order <- Non-standard".into());
    }

    let header_signature_valid = nca_files.iter().all(|n| n.header_valid);
    let is_standard_nsp = can_proceed
        && !generate_new_ticket
        && !has_loose_files
        && is_file_order_correct
        && !copy_new_cert
        && !bad_padding;

    if errors.is_empty()
        && header_signature_valid
        && !copy_new_cert
        && nca_files.iter().all(|n| !n.is_errored())
        && nca_files
            .iter()
            .all(|n| n.hash_match != HashMatch::Mismatch)
    {
        inf!("Validation Complete <- No problems found");
        inf!(
            "Is Standard NSP?  <- {}",
            if is_standard_nsp { "Yes" } else { "No" }
        );
    } else {
        inf!("Validation Complete <- Problems found");
    }
    for w in &warnings.0 {
        wrn!("{w}");
    }
    for e in &errors.0 {
        crate::err!("{e}");
    }

    if !crate::log::is_quiet() {
        let yes_no = |b: bool| if b { "Yes" } else { "No" };
        let valid = |b: bool| if b { "Valid" } else { "Invalid" };
        let mut rows: Vec<(&str, String)> =
            vec![("Title", format!("{display_title} (From {source:?})"))];
        if let Some(p) = display_parent_title
            .as_deref()
            .filter(|p| is_dlc && !p.is_empty())
        {
            rows.push(("Parent Title", format!("{p} (From {source:?})")));
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
        rows.push(("Is Interactive Display ?", yes_no(is_retail_display).into()));
        rows.push(("Is Demo ?", yes_no(is_demo).into()));
        rows.push(("Is DLC Unlocker ?", yes_no(possible_dlc_unlocker).into()));
        if let Some(d) = release_date
            .as_ref()
            .filter(|_| title_type == MetaType::APPLICATION)
        {
            rows.push(("Release Date", d.clone()));
        }
        rows.push(("Has Sparse NCAs ?", yes_no(has_sparse_ncas).into()));
        rows.push((
            "Is Main NCA Compressed ?",
            yes_no(has_compressed_nca).into(),
        ));
        rows.push(("Key Generation", key_gen.to_string()));
        rows.push(("NSP Version", display_version.clone()));
        rows.push(("Rights ID", rights_id_display.clone()));
        rows.push(("Header Validity", valid(header_signature_valid).into()));
        rows.push((
            "NCA Validity",
            valid(nca_files.iter().all(|n| !n.is_errored())).into(),
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
        rows.push(("Has loose files ?", yes_no(has_loose_files).into()));
        rows.push((
            "NCA File Order",
            if is_file_order_correct {
                "Correct"
            } else {
                "Non-Standard"
            }
            .into(),
        ));
        rows.push((
            "File Padding",
            if bad_padding { "Incorrect" } else { "Correct" }.into(),
        ));
        if let (Some(enc), Some(dec)) = (title_key_encrypted, title_key_decrypted) {
            rows.push(("TitleKey (Enc)", hex::encode_upper(enc)));
            rows.push(("TitleKey (Dec)", hex::encode_upper(dec)));
            rows.push((
                "Ticket Signature",
                if is_normalised_signature && title_type == MetaType::APPLICATION {
                    "Normalised".into()
                } else {
                    valid(is_ticket_signature_valid).into()
                },
            ));
            rows.push((
                "Ticket Properties",
                if generate_new_ticket {
                    "Non-Standard"
                } else {
                    "Passed"
                }
                .into(),
            ));
            rows.push(("MasterKey Revision", master_key_rev.to_string()));
        }
        rows.push(("Output Name", output_name.clone()));
        let width = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
        println!();
        for (k, v) in rows {
            println!("  {k:<width$} : {v}");
        }
        println!();
    }

    if !errors.is_empty() && settings.exit_on_error {
        return Ok(1);
    }
    if !can_proceed {
        ftl!("NSP Validation failed.");
        return Ok(1);
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
    if is_standard_nsp && !settings.force_convert {
        inf!("File is already in Standard NSP format. Skipping conversion.");
        return Ok(0);
    }
    if settings.dry_run {
        inf!("[DRYRUN] -> Would create: {output_name}.nsp");
    }

    for p in &plan {
        match p {
            Planned::SkipDelta(name) => inf!("[SKIP] -> Skipping delta fragment: {name}"),
            Planned::Missing(name) => {
                crate::err!("Failed to locate NCA file {name} in the NSP file-system.");
                return Ok(1);
            }
            Planned::MissingNcz(name) => {
                crate::err!("Failed to locate NCZ file {name} in the NSP file-system.");
                return Ok(1);
            }
            Planned::DeltaNotFound(name) => {
                inf!("[SKIP] -> Delta fragment {name} not found in the NSP file-system. Skipping.")
            }
            Planned::Add(i) => {
                if settings.dry_run {
                    inf!("[DRYRUN] -> Would add: {}", ncas[*i].file_name);
                }
            }
        }
    }
    if settings.dry_run {
        inf!("[DRYRUN] -> Would add: {}", ncas[meta_idx].file_name);
    }

    let tail = tail_files(
        ticket.as_ref().filter(|_| has_title_key_crypto),
        &settings.cert,
    );
    if settings.dry_run {
        for (name, _) in &tail {
            inf!("[DRYRUN] -> Would add: {name}");
        }
    }

    if utf16_len(&target_str) > 254 {
        crate::err!("Path too long for Windows ({})", utf16_len(&target_str));
        return Ok(1);
    }
    if target_name == nsp_path || is_same_file(&target_name, nsp_path) {
        crate::err!("Trying to save converted file to the same location as the input file.");
        return Ok(1);
    }
    if target_name.exists() && !settings.overwrite {
        crate::err!(
            "File already exists. ({target_str}). Use --overwrite to overwrite an existing file."
        );
        return Ok(2);
    }
    if settings.dry_run {
        if settings.delete_source {
            inf!("[DRYRUN] -> Would delete: {file_name}");
        }
        return Ok(0);
    }

    inf!("Building Standard NSP => {output_name}");
    let result = match pending {
        Some(p) => p.finish(&target_name, &tail),
        None => PartialNsp::start(
            &target_name,
            &whole,
            &ncas,
            &output_order(&plan, meta_idx),
            &tail,
        )
        .and_then(|p| p.finish(&target_name, &tail)),
    };
    if let Err(e) = result {
        crate::err!("Failed to convert file. {e}");
        return Ok(1);
    }
    inf!("Converted: {output_name}.nsp");

    if settings.delete_source {
        drop(file);
        if let Err(e) = std::fs::remove_file(nsp_path) {
            crate::err!("Failed to delete file. {e}");
            return Ok(1);
        }
        inf!("Cleaned  : {file_name}");
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
    if t.rights_id.is_zero() {
        wrn!("Import tickets - Empty Rights ID. Skipping");
        return Ok(());
    }
    let Some(key) = t.title_key(keys) else {
        wrn!("Import tickets - TitleKey not found. Skipping");
        return Ok(());
    };
    match title_keys.get(&t.rights_id) {
        Some(existing) if *existing != key => bail!("External key already registered"),
        _ => {
            title_keys.insert(t.rights_id, key);
        }
    }
    if current.is_some() {
        wrn!("Import tickets - Multiple tickets found. Using first.");
        return Ok(());
    }
    *current = Some(t);
    Ok(())
}
