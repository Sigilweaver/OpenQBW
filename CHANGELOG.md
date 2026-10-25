# Changelog

All notable changes to this project will be documented here. The
format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Built on opensqlany 0.3 (pinned to a git revision until it is published).
`Cargo.lock` is committed. The minimum Rust version is 1.95.

### Behavior changes

- Opening a file checks the superblock magic, the version triple (3, 201.12),
  the `flags_06` base bits, and the CRC-32 footer and reserved trailer bytes
  of every page. A file that is not a QuickBooks company file (a `.QBB`, a
  `.TLG`, any 4 KiB-aligned file), has another version, or has a damaged page
  fails to open with a specific error (`BadMagic`, `UnsupportedVersion`,
  `UnsupportedFlags`, `BadCrc`, `BadTrailer`) instead of exporting zero or
  spurious records. A file whose keystream cannot be calibrated at all is an
  error (`Store`) instead of decoding with `bv = 0`.
- Reads are strict by default in the library, the CLI and Python: a record
  or page that cannot be decoded is an error. `ReadOptions::lenient()`
  (`--lenient`, `lenient=True`) skips each one and lists it in `rejected`
  (`scan_rejections` in SQLite, `rejected.csv` in CSV). This now applies to
  every command, including `catalog`, `schema` and the catalog read at open,
  where a page that failed to decode used to end the scan silently.
- Every page is decoded through one path. A page whose keystream opensqlany
  confirms is read as before. A page where exactly one `bv` decodes the
  QuickBooks C.36 anchor is read with that key (earlier versions took the
  first of several). Any other `E` page is decoded with opensqlany's
  best-scoring key, and every record read from it carries
  `RecordFlag::KeystreamUnverified`; a record whose bytes touch a sector
  whose step opensqlany could not determine carries
  `RecordFlag::LowConfidenceSector`. Scans count both in a run-level
  warning. The OpenQBW byte-zero oracle and brute search are no longer used
  to decode pages.
- Slot directories must be confirmed by the page prelude. Header pages
  without a confirmed directory use the linear scan, whose records carry
  `HeaderIdAmbiguous`, and are counted in an `unconfirmed_directory_pages`
  warning. `SYSCOLUMN` pages without one are skipped and counted in
  `SyscolumnStats::pages_unconfirmed_directory`, and `openqbw schema` prints
  the count. Not measured on a corpus: opensqlany reports that about a fifth
  of `E` pages with a directory are not confirmed.
- A line-item anchor in a page's free space (between a confirmed slot
  directory and the lowest live row) is left out and counted in a
  `records_outside_live_rows` warning; earlier versions exported such
  residue of deleted or moved records as live line items. Pages without a
  confirmed directory keep every anchor, and freed pages are still read,
  because the allocation map is not decoded. Not measured on a corpus.
- Line items take the nearest `04 00 10 <id>` item ID instead of the farthest
  in the 96 bytes before the anchor, and their search windows stop at the
  neighbouring records' anchors, so a record no longer reports the previous
  record's item ID or date.
- Transaction headers whose row holds more than one 16-character ID in the
  header shape, and all headers on pages without a readable slot directory,
  carry `RecordFlag::HeaderIdAmbiguous`.
- `SYSTABLE` trailer fields (`col_count`, `data_root_page`, `last_page`) are no
  longer read from the page trailer bytes at 0xFF0 and above; a row too close
  to the end of the page data area gets `None` for fields that do not fit.
- A `SYSTABLE` row whose `data_root_page` is past the end of the file or not
  a data (`E`) page carries `RecordFlag::RootPageInvalid` and is not used for
  page-to-table attribution; the catalog carries an `invalid_root_pages`
  warning. Earlier versions let such a value move the attribution boundaries
  of every page above it.
- `SYSCOLUMN` name recovery tries every default-value length and requires the
  default's length byte to agree. A column whose default value is itself a
  valid identifier (for example `is_active` with default `Y`) is counted in
  `SyscolumnStats::rows_ambiguous_name` and left out, where earlier versions
  reported the default as the column name. Defaults of any length are
  recognized.
- `openqbw schema` warns about columns missing relative to the `SYSTABLE`
  count, owners that tied, and rows or pages it could not read.
- A `SYSOBJECT` owner whose candidate table names tie on votes is left
  unbridged instead of taking an arbitrary one.
- The foreign-key graph joins column owners to tables through the owner
  bridge; columns that cannot be placed are counted (`columns_unbridged`)
  instead of dropped, and `openqbw fkgraph` prints the count. Earlier
  versions joined on `data_root_page`, which names unrelated tables for most
  owners.
- Line-item scans count pages whose body is indistinguishable from random
  bytes. When more than half of the data pages are opaque (as in a
  password-protected file) the scan fails with `ContentOpaque`; otherwise
  the count is reported as an `opaque_pages` warning. The 50% threshold has
  not been measured on a corpus.
- When page-to-table attribution is unavailable, the catalog, line-item and
  header scans carry an `attribution_gap` warning.
- Line-item and header dates are found in a wider window (SA-days
  10000..=40000, was 13000..20000, which stopped at Oct 2013), so records
  dated after Oct 2013 now have a date. When two separate date/counter pairs
  sit in the search window, the date and counter are left unset and
  `date_source` is `ambiguous` (`window_scan` when one pair was found,
  `not_found` when none), with `RecordFlag::DateAmbiguous`. SQLite, CSV and
  Python outputs carry `date_source` and `flags`; `transactions.csv` gains
  `txn_date_raw`, `counter` and `date_source`. The SA-day to calendar
  conversion uses the offset documented in `date.rs`; it is fitted to the
  Rock Castle sample and not checked against a date known from another
  source.
- Line-item amounts are decoded per source table. Journal lines
  (`abmc_general_journal_inventory_lineitem`) use the sign bit, so a line
  stored as `02 c0 2d 01` is -$771.20, where earlier versions reported
  +$772.48. Invoice lines (`abmc_invoice_inventory_lineitem`) are unsigned.
  Lines on any other table, lines without a source table, and lines with
  amount type `0x00`, `0x01` or `0x03` have no amount (`amount_cents` is
  `None` / NULL), where earlier versions reported an unsigned reading or 0.
- An anchor followed by an unknown amount-type byte, or one whose amount runs
  past the page data area, is an error (`UnknownAmountType`,
  `TruncatedAmount`) instead of a line item with a zero or unknown amount.
- Line items are read only when the field after the parent ID is
  `float32(1.0)`, which the module documentation calls the quantity. An
  anchor with any other plausible value there is now counted in a
  `line_items_not_read` warning, because such a line is missing from the
  results and from every total; earlier versions left it out silently.
  Whether the field is the quantity is not confirmed.
- Line items are scanned to the end of the page data area (0xFF0, was 0xFE0).
- SQLite export: `transactions.total_cents` is NULL when any line of the
  transaction has no decoded amount; `unresolved_lines` counts them.
  `transaction_line_items` gains `amount_encoding` and `amount_raw_u24_cents`
  (the undecoded unsigned reading, kept for the `verify` regression baseline)
  and loses `amount_cents_signed`. Export summaries state how many
  transactions and line items are excluded from the grand total.
- CSV export: `amount_signed_cents` is replaced by `amount_encoding`;
  `amount_decimal` is computed from integer cents. `rejected.csv` is always
  written, with a `kind` column, and a `manifest.json` records
  `format_version`.
- SQLite export: a `meta` table and `PRAGMA user_version` record the format
  version (1); `scan_rejections` stores `kind` and `message`. A lenient
  export that skipped a record no longer fails while writing
  `scan_rejections`.
- SQLite export: `source_table` is NULL instead of an empty string when a
  line's page (or every line of a synthesized transaction) is not
  attributed to a table. `transaction_line_items` has an `id` primary key and
  a unique `(page_number, page_offset)` in place of the primary key
  `(qb_id_parent, line_number, source_table)`.
- CSV export: `manifest.json` lists every file written with its row count
  (`files`), next to `format_version` and `openqbw_version`; the top-level
  `lineitems` and `rejected` counts are replaced by it.
- `openqbw verify` exits with status 3 when a check reports DIFF (it exited
  0). `--no-regression-baseline` skips the Rock Castle line-item baseline.
  It refuses an SQLite export that has no `meta.format_version` or a newer
  version than it reads, and a row read error while comparing attribution is
  an error instead of shrinking the compared page set.
- `openqbw migrate --format iif` exits with an error. `--placeholder-accounts`
  writes the file with placeholder accounts and leaves out, and counts,
  transactions that have an undecoded amount.
- `openqbw forensics` reports the size of the opened store instead of 0 when
  file metadata cannot be read.

### API breaks

- Python 3.10 or later is required (was 3.9, which reached end of life in
  October 2025); wheels use the 3.10 stable ABI.
- `Reader::open(path, &ReadOptions)` (and `Reader::from_bytes`) is the entry
  point. It owns the page store, the keystream model, the catalog and the
  page attribution; `line_items()`, `transactions()`, `indexes()`,
  `columns()`, `schema(table)`, `catalog()` and `attribution_gap()` read
  from it. No `opensqlany` type appears in the public API. Removed:
  `open_store`, `check_store`, `OpenOptions`, `learn_model`,
  `iter_lineitems`, `iter_lineitems_with_attribution`, `scan_lineitems`,
  `iter_transaction_headers`, `scan_transaction_headers`,
  `iter_systable_entries`, `collect_unique`, `iter_sysindex`,
  `collect_unique_sysindex`, `iter_syscolumns`, `scan_syscolumns`,
  `collect_unique_syscolumns`, `schema_for`, `recover_schema`,
  `PageAttribution::build`, `bridge_owners_via_backref`,
  `extend_with_sysobject`, `bridge_owners_to_tables`, `count_opaque_pages`
  and `OpaqueCount`.
- `ReadOptions` holds a `Leniency` set (`Leniency::STRICT`, the default, and
  `Leniency::RECORDS`) in place of `ScanOptions` and `Strictness`.
- Scans return `Scan<T>` (`items`, `rejected: Vec<Error>`,
  `warnings: Vec<Warning>`) in place of `ScanResult` and `ScanWarning`.
  `Warning` has a `WarningKind` (`OpaquePages`, `KeystreamUnverifiedPages`,
  `LowConfidenceSectorPages`, `UnconfirmedDirectoryPages`, `AttributionGap`,
  `Other(code)`) with a stable `as_str()`.
- One crate `Error` with `kind()` (`ErrorKind`, stable `as_str()`),
  `page_number()` and `page_offset()` replaces `LineItemError`. New variants
  `UnsupportedFlags` and `BadTrailer`. Struct-like variants are
  `#[non_exhaustive]`.
- `LineItem::amount_cents` is `Option<i64>` and holds the amount under
  `amount_encoding`; `amount_cents_signed` and `AmountType::decode_cents*`
  are removed. `AmountType::OneByteOne` is `Unestablished`, `AmountType::Other`
  is removed, and `LineItem::amount_type_byte()` gives the raw byte.
  `LineItem` and `TransactionHeader` gain `amount_encoding` (line items),
  `date_source` and `flags`; `SysTableEntry`, `SysIndexEntry` and `SysColumn`
  gain `flags`.
- Every public struct and enum is `#[non_exhaustive]`.
- Reverse-engineering cross-checks moved to `openqbw::diagnostics`, which is
  outside semantic versioning: `fk_graph`, `nulls_flag_histogram`,
  `content_attribution`, `schema_attribution`, `sysindex_cross_validation`,
  `learned_block_count` and their result types (formerly `build_fk_graph`,
  `fk_graph_stats`, `nulls_flag_histogram`, `ContentAttribution::build`,
  `SchemaAttribution::build`, `CrossValidation` and friends at the crate
  root). Their methods take `&Reader` instead of a page store and model.
- `SchemaRecovery` gains `candidate_owners`, `catalog_col_count`,
  `scan_stats`, `pages_rejected`, `warnings` and
  `columns_short_of_catalog()`. `SyscolumnStats` gains
  `pages_unconfirmed_directory`; `scan_syscolumn_page` takes a
  `&mut SyscolumnStats`. `OwnerBridge` and `FkGraph` gain `pages_rejected`.
- `AttributionGap` gains `as_str()`.
- Python: `openqbw.open(path, *, lenient=False)`; the options are
  keyword-only and apply to every read from the returned `Reader`, so
  `line_items()` and `transactions()` no longer take `lenient`. Every method
  (`tables()`, `indexes()`, `line_items()`, `transactions()`) returns a
  `ScanReport` (a sequence, with `items`, `rejected` and `warnings`) of
  frozen record objects (`Table`, `Index`, `LineItem`, `Transaction`) with
  attributes and `to_dict()`, in place of lists of dicts. Warnings are
  `ReadWarning` objects (`kind`, `message`) and are also issued as
  `openqbw.OpenQBWWarning`. `Reader.attribution_gap` is a snake_case code.
- The crate root holds the reading API only: `Reader`, `ReadOptions`,
  `Leniency`, `Scan`, `Warning`, `WarningKind`, `RecordFlag`, `Error`,
  `ErrorKind`, the record types, `SchemaRecovery`, `SyscolumnStats`,
  `BridgeSource`, `PageAttribution` and `AttributionGap`. The keystream
  oracles (`recover_bv_*`, `oracle_bv_e_page`, `deobfuscate_with_bv`,
  `APAGE_MAGIC`), the per-page row scanners (`scan_systable_page`,
  `scan_sysindex_page`, `scan_syscolumn_page`), the anchor and threshold
  constants, `is_opaque_high_entropy`, `OwnerBridge`, `BackrefVotes` and
  `SUPPORTED_VERSION` moved to `openqbw::diagnostics`.
- The date conversion is internal: `DATE_EPOCH_DAYS_BEFORE_UNIX`,
  `SA_DAY_MIN_PLAUSIBLE`, `SA_DAY_MAX_PLAUSIBLE`, `sa_day_to_unix_day`,
  `sa_day_to_unix_seconds` and `unix_day_to_sa_day` are removed from the
  API; use `LineItem::txn_date_days_since_unix` or the new
  `TransactionHeader::txn_date_days_since_unix`, documented as best effort,
  or the stored `txn_date_raw`.
- CLI: `fkgraph`, `nulls`, `validate-attribution`, `indexes` and
  `forensics` moved under `openqbw debug`, whose names, flags and output
  may change in any release. `export`, `catalog`, `schema`, `migrate` and
  `verify` stay at the top level.
- Python exceptions: `OpenQBWError` derives from `ValueError`;
  `OpenQBWIOError(OpenQBWError, OSError)` replaces the plain `OSError` for
  unreadable files; `FormatError`, `RecordError` (now a `FormatError`) and
  `ContentError` (now a `FormatError`) carry `kind`, `page_number` and
  `page_offset`. The package ships `.pyi` stubs and `py.typed`; the native
  module is `openqbw._openqbw`.

## [0.1.6] - 2026-10-04

### Fixed

- Schema gap diagnostics count distinct plausible column ordinals, preventing
  duplicate names at the same position from underflowing or hiding gaps.
- Equally supported back-reference layouts remain unresolved, allowing schema
  recovery to use the independent `SYSOBJECT` scan instead of guessing.
- `schema_for` on 0.1.5 could return different columns for the same table
  on repeated runs, sometimes another table's columns, because the
  `SYSOBJECT` name bridge was read from an unordered map. 0.1.6 returns the
  same result on every run, and a table whose owner cannot be bridged
  returns no columns.

- `openqbw schema` could not recover the columns of most tables the
  `catalog` command lists. Owners were bridged to table names only by
  the `SYSOBJECT` name scan, which needs the table's name to appear a
  second time in the file, so on the TLR 2021/2022 practice files it
  reached 115-144 of ~650 names - `abmc_invoice_header`, whose name
  appears exactly once, was not among them. `SYSTABLE` rows carry the
  `SYSCOLUMN` owner id themselves, 30 bytes before the row tag, and
  that back-reference is now the primary bridge: coverage goes to
  401-420 names per file, and `openqbw schema <file> abmc_invoice_header`
  returns its 46 recovered columns on both files from the report.
  Fixes #19, reported by @Balaxxe.

### Added

- `openqbw::recover_schema` and `SchemaRecovery`, the diagnosing form of
  `schema_for`: which bridge resolved the table, how many `SYSCOLUMN`
  rows and owners the file yielded, and which `column_id`s inside the
  recovered range are still missing.
- `openqbw::owner_bridge`: `bridge_owners_via_backref`,
  `extend_with_sysobject`, `OwnerBridge`, `BridgeSource` and the
  `BackrefVotes` calibrator, which picks the back-reference distance
  per file (4..=96 bytes) instead of hard-coding it, so a layout change
  in another QuickBooks version recalibrates rather than silently
  returning nothing.
- `SPECIFICATION.md` §5.1: the `SYSTABLE` row-prefix `table_id` field,
  with the cross-file evidence for it.

### Changed

- A failed `openqbw schema` lookup now reports the stage that failed -
  catalog presence, `SYSCOLUMN` recovery, bridge coverage - instead of
  claiming the `SYSTABLE` row lacks a recoverable `data_root_page`,
  which `schema` never consulted. A successful lookup names the owner
  and the bridge that found it, and warns when the recovered
  `column_id`s have holes.
- `schema` only falls back to the whole-file `SYSOBJECT` scan when the
  back-reference does not resolve the requested table, so the common
  case no longer pays for that pass.

## [0.1.5] - 2026-09-09

### Changed

- Bumped the `opensqlany` dependency to 0.2.0. Its page-trailer model was
  corrected there: the u32 at trailer `0xFF4..0xFF8` is a log sequence
  number rather than two metadata bytes plus six reserved-zero bytes, so
  `verify_trailer` no longer rejects valid pages on any file whose LSN
  has passed 65 535. No OpenQBW API changes - the renamed
  `PageTrailer`/`Superblock` fields aren't used here.

## [0.1.4] - 2026-08-12

### Added

- Docs: Python API reference page (`python-api`) covering `openqbw.open`,
  `Reader`, and its `tables()` / `indexes()` / `line_items()` /
  `transactions()` methods, registered in the sidebar. Fixes #1. (@Nabejo)
- `PageAttribution::gap()` and `AttributionGap`, distinguishing "no
  SYSTABLE rows found" from "SYSTABLE rows found, but every
  `data_root_page` is zero" - the latter observed on a QuickBooks
  Enterprise 24.0 file, where it means there's no B-tree root to anchor
  the position-heuristic attribution on (the block/extent scan
  `collect_unique` uses to find rows is unaffected and is already the
  only route to the data on these files). The CLI's `export`, `catalog`,
  and `migrate --format=csv/iif` commands now print a diagnostic
  explaining which case applies instead of leaving it to show up as
  silently-empty attribution downstream. Partially addresses #16.
  Reported by @pete-green.
- `SPECIFICATION.md` §8: answered two of the file's long-standing open
  questions from the same report - `flags_06` is a bitfield over base
  `0x09` (not an enum of magic values; a QuickBooks Enterprise 24.0 file
  adds a third value, `0x29 = 0x09|0x20`, fitting the same pattern as
  the original `0x49 = 0x09|0x40`), and `version_a`/`version_b` are not
  an ASA schema version pair - the triple is an invariant engine-format
  constant across QuickBooks 2018 through Enterprise 2024. The bitfield
  accessor itself (`Superblock::flags_06_variant_bits`) lives upstream
  in `opensqlany` and will be available here once this repo picks up
  its next release. §5 also gained a note on SYSTABLE row anchoring: on
  the same file the row is length-prefixed and begins 34 bytes before
  the tag our reader scans for, which doesn't affect the current
  tag-anchored scan but matters for any future work needing the row's
  true boundaries. Remainder of #16.
- `SPECIFICATION.md` §8b: preserved @pete-green's dead-ends writeup
  from building an independent decoder against the same file (#17) -
  the still-open multi-page `LONG VARBIT` chaining blocker (including
  the unresolved multi-copy/version-selection problem), and four
  methodology notes on validating a byte-level reader (a sector-linear
  addressing bug that survives casual inspection, why ±1 shifted-window
  controls don't discriminate on this format, an object-ID minimum-gap
  self-check, and requiring ≥3 distinct values before trusting a
  fixed-offset anchor). No code changes; recorded so the negative
  results aren't lost. Fixes #17.

### Changed

- Upgraded `pyo3` from 0.22 to 0.29, clearing RUSTSEC-2025-0020 and
  RUSTSEC-2026-0177; the `--ignore` workarounds in the audit workflow
  are removed accordingly.
- PyPI package now declares `keywords` (`quickbooks`, `qbw`,
  `accounting`, `parser`) so the package is findable via PyPI search;
  previously only the crates.io side had them.

### Fixed

- `SysIndexIter`, `SysTableIter`, `SysColumnIter`, `TransactionHeaderIter`,
  and `bridge_owners_to_tables` validated the E-page bv oracle
  (`oracle_bv_e_page`) with a `candidate[0] == 0` check that is always
  true by construction, regardless of whether the guessed bv is correct.
  Whenever the higher-confidence C.36 anchor didn't fire, a wrong guess
  was silently accepted, corrupting catalog and header page decoding and
  cascading into wrong table attribution, orphaned parents, and an
  inflated invoice total on some files. All five call sites now go
  through `recover_bv_any`, which validates with an actual zero-density
  check instead. Fixes #15. Root cause identified and diagnosed by
  @pete-green.
- CI was red on `main`: a newer `clippy` (rust 1.97) added the
  `byte_char_slices` lint, which fired on two pre-existing test-only
  byte-array literals in `attribution_schema.rs`. Rewrote them as byte
  string literals per clippy's own suggestion. Unrelated to the pyo3
  bump above - this was already broken before it.

## [0.1.3] - 2026-07-04

### Fixed

- Bundle the `LICENSE` file into the sdist so source-based installs
  and conda-forge packaging carry the license text.

## [0.1.2] - 2026-07-04

### Changed

- Release workflow now builds and publishes a source distribution
  (sdist) to PyPI alongside the wheels, enabling source-based
  installs and conda-forge packaging.

## [0.1.1] - 2026-05-31

### Added

- `CITATION.cff`: author identity (Nathan Riley + ORCID) and a
  scaffolded `identifiers:` block ready for the Zenodo concept DOI.

### Changed

- README badge block unified across the Sigilweaver portfolio.

## [0.1.0] - 2026-05-22

First publication-ready release.

### Added

- `openqbw` library: read-only parser for QuickBooks Desktop `.QBW`
  company files. Peels Intuit's additive-progression obfuscation off
  the underlying SA17 page store via the `opensqlany` companion crate,
  then enumerates user tables, parses `SYSTABLE` / `SYSCOLUMN` /
  `SYSINDEX` / `SYSOBJECT` system catalogs, and extracts invoice
  headers + line items.
- `openqbw` CLI: `inspect`, `tables`, `migrate` (CSV / SQLite / IIF),
  and `forensics` subcommands.
- `openqbw` Python wheel (`pip install openqbw`): pyo3 bindings
  exposing the high-level reader API, abi3-py39, classifiers for
  Office/Business :: Financial :: Accounting.
- `SPECIFICATION.md` covering the `.QBW` on-disk format and the
  reverse-engineering record in `re/NOTES.md`.
- Workspace metadata, MSRV 1.87, `unsafe_code = "forbid"` on
  pure-Rust crates (excluded on the pyo3 binding crate by necessity).
- CI matrix (Linux + macOS + Windows): `cargo fmt`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo test`, and a
  per-OS `maturin build` wheel job.
- Tag-triggered release workflow: publishes `openqbw` and
  `openqbw-cli` to crates.io via trusted publishing, plus
  `maturin publish` of the Python wheel to PyPI.
- `CHANGELOG.md`, `CONTRIBUTING.md`, `SECURITY.md`, `NOTICE`.
- Documentation site at <https://sigilweaver.app/openqbw/docs/>.

[Unreleased]: https://github.com/Sigilweaver/OpenQBW/compare/v0.1.6...HEAD
[0.1.6]: https://github.com/Sigilweaver/OpenQBW/releases/tag/v0.1.6
[0.1.5]: https://github.com/Sigilweaver/OpenQBW/releases/tag/v0.1.5
[0.1.0]: https://github.com/Sigilweaver/OpenQBW/releases/tag/v0.1.0
