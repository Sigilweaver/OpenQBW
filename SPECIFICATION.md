# QBW File Format Specification (Draft)

**Status:** incomplete, in-progress reverse engineering. Every numeric
claim in this document is backed by a reproducible observation in
[`re/NOTES.md`](re/NOTES.md) against the corpus manifest
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

Page 0 is the only page that contains plaintext structure. Pages 1..N
are high-entropy (encrypted). Page 0 ends with several plaintext
collation names — see §3.3.

## 3. Page-0 superblock

The first 64 bytes of every `.QBW` are a fixed-layout header. Byte
roles were determined by the conservation map below (`.` = byte is
identical across all 112 files, `f` = 2–4 distinct values (flag),
`V` = high-cardinality variable):

```
 off   0  1  2  3  4  5  6  7  8  9  a  b  c  d  e  f
0x00   .  .  .  .  .  .  f  .  V  V  .  .  .  .  .  .
0x10   .  .  .  .  .  .  .  .  .  .  .  .  V  V  .  .
0x20   V  V  .  .  V  V  V  V  V  f  V  V  V  .  .  .
0x30   .  .  .  .  .  .  .  .  .  .  .  .  .  .  .  .
```

### 3.1 Superblock header (offsets 0x00 – 0x2F)

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

### 3.3 Plaintext collation / codepage block (≈ offsets 0x162 – 0x1FF)

Near the end of the 4 KiB superblock there is a plaintext region that
contains the SAP SQL Anywhere collation names:

```
0x0162  57 02 00 00 31 32 35 32 4c 41 54 49 4e 31 00 …    |W...1252LATIN1..|
0x01B8  77 69 6e 64 6f 77 73 2d 31 32 35 32 00 …           |windows-1252..|
0x01D4  55 43 41 00 …                                      |UCA..|
0x01FC  55 54 46 2d …                                      |UTF-|  (probably UTF-8)
```

- `1252LATIN1` — default / CHAR collation.
- `windows-1252` — CHAR codepage label.
- `UCA` — Unicode Collation Algorithm (alternate / NCHAR collation).
- `UTF-8` (truncated in our window) — NCHAR codepage label.

These four strings, in this order, are the SAP SQL Anywhere "language
/ collation" record written verbatim into the ASA database header.
Their presence in every `.QBW` at the same position is the strongest
single piece of evidence that `.QBW` payload is an obfuscated SQL
Anywhere database image.

## 4. Page 1 … N — encrypted payload

Every page after page 0 observed so far is high-entropy. Cursory
statistical inspection has not yet revealed an obvious per-page
plaintext header. Hypotheses, all untested:

1. Pages are XOR-encrypted with a file-keyed keystream.
2. Pages are encrypted with a block cipher (DES / 3DES / AES) keyed
   by a per-file value derivable from the superblock.
3. Only a subset of pages (e.g. "data" pages, not "free" pages) are
   encrypted.

Next experiments (see `re/`):

- Entropy histogram per page for one file.
- XOR two pages of the same file and look for structure.
- XOR homologous pages of two files with the same QuickBooks version
  (e.g. two B22 files) and look for constant-plaintext diff residue.

## 5. Record / row encoding

_Unknown._

## 6. Schema / catalog

_Unknown._ Expected to follow SAP SQL Anywhere's on-disk catalog
layout once decryption is solved.

## 7. Encryption & obfuscation

_Unknown._ See §4.

## 8. Open questions

1. What algorithm decrypts pages 1..N? Is the key derived from
   `file_id_lo` (`0x08`), a constant, or a truncated password?
2. What is `flags_06`? Why 0x09 vs 0x49 and nothing else?
3. Is `version_a = 201` / `version_b = 12` an ASA schema version pair?
4. Are pages 1..127 really reserved (§3.2), and what do they hold?
5. Do `.TLG` files share the page-0 superblock, or do they use a
   completely different journal format?

## 9. Change log

- **2026-04-19 · C.1–C.4** — Initial specification: corpus survey,
  4 KiB page size, `0xDA7ABA5E` magic, version triple `{3, 201, 12}`,
  plaintext collation block, conservation map of the first 64 bytes
  of the superblock.
