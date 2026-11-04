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
and TitleDB lookups) and `build-titledb`. Output file names, verdicts and the
bytes of converted NSPs are identical to those of NSFW 0.1.184. It is a
single self-contained binary: no .NET runtime is needed.

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
1. Put your keys in `~/.switch/prod.keys` (or pass `-k`). On Windows `~` is
   your user profile directory.
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

Usage: nsfw validate [OPTIONS] <NSP>

Arguments:
  <NSP>  Path to NSP file or directory

Options:
  -k, --keys <KEYS>        Path to NSW keys file [default: ~/.switch/prod.keys]
  -c, --cert <CERT>        Path to 0x700-byte long common certificate chain file (created from the NSP if missing) [default: ~/.switch/common.cert]
  -s, --standard           Convert to Standardised NSP
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

Output is colored when it goes to a color terminal. Set `NO_COLOR` to turn
colors off or `CLICOLOR_FORCE` to force them on. Piped output has no colors.

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

## Build TitleDB (`build-titledb`)

Builds `titledb.db` from a checkout of
[blawar/titledb](https://github.com/blawar/titledb), with the same schema and
row order as NSFW. The database is written next to the JSON files, so the
default layout matches the `--titledb` default of `validate`.

```
Build titledb.db from the blawar/titledb JSON files

Usage: nsfw build-titledb [OPTIONS]

Options:
      --titledbdir <TITLEDBDIR>  Path to TitleDB jsons directory [default: ./titledb]
  -c, --clean                    Clean database before rebuilding
  -h, --help                     Print help
```

---

## Not ported (yet)

The following NSFW features are not available in this port:

* `validate`: extract (`-x`, `-X`), rename (`-r`), `--full` output,
  `--related-titles`, `--regional-titles`, `--updates`, `--show-keys`
* the `ticket`, `cnmt`, `query` and `cdn2nsp` commands

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
