<!--
SPDX-FileCopyrightText: 2026 Timothy Redaelli <timothy.redaelli@gmail.com>

SPDX-License-Identifier: MIT OR Apache-2.0
-->

Version: *0.1*

# Standard NSZ

A Standard NSZ is a [Standard NSP](StandardNSP.md) with some of its NCAs
compressed to NCZ, the format written by nsz. It is an extension of this
port, not part of the Standard NSP proposal.

The goal is the same as for the Standard NSP: the same input always gives
the same bytes, so a Standard NSZ can be verified against a known hash, and
decompressing it gives back the Standard NSP byte for byte.

## Contents

Start from the Standard NSP of the input. Then:

1. Each program NCA and each public data NCA is replaced by its NCZ, when
   nsz would compress it (see below). The file name keeps the NCA ID and
   changes the extension: `<nca id>.nca` becomes `<nca id>.ncz`.
2. Every other file stays as it is in the Standard NSP: the CNMT NCA, the
   control, manual and other NCAs, the ticket and the certificate.
3. The PFS0 entries keep the Standard NSP order. An NCZ takes the place of
   the NCA it replaces, and the PFS0 header is built the same way, with the
   NCZ sizes.

A program or public data NCA stays uncompressed, as with nsz, when:

- its enabled sections do not run back to back from the first section to
  the end of the NCA, or the NCA is not larger than 0x4000 bytes;
- one of its sections uses an encryption that NCZ cannot describe
  (anything but none, AES-CTR and AES-CTR-EX).

## NCZ layout

All integers are little endian.

1. The first 0x4000 bytes of the NCA (header and FS headers), unchanged.
2. The section table:
   - `NCZSECTN` (8 bytes), then the entry count (u64);
   - one 0x40 byte entry per section, sorted by offset: offset (u64), size
     (u64), crypto type (u64, 1 = none, 3 = AES-CTR), padding (u64, zero),
     key (16 bytes), counter (8 byte upper counter, then 8 zero bytes).

   The key is the decrypted section key. It is stored in plaintext entries
   too, and is zero there when the title key is not available. An AES-CTR-EX
   section becomes one AES-CTR entry per counter range.
3. The rest of the NCA, from 0x4000 to the end, with the sections
   decrypted, then compressed in blocks:
   - `NCZBLOCK` (8 bytes), then version 2, type 1, one zero byte, and the
     block size exponent 20 (1 MiB blocks), one byte each;
   - the block count (u32) and the decompressed size (u64);
   - the compressed length (u32) of each block;
   - the blocks, each compressed on its own as one zstd frame. A block whose
     frame is not smaller than the block is stored raw.

Blocks keep random access, which is needed to mount the NSZ (for example
with `fuse-nsz`).

## Determinism

Every zstd frame is written with fixed settings:

- libzstd 1.5.5 (pinned), compression level 19;
- multithreaded framing with one worker, and no other parameter;
- each frame stores its decompressed size in the header.

Each block is compressed on its own, so the number of threads used to
compress blocks in parallel does not change the output. The same Standard
NSP always gives the same Standard NSZ.

The bytes differ from what nsz writes for the same NSP, because nsz uses its
own zstd settings. Both are valid NCZ and decompress to the same NCAs.

## Round trip

Decompressing each NCZ back to its NCA, and the NSZ back to an NSP, gives
the Standard NSP byte for byte. A Standard NSZ can therefore be checked
against the same DATs as the Standard NSP, after decompression.
