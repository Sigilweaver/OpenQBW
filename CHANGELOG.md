# Changelog

All notable changes to this project will be documented here. The
format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Built on opensqlany 0.3.0, fetched from crates.io. `Cargo.lock` is committed.
The minimum Rust version stays 1.95; Python 3.10 or later is now required.

Breaking changes are marked **Breaking**. This release is the first to
version its export layout (`format_version` 1).

### Added

- Exact decimal line-item values. The field after a line-item anchor is the
  SQL Anywhere NUMERIC encoding described in issue #22 (write-up by
  pete-green): `[k][ctrl][k digit bytes]` with base-100 digits least
  significant first, the sign in `ctrl & 0x80` and the exponent
  `(ctrl & 0x7F) - 64`, so `02 3f 2f 4b` is 75.47 and `02 c0 3c 09` is -960.
  `Decimal` (checked addition, lossless `Display`) is the type of
  `LineItem::value`. On the Rock Castle sample, `verify` reports 1238 of 2107
  parent groups of three or more fully decoded lines summing to exactly zero
  (58.8%); shuffling the decoded values among those groups' lines leaves 0
  to 2. A field is decoded when `1 <= k <= 10`, every
  digit is at most 99, the field lies inside the page and the exponent is in
  -1..=2. Any other field keeps its raw bytes with `value` `None` and a
  reason (`zero_length`, `digit_over_99`, `exponent_out_of_range`,
  `sector_boundary`), and an `undecoded_values` warning counts them by
  reason. That the value is the line's amount is not established.
- A value field whose length byte sits on, or whose bytes cross, a 512-byte
  sector boundary is undecoded with reason `sector_boundary`, with its raw
  bytes kept. It is not a record error: 429 values on the Rock Castle
  sample carry `sector_boundary`, and the 101 records that used to fail with a
  record error for this reason no longer do. Strict mode still stops on that
  sample: 55 other "value length byte above 10" record errors remain, not related to sector boundaries and
  under investigation, and `--lenient` skips them. Strict stays the default.
- `Reader`, the single entry point: `Reader::open(path, &ReadOptions)` (and
  `Reader::from_bytes`) owns the page store, the keystream model, the catalog
  and the page attribution. `line_items()`, `transactions()`, `indexes()`,
  `columns()`, `schema(table)`, `catalog()` and `attribution_gap()` read from
  it. No `opensqlany` type appears in the public API.
- Strict and lenient reads. Reads are strict by default in the library, the
  CLI and Python: a record or page that cannot be decoded is an error.
  `ReadOptions::lenient()` (`--lenient`, `lenient=True`) skips each one and
  lists it in `rejected` (`scan_rejections` in SQLite, `rejected.csv` in
  CSV). This applies to every command, including `catalog` and `schema`.
- `Scan<T>` (`items`, `rejected`, `warnings`), `Warning` with a stable
  `WarningKind::as_str()`, `RecordFlag`, `DateSource`, and one crate `Error`
  with `kind()` (`ErrorKind`, stable `as_str()`), `page_number()` and
  `page_offset()`. `LineItem` and `TransactionHeader` carry `date_source` and
  `flags`; `SysTableEntry`, `SysIndexEntry` and `SysColumn` carry `flags`.
- Validation on open. The superblock magic, the version triple (3, 201.12),
  the `flags_06` base bits, and the CRC-32 footer and reserved trailer bytes
  of every page are checked. A file that is not a QuickBooks company file (a
  `.QBB`, a `.TLG`, any 4 KiB-aligned file), has another version, or has a
  damaged page fails to open with a specific error (`BadMagic`,
  `UnsupportedVersion`, `UnsupportedFlags`, `BadCrc`, `BadTrailer`) instead
  of exporting zero or spurious records. A file whose keystream cannot be
  calibrated at all is an error instead of decoding with `bv = 0`.
- Run-level warnings, printed on stderr and recorded in the exports:
  `records_outside_live_rows`, `line_items_not_read`, `unattributed_records`,
  `header_rows_without_id`, `invalid_root_pages`, `keystream_unverified_pages`,
  `low_confidence_sector_pages`, `unconfirmed_directory_pages`, `opaque_pages`,
  `undecoded_values`, `sector_boundary_anchors` and `attribution_gap`, plus
  duplicate header ID counts.
  The `sector_boundary_anchors` warning counts line-item anchors cut by a
  sector boundary that were not read; the count is approximate (about 2,880 on
  the Rock Castle sample, where the line items lost are roughly 4.5% of the
  total). It is also written
  to the exports as `line_items.sector_boundary_anchors`.
- Export layout version. SQLite exports have a `meta` table
  (`format_version`, `openqbw_version`, `source_table_attribution`) and
  `PRAGMA user_version`. CSV exports write `manifest.json` with
  `format_version`, `openqbw_version`, every file with its row count, a
  `counts` object and `source_table_attribution`, and always write
  `rejected.csv`. The version is 1. `count.<scan>.<name>` rows in `meta` and
  the `counts` object hold the numbers behind the stderr warnings. SQLite
  also gains `scan_rejections`, and `transaction_line_items` gains `id`,
  `date_source` and `flags`.
- Timestamps (issue #45, reported by SmithJesko). The 4 bytes between the
  table id and the name in a `SYSTABLE` row, and the `u32` at superblock
  offset 0x2A, are counts of minutes since 1600-02-29. `SysTableEntry` has
  `stamp_raw()` and `stamp_unix_seconds()`, `Reader` has `created_raw()` and
  `created_unix_seconds()`, and Python `Table.stamp_unix_seconds` and
  `Reader.created_unix_seconds` carry the same values. A stamp outside
  1990-01-01..2100-01-01 is `None`. `SPECIFICATION.md` documents both fields
  as timestamps; superblock `0x2D` was the high byte of the creation stamp,
  not a separate constant.
- `openqbw debug`, a group for reverse-engineering diagnostics. Its names,
  flags and output may change in any release.
- `openqbw migrate --format iif --placeholder-accounts`, which writes the IIF
  file with placeholder accounts and leaves out, and counts, lines whose
  value is not decoded (and transactions with no decoded line). The `TRNS`
  amount is the sum of the lines written.
- Python: `ScanReport` (a sequence, with `items`, `rejected` and `warnings`),
  frozen record objects (`Table`, `Index`, `LineItem`, `Transaction`) with
  attributes and `to_dict()`, `ReadWarning`, `Rejected`,
  `openqbw.OpenQBWWarning`, `Reader.attribution_gap` (a snake_case code),
  `.pyi` stubs and `py.typed`. The native module is `openqbw._openqbw`.

### Changed

- **Breaking:** Python 3.10 or later is required (was 3.9, which reached end
  of life in October 2025); wheels use the 3.10 stable ABI.
- **Breaking:** line items carry `value` (exact decimal) in place of amounts
  in cents. Library: `LineItem::value` is `Option<Decimal>`, with
  `value_undecoded: Option<UndecodedReason>` and `value_raw: Vec<u8>`;
  `amount_cents`, `amount_cents_signed`, `amount_type`, `amount_raw`,
  `amount_type_byte()`, `AmountType` and its `decode_cents*` methods are
  removed, and `LineItemError` is replaced by `Error`. Decoding no longer
  depends on the source table. Python: `LineItem.value` is a
  `decimal.Decimal` or `None`, with `value_undecoded` and `value_raw`;
  `amount_cents` and `amount_cents_signed` are removed.
- **Breaking:** SQLite export. `transaction_line_items` has `value` (exact
  decimal text, NULL when not decoded), `value_undecoded` and
  `value_raw_hex` in place of `amount_type`, `amount_cents`,
  `amount_cents_signed` and `amount_raw_hex` (which held 4 raw bytes). It
  has an `id` primary key and a unique `(page_number, page_offset)` in place
  of the primary key `(qb_id_parent, line_number, source_table)`.
  `transactions.total_cents` is replaced by `value_sum`, the exact sum of the
  decoded line values (undecoded lines are not in it, and it is not an
  invoice total), `has_deferred` is removed, and `undecoded_lines` is added.
  `source_table` is NULL instead of an empty string when a line's page is
  not attributed to a table.
- **Breaking:** CSV export. `lineitems.csv` has `value`, `value_undecoded`
  and `value_raw_hex` in place of `amount_cents`, `amount_signed_cents` and
  `amount_decimal`, and `qb_id_parent` in place of `invoice_id`. It carries
  the raw date value and its provenance (`txn_date_raw`, `counter`,
  `date_source` and `flags`, like SQLite) in place of `txn_date`, which was a
  calendar date converted from a fitted offset. `transactions.csv` gains
  `txn_date_raw`, `counter` and `date_source`, and `catalog.csv` gains
  `flags`. IIF keeps a `DATE` column because the format requires one: it
  holds the converted date of the parent's first dated line (offset fitted to
  one sample, unverified), or is empty.
- **Breaking:** Python `LineItem.invoice_id` is `qb_id_parent` (also the
  `to_dict()` key), the name SQLite and CSV use. `LineItem.__repr__` shows
  `source_table='...'` or `None` instead of Rust `Some("...")`.
- **Breaking:** the Rust field `LineItem::invoice_id` is renamed
  `qb_id_parent`.
- **Breaking:** Python `openqbw.open(path, *, lenient=False)` returns a
  `Reader` whose `tables()`, `indexes()`, `line_items()` and
  `transactions()` return a `ScanReport` of frozen record objects in place
  of lists of dicts. The options are keyword-only and apply to every read
  from the `Reader`. Exceptions: `OpenQBWError` derives from `ValueError`;
  `OpenQBWIOError(OpenQBWError, OSError)` replaces the plain `OSError` for
  unreadable files; `FormatError`, `RecordError` and `ContentError` carry
  `kind`, `page_number` and `page_offset`.
- **Breaking:** the crate root holds the reading API only: `Reader`,
  `ReadOptions`, `Leniency`, `Scan`, `Warning`, `WarningKind`, `RecordFlag`,
  `DateSource`, `Error`, `ErrorKind`, `Decimal`, `UndecodedReason`, the
  record types, `SchemaRecovery`, `SyscolumnStats`, `BridgeSource`,
  `PageAttribution` and `AttributionGap`. The free functions are removed:
  `iter_lineitems`, `iter_lineitems_with_attribution`,
  `iter_transaction_headers`, `iter_systable_entries`, `collect_unique`,
  `iter_sysindex`, `collect_unique_sysindex`, `iter_syscolumns`,
  `collect_unique_syscolumns`, `schema_for`, `recover_schema`,
  `PageAttribution::build`, `bridge_owners_via_backref`,
  `extend_with_sysobject` and `bridge_owners_to_tables`; use the `Reader`
  methods. Every public struct and enum is `#[non_exhaustive]`.
- **Breaking:** reverse-engineering cross-checks and low-level pieces moved
  to `openqbw::diagnostics`, which is outside semantic versioning:
  `fk_graph`, `nulls_flag_histogram`, `content_attribution`,
  `schema_attribution`, `sysindex_cross_validation`, `learned_block_count`,
  the keystream oracles (`recover_bv_*`, `oracle_bv_e_page`,
  `deobfuscate_with_bv`, `APAGE_MAGIC`), the per-page row scanners
  (`scan_systable_page`, `scan_sysindex_page`, `scan_syscolumn_page`), the
  anchor and threshold constants, `is_opaque_high_entropy`, `OwnerBridge`,
  `BackrefVotes` and `SUPPORTED_VERSION`. Their methods take `&Reader`
  instead of a page store and model.
- **Breaking:** the date conversion is internal. `DATE_EPOCH_DAYS_BEFORE_UNIX`,
  `SA_DAY_MIN_PLAUSIBLE`, `SA_DAY_MAX_PLAUSIBLE`, `sa_day_to_unix_day`,
  `sa_day_to_unix_seconds` and `unix_day_to_sa_day` are removed; use
  `LineItem::txn_date_days_since_unix` or
  `TransactionHeader::txn_date_days_since_unix` (best effort, fitted to the
  Rock Castle sample and not checked against a date known from another
  source), or the stored `txn_date_raw`.
- **Breaking:** CLI. `fkgraph`, `nulls`, `validate-attribution`, `indexes`
  and `forensics` moved under `openqbw debug`. `export`, `catalog`,
  `schema`, `migrate` and `verify` stay at the top level. `--lenient` is
  accepted by every command.
- **Breaking:** `openqbw migrate --format iif` exits with an error unless
  `--placeholder-accounts` is given, because account names and transaction
  types are not decoded.
- **Breaking:** `openqbw verify` no longer runs the invoice regression
  check (13,375 parents, $399.9M, with its `PHASE5_*` constants: it checked
  the same anchor pattern and unsigned sum it was compared against) or the
  journal pair check built on the sign-bit reading. It reports the sum of
  decoded values, undecoded values by reason, and how many parent groups of
  three or more decoded lines sum to exactly zero (informational). It exits
  with status 3 when the export read-back differs from the scan (it exited
  0): it reads the exported SQLite file back and compares its line-item rows,
  rows with no decoded value and stored rejections with the scan, and
  prints DIFF in an "Export read-back" section. Everything else it prints is
  a report. It also checks the file for a `meta.format_version` equal to
  `PRAGMA user_version` and no newer than the build writes, and fails on any
  database error while reading it. Without `--output` it exports to a private
  temporary directory that is removed on exit, including on error (0.1.6 left
  `/tmp/openqbw-verify-<pid>.sqlite` behind).
- `openqbw export`, `migrate` (sqlite, csv, iif) and `verify --output` never
  delete or truncate anything before the input has opened and scanned. The
  output is built in a temporary file beside the destination and renamed over
  it on success; on failure the temporary file is removed and an earlier
  export is untouched. The same commands refuse, touching nothing, to write
  to the input file (same path, symlink or hard link) or over an existing
  file that starts with the SQL Anywhere superblock magic (a QuickBooks
  company file), for example when the arguments are swapped.
- After a strict failure that `--lenient` can get past (a page that failed to
  decode, or a record whose value field cannot be read), the CLI prints a
  separate `hint:` line that points to `--lenient`. It is not printed for a
  bad magic, version or checksum. A bad-magic or unsupported-file error no
  longer suggests `--lenient`. Python `RecordError` and page-store errors say
  to open the file with `lenient=True`.
- Replacing an existing SQLite export also removes a `-journal`, `-wal` or
  `-shm` file left next to it, which SQLite would otherwise replay onto the
  new file.
- Warnings are labeled by scan (`catalog:`, `line items:`, `headers:`), and a
  warning an earlier scan already printed is not repeated, so one run no
  longer prints the keystream and low-confidence warnings three times with
  different denominators. `openqbw catalog` shows a root page past the end of
  the file or not on a data page as `-`, with a `flags` column
  (`root_page_invalid`). A `last_page` that fails the same test carries
  `last_page_invalid` and is shown the same way (`catalog.csv` keeps the raw
  value). `--help` and the verify report no longer use
  internal phase names.
- Every page is decoded through one path. A page whose keystream opensqlany
  confirms is read as before. A page where exactly one `bv` decodes the
  QuickBooks C.36 anchor is read with that key (earlier versions took the
  first of several). Any other `E` page is decoded with opensqlany's
  best-scoring key, and every record read from it carries
  `RecordFlag::KeystreamUnverified`; a record whose bytes touch a sector
  whose step opensqlany could not determine carries
  `RecordFlag::LowConfidenceSector`. Scans count both in a run-level warning.
- Slot directories must be confirmed by the page prelude. Header pages
  without a confirmed directory use the linear scan, whose records carry
  `HeaderIdAmbiguous`, and are counted in an `unconfirmed_directory_pages`
  warning. `SYSCOLUMN` pages without one are skipped and counted in
  `SyscolumnStats::pages_unconfirmed_directory`, and `openqbw schema` prints
  the count. Not measured on a corpus: opensqlany reports that about a fifth
  of `E` pages with a directory are not confirmed.
- A line-item anchor outside every live row of a page with a confirmed slot
  directory is left out and counted in a `records_outside_live_rows`
  warning; earlier versions exported such residue of deleted or moved
  records as live line items. A live row runs from its slot offset to the
  next live offset, because row lengths are not decoded, so a deleted row
  inside that span is still read. Pages without a confirmed directory keep
  every anchor, and freed pages are still read, because the allocation map
  is not decoded.
- Line items are read only when the field after the parent ID is
  `float32(1.0)`. An anchor with any other plausible value there is counted
  in a `line_items_not_read` warning instead of being left out silently.
  Whether the field is a quantity is not confirmed.
- A line-item anchor followed by a value-length byte above 10, or by a value
  field that runs past the page data area, is an error (`InvalidValueLength`,
  `TruncatedValue`) in strict mode, and is listed in `rejected` in lenient
  mode, instead of a line item with a zero or unknown amount.
- Known limit: a line-item anchor that straddles a 512-byte sector boundary
  may not be found, so that line item is not read; the
  `sector_boundary_anchors` warning counts them. On the Rock Castle sample
  this is about 4.5% of line items. Reading across the boundary is planned
  for a later release.
- Line items take the nearest `04 00 10 <id>` item ID instead of the
  farthest in the 96 bytes before the anchor, and their search windows stop
  at the neighbouring records' anchors, so a record no longer reports the
  previous record's item ID or date. Line items are scanned to the end of
  the page data area (0xFF0, was 0xFE0).
- Line-item and header dates are found in a wider window (SA-days
  10000..=40000, was 13000..20000, which stopped at Oct 2013). When two
  separate date/counter pairs sit in the search window, the date and counter
  are left unset and `date_source` is `ambiguous` (`window_scan` when one
  pair was found, `not_found` when none), with `RecordFlag::DateAmbiguous`.
- `source_table` is a position-based attribution that is unverified. The
  content-based cross-check (`verify --strict-attribution`) agrees with it on
  0.3% of pages on the Rock Castle sample, and line items land in 130 tables
  there. It is kept, and exports say so (`meta.source_table_attribution` in
  SQLite, `source_table_attribution` in `manifest.json`).
- Transaction headers that repeat an earlier `qb_id` are left out of every
  export format (SQLite, CSV and IIF) and counted on stderr, together with
  the number of IDs seen under more than one transaction type; the first in
  page order is kept. CSV used to list every sighting, and IIF used the last.
- A `SYSTABLE` row whose `data_root_page` is past the end of the file or not
  a data (`E`) page carries `RecordFlag::RootPageInvalid`, is not used for
  page-to-table attribution, and is counted in an `invalid_root_pages`
  warning. Earlier versions let such a value move the attribution boundaries
  of every page above it. `SYSTABLE` trailer fields (`col_count`,
  `data_root_page`, `last_page`) are no longer read from the page trailer
  bytes at 0xFF0 and above.
- `SYSCOLUMN` name recovery tries every default-value length and requires the
  default's length byte to agree. A column whose default value is itself a
  valid identifier (for example `is_active` with default `Y`) is counted in
  `SyscolumnStats::rows_ambiguous_name` and left out, where earlier versions
  reported the default as the column name.
- A `SYSOBJECT` owner whose candidate table names tie on votes is left
  unbridged instead of taking an arbitrary one. `SchemaRecovery` gains
  `candidate_owners`, `catalog_col_count`, `scan_stats`, `pages_rejected`,
  `warnings` and `columns_short_of_catalog()`, and `openqbw schema` warns
  about missing columns, tied owners, and rows or pages it could not read.
- The foreign-key graph joins column owners to tables through the owner
  bridge; columns that cannot be placed are counted (`columns_unbridged`)
  instead of dropped. Earlier versions joined on `data_root_page`, which
  names unrelated tables for most owners.
- When more than half of the data pages are opaque (as in a
  password-protected file) a line-item scan fails with `ContentOpaque`;
  otherwise the count is reported as an `opaque_pages` warning. The 50%
  threshold has not been measured on a corpus. When page-to-table attribution
  is unavailable, the catalog, line-item and header scans carry an
  `attribution_gap` warning, and when only part of the file is attributed
  the line-item scan carries an `unattributed_records` warning.
- Live rows on header pages that hold no header ID are counted in a
  `header_rows_without_id` warning instead of being skipped silently.
  Transaction headers whose row holds more than one 16-character ID in the
  header shape, and all headers on pages without a readable slot directory,
  carry `RecordFlag::HeaderIdAmbiguous`.
- `openqbw forensics` reports the size of the opened store instead of 0 when
  file metadata cannot be read.

- Packaging: `openqbw-py` has `publish = false`. The release workflow fails
  unless it runs on a tag equal to `v<workspace version>` and the Cargo
  workspace version, both `openqbw` requirements and the pyproject version
  are equal. CI no longer checks out OpenSQLAnywhere.

### Removed

- The amount readings. Amounts read as an unsigned 24-bit cents value
  (`amount_cents`) or a sign-bit value (`amount_cents_signed`) disagreed with
  the data on about half of all values; `value` replaces both. The removals
  in the API, SQLite and CSV are listed under Changed.
- The `openqbw verify` invoice regression and journal pair checks (see
  Changed).

### Fixed

- Line-item amounts disagreed with the data on about half of all values
  (see Added for the replacement decoding). Credit to pete-green for the
  encoding write-up in issue #22.
- Opening a file that is not a SQL Anywhere page store, such as the 2001-era
  QuickBooks file in issue #38 (reported by flywire), fails with `BadMagic`
  ("not a QuickBooks SQL Anywhere company file ... older QuickBooks versions,
  such as 2001-era files, use a different format and are not supported").
  The superblock magic is checked before the file size, so the error no
  longer reads "file size ... is not a multiple of page size".

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
- `SPECIFICATION.md` section 5.1: the `SYSTABLE` row-prefix `table_id` field,
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
- `SPECIFICATION.md` section 8: answered two of the file's long-standing open
  questions from the same report - `flags_06` is a bitfield over base
  `0x09` (not an enum of magic values; a QuickBooks Enterprise 24.0 file
  adds a third value, `0x29 = 0x09|0x20`, fitting the same pattern as
  the original `0x49 = 0x09|0x40`), and `version_a`/`version_b` are not
  an ASA schema version pair - the triple is an invariant engine-format
  constant across QuickBooks 2018 through Enterprise 2024. The bitfield
  accessor itself (`Superblock::flags_06_variant_bits`) lives upstream
  in `opensqlany` and will be available here once this repo picks up
  its next release. section 5 also gained a note on SYSTABLE row anchoring: on
  the same file the row is length-prefixed and begins 34 bytes before
  the tag our reader scans for, which doesn't affect the current
  tag-anchored scan but matters for any future work needing the row's
  true boundaries. Remainder of #16.
- `SPECIFICATION.md` section 8b: preserved @pete-green's dead-ends writeup
  from building an independent decoder against the same file (#17) -
  the still-open multi-page `LONG VARBIT` chaining blocker (including
  the unresolved multi-copy/version-selection problem), and four
  methodology notes on validating a byte-level reader (a sector-linear
  addressing bug that survives casual inspection, why +/-1 shifted-window
  controls don't discriminate on this format, an object-ID minimum-gap
  self-check, and requiring >=3 distinct values before trusting a
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
[0.1.4]: https://github.com/Sigilweaver/OpenQBW/releases/tag/v0.1.4
[0.1.3]: https://github.com/Sigilweaver/OpenQBW/releases/tag/v0.1.3
[0.1.2]: https://github.com/Sigilweaver/OpenQBW/releases/tag/v0.1.2
[0.1.1]: https://github.com/Sigilweaver/OpenQBW/releases/tag/v0.1.1
[0.1.0]: https://github.com/Sigilweaver/OpenQBW/releases/tag/v0.1.0
