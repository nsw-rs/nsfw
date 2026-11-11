<!--
SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>

SPDX-License-Identifier: MIT OR Apache-2.0
-->

# nsfw

(N!nt3ndo Sw!tch File Wizard, in Rust)

---
## What is this?

A Rust port of NSFW 0.1.184 by mRg. It verifies
the integrity of your NSP files, reports any problems and converts them to
the deterministic "Standard NSP" format, so that they can be accurately
verified and preserved.

The port covers the `validate` command (including Standard NSP conversion
and TitleDB lookups), `query` and `build-titledb`. Output file names, verdicts and the
bytes of converted NSPs are identical to those of NSFW 0.1.184, except
where [Differences from NSFW](#differences-from-nsfw) says otherwise. It is
a single self-contained binary: no .NET runtime is needed.

The NSFW and LibHac repositories are no longer available on GitHub, so this
port keeps the tool buildable and maintained.

The Switch file format code (NCA, PFS0, RomFS, tickets, CNMT, ...) lives in
[libhac](https://github.com/nsw-rs/libhac), a Rust port of LibHac.

This tool would not be possible without the work and knowledge of:

* NSFW (mRg), the original tool
* LibHac (Alex Barney, @thealexbarney)
* libhac - https://github.com/nsw-rs/libhac, the Rust port of the
  LibHac parts used here
* DarkMatterCore - https://github.com/DarkMatterCore/ (@DarkMatterCore)
* TitleDB - https://github.com/blawar/titledb, for title metadata
* Diacritics.NET - https://github.com/thomasgalliker/Diacritics.NET, whose
  character map is used to clean up titles

> [!CAUTION]
> This software is still in active development and should be considered
> experimental. Please use with caution and always keep a backup of your
> files.

> [!IMPORTANT]
> This software is designed to work with legal backup copies of your own
> games.

---
## Installation

1. Download the archive for your system from the
   [latest release](https://github.com/nsw-rs/nsfw/releases/latest) and
   extract `nsfw` (`nsfw.exe` on Windows) to a directory of your choice.
   Linux binaries are statically linked.
1. Put your keys in `~/.switch/prod.keys` (or pass `-k`). `~` is `HOME`,
   and on Windows your user profile directory when `HOME` is not set. Only
   a path that is `~` or starts with `~/` is expanded.
1. (*Optional*) The common certificate chain `~/.switch/common.cert` (or
   `-c`) is not shipped because it is Nintendo data. If it is missing, nsfw
   takes it from the first NSP it processes that has a ticket (BASE, UPD or
   DLC) and saves it there. Its SHA-256 is
   `3c4f20dca231655e90c75b3e9689e4dd38135401029ab1f2ea32d1c2573f1dfe`.
1. (*Optional*) Put a `titledb.db` in a directory called `titledb` in the
   current directory. Either take it from an NSFW release, or build it with
   `nsfw build-titledb` (see below).

### Building from source

Requires Rust 1.89 or newer.

```
cargo build --release
```

The binary is written to `target/release/nsfw`.

Or install it from crates.io (the package is `nsfw-cli`, the binary is
`nsfw`):

```
cargo install nsfw-cli
```

## What is a "Standard NSP"?

See [Standard NSP](StandardNSP.md), the format description from NSFW.

## Validate (`v`)

```
Validate NSP files, optionally converting them to Standard NSP

Usage: nsfw validate [OPTIONS] <NSP>...

Arguments:
  <NSP>...  Paths to NSP files or directories

Options:
  -k, --keys <KEYS>        Path to NSW keys file [default: ~/.switch/prod.keys]
  -c, --cert <CERT>        Path to 0x700-byte long common certificate chain file (created from the NSP if missing) [default: ~/.switch/common.cert]
  -s, --standard           Convert to Standardised NSP
  -S, --nsz                Convert to Standardised NSZ: the Standard NSP with its program and public data NCAs compressed to NCZ (implies --standard)
  -r, --rename             Rename NSP to match TitleDB. No other actions performed
      --skip-hash          When re-naming files, skip NCA hash validation
  -o, --nspdir <NSPDIR>    Path to standardised NSP output directory [default: ./nsp]
  -d, --dryrun             Prints actions to be performed but does not execute any of them
      --nl                 Do not include any languages in output filename
      --sl                 Use short language codes in output filename
  -q, --quiet              Set output level to 'quiet'. Minimal display for details
      --force-hash         Force hash verification of bad NCA files (where header validation has already failed)
      --keep-deltas        When creating a standard NSP, include Delta Fragment files
      --force-convert      Force conversion of NSP to standard NSP (even if already in standard format)
      --overwrite          Overwrite any existing files
      --keep-name          Keep original name when converting to standard NSP
  -Z, --delete-source      Delete source NSP file after conversion
  -e, --exit-on-error      Exit batch processing on first error
  -b, --batch <BATCH>      Process a certain number of files and then exit [default: 0]
      --skip <SKIP>        Skip a certain number of files before processing [default: 0]
      --titledb <TITLEDB>  Path to titledb.db file [default: ./titledb/titledb.db]
  -y, --verify             Verify title against TitleDB
  -H, --check-padding      Check headers and padding of NCA files
  -h, --help               Print help
```

NSZ files (NSPs with compressed `.ncz` NCAs, block or solid) are accepted
too, also in directory scans. They are reported with a "Compressed NCA"
warning. See below for converting them to Standard NSP.

Every `validate` of an NSZ, also a plain one and `-r`, decompresses each
`.ncz` and checks it as the NCA it stands for: its sections, its NPDM
signature and its hash against the CNMT. So an NSZ gets the verdict of its
NSP, and a damaged `.ncz` fails validation (exit code 1). The whole file is
read and decompressed for that. `--skip-hash` leaves an `.ncz` compressed:
neither its hash nor its sections are checked.

`-H` checks the header and the string table padding of the NSP itself, not
of the NCAs inside, and nothing else: with it `-s`, `-S` and `-r` do nothing.

When `./titledb/titledb.db` (or the `--titledb` file) exists it is used for
the output name (release region, and the title of a file that has none of
its own) also without `-y`.

Output is colored when it goes to a color terminal. Set `NO_COLOR` to turn
colors off or `CLICOLOR_FORCE` to force them on. Piped output has no colors.
Control characters in file names, titles and TitleDB text are printed in
escaped form (`\u{1b}`).

Exit codes: 0 when the file is valid (also when it is not a Standard NSP,
and "Problems found" is printed) or was converted or renamed, 1 for a file
that fails validation and for every other error, 2 when the output file
exists and `--overwrite` is not given, and for a wrong command line.

### Validate - Example Usage

Validate

`./nsfw v <PATH_TO_NSP_FILE>`

Validate - Minimal details

`./nsfw v --quiet <PATH_TO_NSP_FILE>`

Validate - Verify against TitleDB - Uses TitleDB to check title and metadata
(if found).

`./nsfw v --verify <PATH_TO_NSP_FILE>`

Validate - Force Hash - By default, if header validation fails, the NCA file
will not be hashed, as the file is already invalid. This option will force the
NCA file to be hashed regardless of header validity.

`./nsfw v --force-hash <PATH_TO_NSP_FILE>`

Batch verification is also supported (output is always quiet):

`./nsfw v <PATH_TO_NSP_DIRECTORY>`

Batch verification - Process 10 files, skipping the first 20, and stop at
the first error.

`./nsfw v -b 10 --skip 20 -e <PATH_TO_NSP_DIRECTORY>`

Batch verification of several files or directories - An extension of this
port, not in NSFW (which takes one path). The files of each directory are
sorted as in a single directory scan, and the paths are processed in the
order given. More than one path always runs in batch mode. A file reached
through more than one argument is processed once. A missing path is reported
and skipped (with `-e` it stops the run). In batch mode the exit code is the
worst one over all files (1 before 2), not the one of the last file as in
NSFW.

`./nsfw v <PATH_TO_NSP_FILE> <PATH_TO_NSP_DIRECTORY> ...`

A directory scan takes every `*.ns?` file (in any case: `.nsp`, `.NSZ`),
also in sub-directories. Links to directories are followed, so files
outside the given directory can be reached, and each directory is scanned
once. A directory that cannot be read is reported and skipped, and the run
ends with exit code 1.

---

### Create "Standard NSP" (`v -s`) - Example Usage

Will re-create the NSP file in a standardised format, normalising ticket
properties and re-ordering the NCA files to match the order in the CNMT file.

> [!IMPORTANT]
> This will not fix any signature or validation problems with the NSP file.
> It will abort if any validation errors are found.

> [!WARNING]
> This process will **exclude** Delta Fragment files from the rebuilt NSP as
> they affect standardisation. To include these please use the
> `--keep-deltas` option.

Create "Standard NSP" - Will write the new NSP file to the directory defined
by the `--nspdir` option.

`./nsfw v -s <PATH_TO_NSP_FILE>`

Create "Standard NSP" - Dry Run - Will validate and print actions but will
not execute them.

`./nsfw v -s -d <PATH_TO_NSP_FILE>`

Create "Standard NSP" - Short Languages + TitleDB Verify - Will use short
language codes and verify title name against TitleDB (if available).

`./nsfw v -s --sl -y <PATH_TO_NSP_FILE>`

Create "Standard NSP" - Short Languages + TitleDB Verify + Delete source -
Will use short language codes and verify title name against TitleDB (if
available). This will also **DELETE** the original/source file if conversion
is successful.

`./nsfw v -s --sl -y -Z <PATH_TO_NSP_FILE>`

Create "Standard NSP" from an NSZ - NSZ files (solid or block compressed)
are accepted as input. Like NSFW, an NSZ whose layout is already standard is
reported as Standard NSP and skipped, so `--force-convert` is needed to write
the decompressed NSP.

`./nsfw v -s --force-convert <PATH_TO_NSZ_FILE>`

The output is written as `<name>.part` in the output directory, synced to
disk and then renamed, so the final name only ever holds a complete file.
The source is deleted (`-Z`) only after that, and not at all when the output
is an update that fails validation (see
[Differences from NSFW](#differences-from-nsfw)). Ctrl-C and a closed output
(`nsfw v -s dir | head`) stop the run and remove the partial file. A run
that was killed leaves it behind, and a later run then refuses to convert
that title until the `.part` file is deleted: it cannot know that no other
run is writing it.

---

### Create "Standard NSZ" (`v -S`) - Example Usage

An extension of this port, not in NSFW. Builds the Standard NSP, then
compresses its program and public data NCAs to NCZ the way nsz does (other
NCAs, like the CNMT and control NCAs, stay uncompressed), and writes it as
`.nsz`. `-S` implies `-s` and converts even when the input is already a
Standard NSP.

Decompressing a Standard NSZ (`v -s --force-convert`, or a `fuse-nsz`
mount) gives back the Standard NSP byte for byte, so it can be checked
against the same DATs.

The NSZ bytes are reproducible too: the same input always gives the same
NSZ, whatever the number of CPUs, because the compression settings (zstd
1.5.5, level 19) are fixed. They differ from what nsz writes for the same
NSP.

`./nsfw v -S <PATH_TO_NSP_FILE>`

Each NCA is compressed in 1 MiB blocks, which keep random access (needed
to mount it with `fuse-nsz`).

See [Standard NSZ](StandardNSZ.md) for the format.

---

### Rename to the Standard name (`v -r`) - Example Usage

Renames the NSP in place to the name a Standard NSP conversion would give it
(TitleDB is used when found). Nothing else is changed. An NSZ keeps its
`.nsz` extension. A file that fails validation is not renamed, and an
existing file with the target name is replaced only with `--overwrite`.

`./nsfw v -r <PATH_TO_NSP_FILE_OR_DIRECTORY>`

Rename - Dry run, only show the new name

`./nsfw v -r -d <PATH_TO_NSP_FILE>`

Rename - Fast, skip the NCA hash checks, verify the title against TitleDB

`./nsfw v -r --skip-hash -y <PATH_TO_NSP_FILE>`

`--skip-hash` also works without `-r`, but then the result of the validation
is reported as unknown. It cannot be used with `-s` or `-S` (the C# tool
allows it), so a conversion never copies NCAs that were not hash checked.

---

## Query TitleDB (`q`)

Prints the TitleDB entry of a title ID: name, publisher, description, the
title in each region, the eShop entries with their regions, the known updates
and the related titles (base, update, DLCs). An update ID (ending in `800`)
is looked up through its base title.

```
Look up a title ID in TitleDB

Usage: nsfw query [OPTIONS] <TITLEID>

Arguments:
  <TITLEID>  Title ID to search for

Options:
      --titledb <TITLEDB>  Path to titledb.db file [default: ./titledb/titledb.db]
  -h, --help               Print help
```

`./nsfw q 0100566015AD2000`

---

## Build TitleDB (`build-titledb`)

Builds `titledb.db` from a checkout of
[blawar/titledb](https://github.com/blawar/titledb), with the same schema and
row order as NSFW. The database is written next to the JSON files, so the
default layout matches the `--titledb` default of `validate`. With `-c` the
old database stays in place until the new one is complete. Without it the
titles that are not in the database yet are added.

```
Build titledb.db from the blawar/titledb JSON files

Usage: nsfw build-titledb [OPTIONS]

Options:
      --titledbdir <TITLEDBDIR>  Path to TitleDB jsons directory [default: ./titledb]
  -c, --clean                    Clean database before rebuilding
  -h, --help                     Print help
```

---

## Differences from NSFW

Names, verdicts and converted bytes are those of NSFW 0.1.184 for every
file that NSFW handles, except where a point below says otherwise. What
this port does differently:

* Additions: several paths for `validate`, Standard NSZ output (`-S`), NSZ
  input with solid NCZ, a common certificate taken from the NSP when the
  file is missing, progress bars.
* The `.ncz` files of an NSZ are decompressed and checked in every
  `validate`, also with `-r`: one that is damaged, or whose size is not the
  one in the CNMT, fails validation. NSFW checks them only when it converts
  the file, and reports the damage there as a failed conversion.
  `--force-hash` works on an NSZ.
* The sections and the NPDM of an `.ncz` are checked on the decompressed
  NCA. An intact NSZ reads "No problems found", and one whose ticket does
  not open the sections is not converted. NSFW cannot open the sections of
  an `.ncz`: it reports "Problems found" for every NSZ and converts it
  without that check.
* There is no upper limit on the key generation. NSFW refuses a title
  above key generation 18, which is every title made for firmware 19.0.0
  or later. What counts is that the keys file has the keys
  (`key_area_key_*_XX`, `titlekek_XX`) of that generation: when it does
  not, the message says so. NSFW derives them from the master keys.
* "Is Standard NSP?" is stricter: the file must be the header of its
  files, then the files back to back in the order of the file table, and
  nothing after them. A gap, data in another order, a stray byte in the
  header padding or bytes after the last file give "Problems found", "File
  Padding: Incorrect" and a conversion with `-s`. NSFW reads three bytes
  near the end of the file and says "Yes" for all of these. `-H` fails on
  them too.
* A title taken from the file name loses `.nsz` as it loses `.nsp`. NSFW
  leaves `.nsz` in: `game.nsz` gives `game.nsz (Unknown)[...]`.
* `-Z` does not delete the source of an update whose ticket had to be
  written again (exit code 1). As in NSFW, the new ticket of an update
  keeps the signature of the old one, which does not sign the new fields,
  so the output fails validation ("Ticket signature is invalid."). The
  output bytes are the ones of NSFW.
* A conversion goes to a `.part` file that is renamed at the end. NSFW
  writes the final name directly and deletes it when the conversion fails.
* The output path length limit (254 characters) applies on Windows only.
* `-r`: an NSZ keeps its `.nsz` extension, and `--overwrite` replaces an
  existing file of the target name (NSFW fails there). `-r` with `-s` is
  refused, and so is `--skip-hash` with `-s`.
* Missing keys, a bad certificate or an empty directory end the run with a
  message and exit code 1. In batch mode the exit code is the worst one.
* A dry run (`-d`) and a run that finds no file do not create the output
  directory.
* The directory scan matches `*.ns?` in any case on every system (NSFW only
  on Windows and macOS), lists each directory once and skips one that it
  cannot read.
* Only a leading `~` or `~/` is expanded, so `~name.nsp` is a file name.
* TitleDB is opened without writing next to it, so it works from read-only
  media. `build-titledb -c` keeps the old database until the new one is
  complete, and a second run without `-c` no longer adds the update and
  region rows again.
* `query` prints plain rows instead of nested tables, and takes only a
  hexadecimal title ID.
* The properties table shows the key generation as a number (NSFW shows a
  firmware range) and has no "Meta Validity" row.

## Not ported (yet)

The following NSFW features are not available in this port:

* `validate`: extract (`-x`, `-X`, `--force-extract`), `--full` output,
  `--related-titles`, `--regional-titles`, `--updates`, `--show-keys`,
  `--dump-headers`, `--cdndir`, `--wait`
* the `ticket`, `cnmt`, `hash` and `cdn2nsp` commands, and the hidden
  `cmp`, `scan` and `ni` commands
* the `btdb` alias of `build-titledb`

## License

nsfw is licensed under either of [MIT](LICENSES/MIT.txt) or
[Apache-2.0](LICENSES/Apache-2.0.txt) at your option. Code ported from NSFW
and LibHac keeps the copyright notices of their authors (both MIT upstream).

The exception is `src/diacritics.rs`: its character map comes from the
Diacritics package, which is Apache-2.0 only, so that file is licensed under
[Apache-2.0](LICENSES/Apache-2.0.txt). Binaries therefore carry
`(MIT OR Apache-2.0) AND Apache-2.0`. `StandardNSP.md` is copied unchanged
(except for a dead image link) from NSFW and stays [MIT](LICENSES/MIT.txt).

The repository is [REUSE](https://reuse.software/) compliant: every file
states its copyright and license.
