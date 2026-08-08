# QBW File Format Specification (Draft)

**Status:** incomplete, in-progress reverse engineering. Every numeric
claim in this document is backed by a reproducible observation in
[`re/NOTES.md`](https://github.com/Sigilweaver/OpenQBW/blob/main/re/NOTES.md) against the corpus manifest
`re/corpus_index.json` (112 `.QBW` files, ~1.87 GiB).

## 0. Conventions

- Multi-byte integers are denoted `u16_LE` / `u32_LE` etc. (little-
  endian unless stated). Endianness is observed, not assumed.
- Offsets are in **bytes from start-of-file** unless noted.
- "QBW" here = QuickBooks Desktop **company** file (`.QBW`). Sibling
  formats (`.QBB`, `.QBM`, `.TLG`, `.ND`, `.DSN`, `.QBA`) have their
  own shape and are not specified yet.

## 1. File family overview

| Extension | Role (hypothesised)                              | Corpus? |
| --------- | ------------------------------------------------ | ------- |
| `.QBW`    | Primary company database.                        | 112 files |
| `.QBB`    | Compressed backup of a `.QBW`.                   | 0 |
| `.QBM`    | "Portable" company file (compact backup).        | 0 |
| `.TLG`    | Transaction log / WAL companion to a `.QBW`.     | 0 |
| `.ND`     | Network descriptor (small text config).          | 0 |
| `.QBA`    | Accountant's copy.                               | 0 |

Only `.QBW` is specified here.

## 2. High-level layout of `.QBW`

A `.QBW` file is a **paged, fixed-size-page, encrypted database**
container, empirically consistent with a SAP/Sybase **SQL Anywhere**
database image (ASA / iAnywhere) with bulk page contents encrypted.

Whole-file facts (corpus-wide, 112/112 files):

| Fact                                  | Value |
| ------------------------------------- | ----- |
| Minimum file size                     | 12,955,648 B |
| Maximum file size                     | 44,126,208 B |
| Greatest common divisor of all sizes  | **4096 B** |
| % of files where `size % 4096 == 0`   | 100% |
| % of files where `size % 8192 == 0`   | 51% |

→ **Page size is 4096 bytes (4 KiB)** and a file is exactly
`page_count × 4096` bytes long. See [C.2](#c2-page-size).

### 2.1 Page-0 is the *superblock*

Page 0 is the only page that contains extensive plaintext structure.
Pages 1..N are high-entropy (encrypted payload) but **every** page
(including encrypted ones) ends with a 4-byte integrity footer - see
§2.2.

### 2.2 Universal per-page CRC-32 footer

For **every** page of **every** file in the corpus (456,521 pages
across 112 files) the final four bytes of the page are the
zlib/IEEE CRC-32 of the first 4092 bytes of the same page, stored
little-endian:

```
page[0x0FFC..0x1000] == u32_LE( zlib.crc32( page[0x0000..0x0FFC] ) )
```

100 % match, 0 failures. This holds for encrypted pages as well, which
means the CRC is computed over the on-disk (ciphertext) bytes: the
encryption layer runs *before* CRC stamping, so a parser can validate
page integrity **without** knowing the encryption key. See [C.5](https://github.com/Sigilweaver/OpenQBW/blob/main/re/NOTES.md#c5--universal-page-footer-is-crc32-of-the-page-body--2026-04-19).

### 2.2a Universal 12-byte page trailer (C.12)

The 12 bytes immediately before the CRC-32 footer - offsets
`0xFF0..0xFFB` - have a fixed layout, proven by scanning all
456 409 non-zero pages across 112 files with **zero invariant
failures**:

| Offset | Size | Field         | Notes |
|-------:|-----:|---------------|-------|
| 0xFF0  | 1    | `flag_ff0`    | per-type variable byte |
| 0xFF1  | 1    | `flag_ff1`    | almost always `0x00` (rare `0x01`) |
| 0xFF2  | 1    | **`page_type`** | ASCII letter; see table below |
| 0xFF3  | 1    | `zero_ff3`    | **always `0x00`** (checked) |
| 0xFF4  | 1    | `meta_ff4`    | varies per page type |
| 0xFF5  | 1    | `meta_ff5`    | varies per page type |
| 0xFF6  | 6    | `zero_ff6`    | **always `00 00 00 00 00 00`** (checked) |
| 0xFFC  | 4    | `crc32_le`    | zlib CRC-32 over `page[0x000..0xFFC]` |

Corpus-wide frequency of the page-type byte at 0xFF2
(456 409 pages, 112 files):

| Byte | ASCII | Count  | %      | Working name / guess |
|-----:|------:|-------:|-------:|----------------------|
| 0x45 | `'E'` | 242 460 | 53.12 % | **E**xtent / data page |
| 0x41 | `'A'` | 208 783 | 45.74 % | **A**llocation / free-space map |
| 0x4D | `'M'` |   3 159 |  0.69 % | **M**ap / metadata |
| 0x48 | `'H'` |     550 |  0.12 % | **H**eader block |
| 0x43 | `'C'` |     492 |  0.11 % | **C**atalog |
| 0x40 | `'@'` |     448 |  0.10 % | reserved / bootstrap (pages 4..7) |
| 0x49 | `'I'` |     421 |  0.09 % | **I**ndex (page 1 is typically this) |
| 0x47 | `'G'` |      96 |  0.02 % | unknown |

The page trailer is the **only** fully-structural region observed on
pages 1..N so far. Combined with C.5 it gives a parser the complete
page integrity+typing protocol without touching page contents. See
[C.12](https://github.com/Sigilweaver/OpenQBW/blob/main/re/NOTES.md#c12--universal-12-byte-page-trailer-at-0xff00xffb--2026-04-19).

### 2.2b Pages decompose into 8 × 512-byte sectors; many sectors are pure fill (C.13)

Each 4 KiB page is physically 8 sectors of 512 bytes. For every
sector, either every adjacent byte-difference is a single constant
`step` (mod 256) - in which case the sector is a **pure arithmetic-
progression fill** carrying no information - or the sector carries
real data. The choice is deterministic by page type:

| page type | sectors 1..6 pure-fill % | real data lives in |
|-----------|:------------------------:|--------------------|
| `'C'` catalog | **100.0 %**         | sector 0 and sector 7 only |
| `'H'` header  | **100.0 %**         | sector 0 and sector 7 only |
| `'M'` map     | **94..100 %**       | sector 0 and sector 7 only |
| `'I'` index   | sectors 0..4,6: 0 %, sector 5: 85.7 % | sectors 0..4, 6, 7 |
| `'A'` alloc   | 46..60 %            | sparsely - bitmap overlays |
| `'E'` extent  | 5..6 %              | nearly all 4080 body bytes |
| `'@'` boot    | ~25..75 % (mixed)   | bootstrap pages 4..7 |

(Measured on 10 772 data pages of Rock Castle Construction.)

Consequences for a parser:

1.  For `'C'`, `'H'`, `'M'` pages, only the first 512 B and the
    last 512 B of the page (≈1008 B of payload, net of the trailer)
    need to be parsed. The middle 3 KiB is fill.
2.  The fill is a deterministic function of `(page_number,
    sector_index)` - it is independent of the individual file - 
    which is what makes ~14 % of pages byte-identical between
    unrelated files (§4.1) and rules out any per-file keying.
3.  The first byte of consecutive pure-fill sectors within a page
    increments by 1 (e.g. sectors 1..6 of page 4 start with
    `0x31, 0x32, 0x33, 0x34, 0x35, 0x36`). The exact formula for
    the AP `step` vs `(page, sector)` is not yet linear and is
    open; a likely candidate is a rolling phase index into the
    38-byte SAP copyright string (§3.4).

Reproduce with `re/sector_analysis.py`. See [C.13](https://github.com/Sigilweaver/OpenQBW/blob/main/re/NOTES.md#c13--pages-decompose-into-8--512-b-sectors-many-are-pure-ap-fill--2026-04-19).

### 2.2c The AP fill is an additive keystream (C.14)

Pages 1..N are **obfuscated, not encrypted**, by a deterministic
per-sector arithmetic progression added mod 256 to the SAP SQL
Anywhere plaintext:

```
stored[i] = ( base(page, sector) + i * step(page, sector) + plaintext[i] ) mod 256
```

- `base` and `step` are 8-bit constants depending on
  `(page_number, sector_index)` only. They do **not** depend on
  file identity, which is why ~14 % of pages are byte-identical
  between unrelated files (C.8) and why the corpus clusters into
  6 shipped templates (C.10).
- Empty regions of an SA page carry `plaintext = 0`, so the stored
  bytes reduce to the pure AP fill (C.13).
- `step` is recovered as the modal adjacent byte difference of the
  sector; `base` is then chosen to maximise AP match count. A
  closed-form expression for `(base, step)` as a function of
  `(page, sector)` is still open.

Applying `plaintext[i] = (stored[i] − fill[i]) mod 256` to page 11
of the Rock Castle sample recovers actual SA system procedure
identifiers:

```
...0a sp_columns 01 50 03 ...
...0b sp_password...
...0d sp_addmessage 01 50 03 ...
...0b sp_addlogin 01 50 03 ...
```

and a row-slot directory counting down from `0x0fca` to `0x01c2` - 
the canonical SA **page → slot → record** layout. That is direct
evidence the payload is a genuine SAP SQL Anywhere 17.0.4 database.

Reproduce with `re/fill_overlay.py`. See
[C.14](https://github.com/Sigilweaver/OpenQBW/blob/main/re/NOTES.md#c14--the-ap-fill-is-an-additive-keystream-plaintext-recovered--2026-04-19).

### 2.3 Underlying engine: SAP SQL Anywhere 17.0.4 build 2182

Page 0 contains, starting at offset `0x401` and repeated (rolled) to
fill offsets `0x400..0x1000`, the fixed 38-byte SAP SQL Anywhere
sector-fingerprint string

```
"2182 SAP SE, Copyright (c)2015 17.0.4."
```

...which pins the embedded engine to **SAP SQL Anywhere 17.0.4 build
2182** (2015 release). The substring `"17.0.4.2182 SAP SE, Copyright
(c)2015"` is present in page 0 of 112/112 files. See [C.6](https://github.com/Sigilweaver/OpenQBW/blob/main/re/NOTES.md#c6--sap-sql-anywhere-1704-build-2182-fingerprint--2026-04-19).

## 3. Page-0 superblock

The first 64 bytes of every `.QBW` are a fixed-layout header. Byte
roles were determined by the conservation map below (`.` = byte is
identical across all 112 files, `f` = 2-4 distinct values (flag),
`V` = high-cardinality variable):

```
 off   0  1  2  3  4  5  6  7  8  9  a  b  c  d  e  f
0x00   .  .  .  .  .  .  f  .  V  V  .  .  .  .  .  .
0x10   .  .  .  .  .  .  .  .  .  .  .  .  V  V  .  .
0x20   V  V  .  .  V  V  V  V  V  f  V  V  V  .  .  .
0x30   .  .  .  .  .  .  .  .  .  .  .  .  .  .  .  .
```

### 3.1 Superblock header (offsets 0x00 - 0x2F)

| Offset | Size | Type     | Name                  | Notes |
| -----: | ---: | -------- | --------------------- | ----- |
| 0x00   | 6    | zeros    | `reserved_0`          | Always `00 00 00 00 00 00`. |
| 0x06   | 1    | u8 flag  | `flags_06`            | Observed values: `0x09` (105/112) and `0x49` (7/112). The 7 `0x49` files are all "Getting Started / Chapter 1 / Chapter 13 / Managing Company Files" demo files. Semantic unknown. |
| 0x07   | 1    | zeros    | `reserved_07`         | Always `0x00`. |
| 0x08   | 4    | u32_LE   | `file_id_lo`          | Unique per file (111/112 distinct). Candidate: low half of a 64-bit file UUID / random salt. |
| 0x0C   | 4    | zeros    | `reserved_0C`         | Always `00 00 00 00`. |
| 0x10   | 4    | u32_LE   | `format_major`        | Always **3**. Candidate: major format version. |
| 0x14   | 4    | u32_LE   | **`magic`**           | Always **`0xDA7ABA5E`** (bytes `5E BA 7A DA`). |
| 0x18   | 2    | u16_LE   | `version_a`           | Always **201** (`0x00C9`). |
| 0x1A   | 2    | u16_LE   | `version_b`           | Always **12** (`0x000C`). |
| 0x1C   | 2    | u16_LE   | `page_count_hint`     | Non-constant; `= total_pages − 128` almost always. See §3.2. |
| 0x1E   | 2    | zeros    | `reserved_1E`         | Always `00 00`. |
| 0x20   | 4    | u32_LE   | `unknown_20`          | Varies per file. |
| 0x24   | 4    | u32_LE   | `unknown_24`          | Varies per file. |
| 0x28   | 1    | u8       | `unknown_28`          | Varies per file. |
| 0x29   | 1    | u8 flag  | `flags_29`            | `0x00` (72/112) or `0x01` (40/112). |
| 0x2A   | 3    | var      | `unknown_2A`          | Varies. |
| 0x2D   | 3    | const    | `const_2D`            | Always `0D 04 00`. |
| 0x30   | 16   | zeros    | `reserved_30`         | Always 16 zero bytes. |

### 3.2 Page-count hint

The `u32_LE` at offset `0x1C` is `total_pages − 128` for the vast
majority of files (`total_pages = file_size / 4096`). Across 112 files
the difference `total_pages − hint` is ≥ 78, usually exactly 128. This
strongly suggests the first 128 pages (524 288 B) are *reserved /
metadata* and the hint counts the data pages after them.

_Validate_ this once we have an explicit page-usage bitmap.

### 3.2a  Middle zero band (0x040 - 0x162)

Largely zero with a few file-specific bytes around offsets `0x06C`,
`0x096`, `0x0A0`, `0x0B8`, `0x0C8`. Not yet decoded.

### 3.2b  Secondary header block (0x2BC - 0x2E4)

A second dense structure block, ~40 bytes long, containing two
repeated 8-byte values (e.g. `31 9C 41 3F 90 92 B4 00` appears twice
per file) and what looks like a UUID
(`58 E1 D7 49 46 AD 44 6B A0 5A 9C 8B C4 5C EB C5` - version nibble
`0x4` → UUIDv4). Function unknown. Candidates: LSN pair + instance
UUID.

### 3.3 Plaintext collation / codepage block (≈ offsets 0x162 - 0x1FF)

Near the middle of the 4 KiB superblock there is a plaintext region
that contains the SAP SQL Anywhere collation names:

```
0x0162  57 02 00 00 31 32 35 32 4c 41 54 49 4e 31 00 ...    |W...1252LATIN1..|
0x01B8  77 69 6e 64 6f 77 73 2d 31 32 35 32 00 ...           |windows-1252..|
0x01D4  55 43 41 00 ...                                      |UCA..|
0x01FC  55 54 46 2d ...                                      |UTF-|  (probably UTF-8)
```

- `1252LATIN1` - default / CHAR collation.
- `windows-1252` - CHAR codepage label.
- `UCA` - Unicode Collation Algorithm (alternate / NCHAR collation).
- `UTF-8` (truncated in our window) - NCHAR codepage label.

These four strings, in this order, are the SAP SQL Anywhere "language
/ collation" record written verbatim into the ASA database header.
Their presence in every `.QBW` at the same position is the strongest
single piece of evidence that `.QBW` payload is an obfuscated SQL
Anywhere database image.

### 3.4 Page-0 copyright-fingerprint region (0x400 - 0xFFC)

Offsets `0x400..0xFFC` (3 068 bytes) are a single deterministic
pattern: the 38-byte string `"2182 SAP SE, Copyright (c)2015 17.0.4."`
repeated as a rolling cycle. The byte at the very start of each
0x200-aligned sector (`0x400`, `0x600`, `0x800`, `0xA00`, `0xC00`,
`0xE00`) is a phase index that selects which rotation of the 38-byte
string the sector starts at. Last four bytes (0x0FFC..0x1000) are the
CRC-32 footer (§2.2).

## 4. Page 1 ... N - "encrypted" payload

Every page after page 0 observed so far is high-entropy (Shannon
entropy near 8 bit/byte) in isolation.

### 4.1 The obfuscation is *not* per-file keyed

Comparing pages of two unrelated `.QBW` files from the same
QuickBooks distribution (see `re/xor_test.py`):

| Metric                                              | Observed | Random-noise expectation |
| --------------------------------------------------- | -------- | ------------------------ |
| Pages byte-identical between the two files          | 14.2 %   | ~0 %                     |
| Mean zero-bytes per 4096-byte XOR(page\_A,page\_B)  | 2460.4   | 16.0                     |
| Mean Shannon entropy of XOR(page, page) (first 64)  | 0.247 b  | 8.000 b                  |

→ Whatever transformation produces the on-disk ciphertext is
**deterministic in `(page_number, byte_offset)` alone**. The per-file
`file_id_lo` at 0x08 is *not* an encryption key; two files with
different `file_id_lo` values still produce the same ciphertext for
the same (page, offset) wherever their plaintexts coincide. See
[C.8](https://github.com/Sigilweaver/OpenQBW/blob/main/re/NOTES.md#c8--encryption-is-file-independent--2026-04-19).

### 4.2 Arithmetic-progression residue in "empty" pages

Several data pages exhibit byte sequences of the form
`byte[i] ≡ (S · i + C) (mod 256)` for a page-specific step `S` and
constant `C`. Example (file #0, page 6, offsets 0x00..0x1F):

```
  63 66 69 6c 6f 72 75 78 7b 7e 81 84 87 8a 8d 90
  93 96 99 9c 9f a2 a5 a8 ab ae b1 b4 b7 ba bd c0
```

Exactly matches `(3 · offset + 0x63) mod 256` with zero residual.
Across the first 100 pages of that file the best-fit `(S, C)` pair
covers up to **~13 %** of each page's bytes in tight runs. Candidate
explanations (untested):

- SAP SQL Anywhere's "free sector" / "unused slot" fill pattern - 
  an on-disk marker pattern analogous to the page-0 copyright
  fingerprint, but numeric.
- B-tree slot-offset arrays naturally forming arithmetic sequences
  when slots are packed at a regular stride.

Together with §4.1 this strongly suggests the payload is **not**
encrypted at all: pages 1..N are genuine SA 17.0.4 on-disk pages,
and the "randomness" is simply SA's binary structure (per-page CRC,
LSN, hashed b-tree index entries, numeric fill patterns). Formal
refutation/confirmation is the next milestone.

### 4.3 Next experiments

- Attempt to parse page 1 assuming a documented SA page-header
  layout (type, size, LSN, prev/next pointers).
- Decode the SA `SYSTABLE` catalog root pointer, which in SA is at a
  known well-known offset in the superblock (candidate offsets 0x20,
  0x24 of our page 0 - both vary per file).
- Locate `SYSCOLUMN` / `SYSUSER` / `SYSDOMAIN` to validate that we
  are reading real SA structures.

## 5. Record / row encoding

Partially understood. SYSTABLE rows carry an 8-byte invariant record
tag immediately before the `table_name` column:

```
05 00 00 00                    row marker (constant on all SYSTABLE rows)
<table_id u32_LE> 00 00 00 00  32-bit id, zero-padded to 8 bytes
b1 0d 19 0d 00 00 00 00        fixed record tag
<name_len u8> <name ASCII>
...                              trailer (see below)
```

The trailer immediately after the name contains monotone sequences
of table-local fields. On consecutive rows of the SA system schema
the `u32_LE` at offset +21 past the name increments by one per
table, matching the SA `object_id` space; the `u16_LE` at offset +6
(`4e 3c`, `4e 3e`, `4e 40`, `4e 42` ...) also moves monotonically.
The trailer's `col_count` (+6), `data_root_page` (+34, first/leftmost
leaf of the table's B-tree), and `last_page` (+50, rightmost leaf) are
now identified and extracted (`crates/openqbw/src/systable.rs`); actual
B-tree traversal from `data_root_page` is not implemented, so these
still only drive the position-heuristic attribution in
`page_attribution.rs`, not a real leaf walk. The rest of the trailer's
variable-field layout past `last_page` is still open.

Rows smaller than the SYSTABLE header padding, non-SYSTABLE tables,
and rows in other system tables (SYSCOLUMN, SYSINDEX, ...) have not
been decoded yet - their record tags may differ.

Our reader anchors purely on the 21-byte tag (`05 00 00 00 <tid> 00
00 00 00 <magic 4B> 00 00 00 00 <name_len>`) and scans forward from
there; it never assumes the tag is the start of the underlying SA
slotted-page row. That matters because it isn't: on a QuickBooks
Enterprise 24.0 file, the row is length-prefixed and begins 34 bytes
*before* the tag (openqbw#16, reported by @pete-green). A reader that
assumed the tag was the row start would land mid-row. This doesn't
affect the current tag-anchored scan, but matters for any future work
that needs the row's true boundaries (e.g. computing row length from
the slot array, per §6's open prelude-field questions).

## 6. Slotted catalog pages

Deobfuscated catalog pages follow the classic SAP SQL Anywhere
**slot-directory** layout: variable-size rows live in the page body
and a descending array of little-endian row offsets points at them.

Observed structure on the Rock Castle sample:

- **Page 2** (type `A`, index catalog): slot scan starts at `0x069`,
  the actual array starts at `0x06B` after a zero sentinel, there are
  **39 slots**, and the smallest row offset is `0x0076`. Prelude bytes
  immediately before the array decode as
  `[0x0404, 0x30C6, 0x0076, 0x0027, 0x0001, 0x0000]`.
- **Page 11** (type `E`, SA procedure catalog): slot scan starts at
  `0x06F`, array starts at `0x071`, there are **124 slots**, and the
  smallest row offset is `0x01C2`. Prelude bytes contain
  `[0x0404, 0x7265, 0x01C2, 0x007C, 0x05B4, 0x0000]`; the leading
  `0x7265` is likely row tail, not header.
- **Page 340** (type `E`, system-table list): slot scan starts at
  `0x09B`, array starts at `0x09D`, there are **44 slots** of which
  **2 are deleted** (`0x0000` interior entries), and the smallest live
  row offset is `0x01F8`. Prelude bytes decode as
  `[0x0304, 0x022C, 0x01F8, 0x002C, 0x0001, 0x0000]`.

In all three pages:

- the slot array is monotonic decreasing;
- the slot-count field in the prelude matches the number of slot
  entries, not just live rows;
- the array may start on an **odd byte boundary**;
- a zero sentinel word may appear immediately before the first slot.

The precise semantics of the non-count prelude fields are not yet
assigned.

## 7. Encryption & obfuscation

There is **no keyed encryption layer**. Three independent lines of
evidence (C.8, C.10, C.11) rule it out. But there *is* a lightweight
obfuscation layer: an 8-bit per-sector arithmetic progression added
mod 256 to the SA plaintext (C.14, §2.2c). Removing it is a
purely-deterministic per-(page, sector) operation, not a key, and
recovers genuine SAP SQL Anywhere 17.0.4 page contents.

Evidence:

1. **Pairwise file comparison.** 14 % of pages are byte-identical
   between two unrelated files; the XOR has entropy 0.25 bit/byte.
   (C.8.)
2. **Template clustering.** The 112-file corpus partitions into 6
   byte-identical schema templates. Pages 0..200 are shipped
   identically with every new company file in the cluster. (C.10.)
3. **Block analysis.** The most common 16-byte block on disk is
   `00...00` (540 occurrences across 20 random files × 50 pages).
   The second-most-common blocks are literal ASCII fragments of
   the SAP copyright string. Any deterministic keyed encryption
   (AES-ECB, XOR-with-fixed-keystream, ...) would destroy this
   signature. (C.11.)
4. **Plaintext recovery.** Subtracting the per-sector AP fill from
   page 11 of Rock Castle reveals ASCII strings `sp_columns`,
   `sp_password`, `sp_addmessage`, `sp_addlogin` and a monotonic-
   decreasing u16 slot directory - real SA catalog data. (C.14.)

The on-disk bytes of the payload are the logical SAP SQL Anywhere 17
pages, obfuscated by an additive per-sector arithmetic progression
whose `(base, step)` is a function of `(page_number, sector_index)`
alone. No secret key is required at any point.

## 8. Open questions

1. ~~What is `flags_06`? Why 0x09 vs 0x49 and nothing else?~~ Partly
   answered (openqbw#16, @pete-green): it's a bitfield over base `0x09`,
   not an enum - `0x49 = 0x09|0x40` (bit 6), and a QuickBooks Enterprise
   24.0 file adds `0x29 = 0x09|0x20` (bit 5). See
   `opensqlany::Superblock::flags_06_variant_bits`. What any given bit
   *means* is still unknown.
2. ~~Is `version_a = 201` / `version_b = 12` an ASA schema version
   pair?~~ Answered no (openqbw#16, @pete-green): the triple
   (`version_a=201`, `version_b=12`, `format_major=3`) is identical
   across every file tested from QuickBooks 2018 through Enterprise
   2024, including different product editions - an engine-format
   constant, not a version signal.
3. Are pages 1..127 really reserved (§3.2), and what do they hold?
4. Do `.TLG` files share the page-0 superblock, or do they use a
   completely different journal format?
5. What do the non-count prelude fields mean on slotted pages
  (`0x0404`, `0x30C6`, `0x0304`, `0x022C`, `0x0001`, `0x05B4`, ...)?
6. Why do QB UI pages such as pn 1328/1329 not expose a clean slot
  directory under the current per-sector recovery model?

## 8a. Template clustering (C.10)

Hashing page 3 of every file in the corpus produces only **6 distinct
hashes** across 112 files. The clusters line up with the QuickBooks
distribution + flavour:

| files | hash prefix | composition |
|------:|-------------|-------------|
|    55 | `a68289bc...` | 11 files each from five QB distributions (2018, 2020, 2021, 2022, ...) |
|    24 | `8fc7be19...` | 4-5 files each from the same five distributions |
|    20 | `a536c856...` | 4 files each from the same five distributions |
|     8 | `1b4a8402...` | `Mastering QuickBooks for Contractors` only |
|     3 | `ef33a71f...` | `Mastering QuickBooks for Contractors` only |
|     2 | `298853cb...` | `UserBooks2018E-2` only |

Within each cluster, page 3 is **byte-identical**, as are many other
pages in the 0..200 range. This is unmistakably a shipped
schema/template image: every new company file is seeded from one of a
small set of fresh SA databases that already contain the QuickBooks
system catalog (SYSTABLE, indexes, stored procedures, ...) and then the
user's data is written on top.

Consequences for the parser:

- The schema is (mostly) static and discoverable. Once one template's
  catalog is decoded, the result generalises to every file in the
  cluster and largely to the others.
- This reinforces C.8: the bytes shared across files cannot be
  encrypted with a per-file key.

Reproduce with `re/template_clusters.py`.

## 9. Change log

- **2026-04-19 · C.1-C.4** - Initial specification: corpus survey,
  4 KiB page size, `0xDA7ABA5E` magic, version triple `{3, 201, 12}`,
  plaintext collation block, conservation map of the first 64 bytes
  of the superblock.
- **2026-04-19 · C.5-C.7** - Universal per-page CRC-32 footer
  (456 521 / 456 521 pages pass). Engine fingerprinted as SAP SQL
  Anywhere 17.0.4 build 2182 via the rolling copyright string at
  page-0 offsets 0x400..0xFFC. Secondary page-0 structure block at
  0x2BC identified.
- **2026-04-19 · C.8-C.9** - Pairwise file comparison shows the
  per-(page, offset) ciphertext is **file-independent** (14 % of
  pages byte-identical across unrelated files; XOR residue has
  entropy 0.25 bit/byte). Arithmetic-progression residue of the
  form `byte[i] = (S·i + C) mod 256` observed in "empty" data
  pages. Working hypothesis: pages 1..N are plain SAP SQL Anywhere
  17 pages with no encryption layer.
- **2026-04-19 · C.10** - Hashing page 3 of every file partitions the
  corpus into exactly **6 byte-identical template clusters** aligned
  with QB distribution/flavour. Proves the "system" pages are a
  shipped schema image seeded at company-file creation, not
  per-file-keyed ciphertext. See §8a.
- **2026-04-19 · C.11** - 16-byte block analysis across 20 × 50 pages
  finds the most-frequent block on disk is all-zeros and the next
  most-frequent are literal ASCII fragments of the SAP copyright
  string. **Definitively no encryption layer**; §7 rewritten.
- **2026-04-19 · C.12** - Universal 12-byte page trailer at
  `0xFF0..0xFFB` (just before the CRC): `page_type` byte at 0xFF2
  ('E'/'A'/'M'/'H'/'C'/'@'/'I'/'G'), two reserved-zero regions, and
  per-type metadata bytes. Zero invariant failures across 456 409
  pages / 112 files. See §2.2a.
- **2026-04-19 · C.13** - Pages decompose into 8 physical 512-byte
  sectors. For `'C'` / `'H'` / `'M'` pages, the middle 6 sectors
  (3 KiB) are pure arithmetic-progression fill with no information;
  real content lives only in sectors 0 and 7 (~1 KiB). The fill is
  a function of `(page_number, sector_index)` - file-independent - 
  which is consistent with C.8's pairwise-XOR observations. See
  §2.2b.
- **2026-04-19 · C.14** - The AP fill is an **additive keystream**:
  `stored[i] = (base + i·step + plaintext[i]) mod 256`. Subtracting
  the per-sector AP recovers SAP SQL Anywhere plaintext - ASCII
  procedure names (`sp_columns`, `sp_password`, `sp_addmessage`,
  `sp_addlogin`) and a monotonic u16 slot directory on page 11 of
  Rock Castle. §7 rewritten; §2.2c added.
- **2026-04-19 · C.15** - Bulk plaintext extraction. Deobfuscating
  the first 500 type-`E` pages of Rock Castle and filtering for
  printable ASCII runs ≥ 6 chars yields **9 503 unique strings**,
  including the full SAP SQL Anywhere system catalog (`SYSCOLUMN`,
  `SYSINDEX`, `SYSPROCEDURE`, `SYSTABLE`-family, `SYSCATALOG`,
  `SYSFOREIGNKEY`, `SYSJAVACLASS`, ...) **and** QuickBooks-specific
  Java class names (`QBAccountingGroup`, `PayrollGroup`,
  `VerifyPriceRulesOrphanItems`, `VerifySiteItemIdMatches`,
  `emp_Pay_Item_Id_Num`). This corroborates C.14 at corpus scale
  and confirms the payload is a genuine SA 17 database with an
  Intuit schema overlaid. Tool: `re/strings_plain.py`.
- **2026-04-19 · C.16** - Catalog + UI pages located. On Rock
  Castle, page 2 (type `A`) is the SA **index catalog** (ISYS*
  names); page 340 (type `E`) holds the SA **system-table list**
  (SYSTABLE, SYSCOLUMN, SYSCATALOG, ...); pages 1328 / 1329 / 1337
  carry QuickBooks **UI strings** - the Home-screen menu for
  Company Center, Customer Center, Create Invoices, Pay Bills,
  Chart of Accounts etc., with references to `qbwin32.dll` and
  `qbw:centers?...` URLs. Row-data pages for QB entities begin at
  pn ≈ 1289. Tool: `re/find_string.py`.
- **2026-04-19 · C.17** - Slotted-page directories parsed.
  `re/page_layout.py` now locates the descending u16 row-offset array
  on deobfuscated SAP catalog pages even when the array is odd-aligned
  and preceded by a zero sentinel. Verified on Rock Castle:
  page 2 (`A`) has 39 slots, page 11 (`E`) has 124 slots, and page
  340 (`E`) has 44 slots with 2 deleted entries. The prelude bytes
  immediately before the array contain the minimum row offset and the
  slot count, but other fields remain unidentified.
- **2026-04-19 · C.18** - SYSTABLE row tag identified and catalog
  scanned. `re/systable_scan.py` extracts `(table_id, name)` pairs
  from every deobfuscated `E` page matching the invariant tag
  `b1 0d 19 0d 00 00 00 00` preceded by `05 00 00 00` and the row
  `table_id u32_LE`. On `B22_Sample.qbw` the scan recovers 114
  unique tables spread across 10 pages - all SA system/diagnostic
  catalog, no QuickBooks user tables. On two other files in the
  corpus (`B22_Chapter 3`, `B22_Chapter 5`) the tag appears on zero
  pages because `fill_overlay.recover_base` misfires on data-dense
  sectors. Page-walking to QB user tables is blocked on C.19.
  `fill_overlay.recover_base` also rewritten from O(256·n) to O(n)
  via a residue histogram - ~40× speed-up on full-corpus scans.
