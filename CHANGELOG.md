# Changelog

All notable changes to this project will be documented here. The
format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Behavior changes

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
- `SYSCOLUMN` name recovery tries every default-value length and requires the
  default's length byte to agree. A column whose default value is itself a
  valid identifier (for example `is_active` with default `Y`) is counted in
  `SyscolumnStats::rows_ambiguous_name` and left out, where earlier versions
  reported the default as the column name. Defaults of any length are
  recognized. `scan_page` takes a `&mut SyscolumnStats`.
- `SchemaRecovery` gains `candidate_owners`, `catalog_col_count`, `scan_stats`
  and `columns_short_of_catalog()`. `openqbw schema` warns about columns
  missing relative to the `SYSTABLE` count, owners that tied, and rows or pages
  it could not read.
- A `SYSOBJECT` owner whose candidate table names tie on votes is left
  unbridged instead of taking an arbitrary one.
- `build_fk_graph` returns an `FkGraph` and joins column owners to tables
  through the owner bridge; columns that cannot be placed are counted
  (`columns_unbridged`) instead of dropped, and `openqbw fkgraph` prints the
  count. Earlier versions joined on `data_root_page`, which names unrelated
  tables for most owners.
- Opening a file checks the superblock magic, the version triple (3, 201.12)
  and the CRC-32 footer of every page. A file that is not a QuickBooks
  company file (a `.QBB`, a `.TLG`, any 4 KiB-aligned file), has another
  version, or has a damaged page now fails to open with a specific error
  instead of exporting zero or spurious records. The CLI and Python
  `openqbw.open()` use `openqbw::open_store`; Python raises `FormatError`.
- Line-item scans count pages whose decoded body is indistinguishable from
  random bytes. When more than half of the data pages are opaque (as in a
  password-protected file) the scan fails with `ContentOpaque`; otherwise
  the count is reported as a `ScanWarning::OpaquePages` (CLI stderr, Python
  `UserWarning`). The 50% threshold has not been measured on a corpus.
- Python `open()` issues a `UserWarning` when page-to-table attribution is
  unavailable, and `Reader.attribution_gap` names the reason.
- `Reader.file_size` and `openqbw forensics` report the size of the opened
  store instead of 0 when file metadata cannot be read.
- Line-item and header dates are found in a wider window (SA-days
  10000..=40000, was 13000..20000, which stopped at Oct 2013), so records
  dated after Oct 2013 now have a date. When two separate date/counter pairs
  sit in the search window, the date and counter are left unset and
  `date_source` is `ambiguous` (`window_scan` when one pair was found,
  `not_found` when none). `LineItem` and `TransactionHeader` gain
  `date_source` and `flags` (`RecordFlag::DateAmbiguous`); SQLite, CSV and
  Python outputs carry them. The scan window itself is internal.
- `iter_transaction_headers` yields `Result<TransactionHeader, Error>`, and
  `scan_transaction_headers` collects them with `ScanOptions`. Python
  `Reader.transactions(*, lenient=False)` returns a `ScanReport` like
  `line_items()` (renamed from `LineItems`). `transactions.csv` gains
  `txn_date_raw`, `counter` and `date_source`. The SA-day to
  calendar conversion uses the offset documented in `date.rs`; it is fitted to
  the Rock Castle sample and not checked against a date known from another
  source.
- Line-item amounts are decoded per source table. Journal lines
  (`abmc_general_journal_inventory_lineitem`) use the sign bit, so a line
  stored as `02 c0 2d 01` is -$771.20, where earlier versions reported
  +$772.48. Invoice lines (`abmc_invoice_inventory_lineitem`) are unsigned.
  Lines on any other table, lines without a source table, and lines with
  amount type `0x00`, `0x01` or `0x03` have no amount (`amount_cents` is
  `None` / NULL), where earlier versions reported an unsigned reading or 0.
- `LineItem::amount_cents` is `Option<i64>` and holds the amount under
  `amount_encoding`; `amount_cents_signed` and `AmountType::decode_cents*`
  are removed. `AmountType::OneByteOne` is `Unestablished`, `AmountType::Other`
  is removed (an unknown byte is an error), and `LineItem::amount_type_byte()`
  gives the raw byte. `LineItem` gains `amount_encoding` and `flags`
  (`RecordFlag`). `LineItem`, `AmountType`, `AmountEncoding`, `RecordFlag` and
  the other public result and option types are `#[non_exhaustive]`.
- One crate `Error` (with `ErrorKind`, `page_number()`, `page_offset()`)
  replaces `LineItemError`; no `opensqlany` type appears in it. Page-store
  failures map to `Error::Io`, `BadMagic`, `BadCrc` or `Store`.
- `iter_lineitems` and `iter_lineitems_with_attribution` yield
  `Result<LineItem, Error>`. An anchor followed by an unknown amount-type
  byte, or one whose amount runs past the page data area, is an error
  (`UnknownAmountType`, `TruncatedAmount`) instead of a line item with a zero
  or unknown amount. A page read failure is an error instead of ending the
  scan.
- New `scan_lineitems(store, model, attribution, &ScanOptions)` returns a
  `ScanResult` with `items` and `warnings`. `ScanOptions` holds a
  `Strictness`; `Strict` (the default) returns the first error and `Lenient`
  turns each into a `ScanWarning`.
- The CLI stops at the first such error. `--lenient` skips those records,
  prints each warning on stderr, and lists them (`scan_rejections` table in
  SQLite, `rejected.csv` in CSV exports).
- Python `Reader.line_items(*, lenient=False)` returns a `LineItems` result
  (`items`, `rejected`, `warnings`; iterable) instead of a list. An
  undecodable record raises `RecordError`; with `lenient=True` it is listed
  in `rejected` and a `UserWarning` is issued. New exception hierarchy under
  `OpenQBWError`: `FormatError`, `RecordError`, `ContentError`; each carries
  `kind`, `page_number` and `page_offset`. The item dicts gain
  `amount_encoding`, `amount_type`, `amount_raw`, `txn_date_raw`, `counter`
  and `flags`, and lose `amount_cents_signed`.
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
  version (1); `scan_rejections` stores `kind` and `message`.
- `openqbw migrate --format iif` exits with an error. `--placeholder-accounts`
  writes the file with placeholder accounts and leaves out, and counts,
  transactions that have an undecoded amount.

### Fixed

- The opaque-page count measures entropy on the decoded page body. It
  measured the stored AP ciphertext, which is always near 8 bits/byte, so every
  data page of a readable file counted as opaque and the scan failed with
  `ContentOpaque`. `count_opaque_pages` takes the `ApModel` used to decode
  the pages.

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
