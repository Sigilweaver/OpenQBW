# OpenQBW

[![CI](https://github.com/Sigilweaver/OpenQBW/actions/workflows/ci.yml/badge.svg)](https://github.com/Sigilweaver/OpenQBW/actions/workflows/ci.yml)
[![DOI](https://zenodo.org/badge/DOI/10.5281/zenodo.20470597.svg)](https://doi.org/10.5281/zenodo.20470597)
[![crates.io](https://img.shields.io/crates/v/openqbw.svg)](https://crates.io/crates/openqbw)
[![PyPI](https://img.shields.io/pypi/v/openqbw.svg)](https://pypi.org/project/openqbw/)
[![docs.rs](https://img.shields.io/docsrs/openqbw)](https://docs.rs/openqbw)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust MSRV](https://img.shields.io/badge/rust-1.95%2B-orange.svg)](https://www.rust-lang.org)
[![Docs](https://img.shields.io/badge/docs-sigilweaver.app-blue.svg)](https://sigilweaver.app/openqbw/docs/)

> Open specification and open-source parser for the **QuickBooks Desktop company file** (`.qbw`) format.

Intuit has announced end-of-life for QuickBooks Desktop, forcing small
businesses either onto an expensive cloud subscription or to lose
practical access to their own books. OpenQBW reverse engineers the
on-disk file format from publicly available files, writes it up as
a specification, and ships a Rust parser so that the accounting
data a business has already paid for stays accessible.

## Status

OpenQBW is in active development. The current code can:

- Open a `.qbw` file, checking its magic, version and page checksums, and
  read its pages (SA17 page store plus Intuit's additive-progression
  obfuscation). Pages whose keystream cannot be confirmed are read with the
  best candidate key and their records are flagged.
- Enumerate user tables via the SA17 `SYSTABLE` catalog.
- Parse the `SYSCOLUMN`, `SYSINDEX` and `SYSOBJECT` system catalogs.
- Extract **line items** and **transaction headers** at row granularity.
  Line items come from many tables (130 on the Rock Castle sample), not only
  invoices.
- Attribute pages to tables by position. The attribution is unverified; see
  below.
- Export the parsed catalog, transactions and line items to SQLite, CSV or
  IIF.

Each line item carries its numeric value field, decoded by structure (the
SQL Anywhere NUMERIC layout) as an exact decimal. A field that does not fit
the layout, or sits on a 512-byte sector boundary, keeps its raw bytes and is
counted. That the value is the line's amount is not established. `source_table`
is a position-based attribution that is unverified: a content-based check
agrees with it on 0.3% of pages on the Rock Castle sample. A line item whose
anchor straddles a sector boundary may not be read (about 4.5% of line items
on that sample).

The format write-up is [SPECIFICATION.md](SPECIFICATION.md), including the
line-item value layout in section 5.2 and the open questions in section 8.

## Non-goals

- Shipping, linking, or distributing any Intuit code or trademarks.
- Breaking passwords or DRM. OpenQBW targets the on-disk layout of
  company files that the lawful owner can already open.
- Writing `.qbw` files. OpenQBW is **read-only**.

## Use cases

- **Data liberation for users leaving the QuickBooks Desktop ecosystem.**
  Export your transactions to CSV, SQLite, or IIF so you can move
  them to another accounting package, or just keep an offline copy
  for the legally required retention period after the SaaS subscription
  lapses.
- **Forensic accounting and litigation support.** Read a `.qbw` file
  without owning a QuickBooks license, including offline copies on
  machines where the QB application has been uninstalled.
- **Audit and discovery.** Inventory tables, indexes, row counts,
  and surface gaps that suggest deleted records or schema drift.
- **Long-term archival.** Keep an open-format snapshot of the books
  every fiscal year, independent of Intuit's product roadmap.

## Install

Command-line tool:

```console
$ cargo install openqbw-cli
$ openqbw --help
```

Python package (Python 3.10 or later; wheels for Linux, macOS and Windows):

```console
$ pip install openqbw
```

Rust library:

```toml
[dependencies]
openqbw = "0.2"
```

Or build from a clone; `Cargo.lock` is committed, so `--locked` builds the CLI
and the Python wheel from the dependency versions CI tested:

```console
$ git clone https://github.com/Sigilweaver/OpenQBW.git
$ cd OpenQBW
$ cargo build --release --locked
$ ./target/release/openqbw --help
```

Supported toolchains: Rust 1.95 or later (the `rust-version` in
`Cargo.toml`, checked in CI) and Python 3.10 or later (abi3 wheels built
against the 3.10 stable ABI). A release that raises either floor says so in
the CHANGELOG.

### Python bindings

```python
import openqbw

r = openqbw.open("mybooks.qbw")
print(r.page_count, "pages")
```

See [crates/openqbw-py/README.md](crates/openqbw-py/README.md) for the
full Python API and for building the extension from source.

## CLI quickstart

```console
# Inventory the user tables in a company file
$ openqbw catalog mybooks.qbw

# Columns recovered for one table
$ openqbw schema mybooks.qbw abmc_invoice_header

# Export transactions and line items to SQLite for inspection in any
# SQL tool (DB Browser, Datasette, pandas, ...). The output file is
# replaced only when the export succeeds.
$ openqbw export mybooks.qbw books.sqlite

# CSV directory, or IIF with placeholder accounts
$ openqbw migrate mybooks.qbw --format csv --out books/
$ openqbw migrate mybooks.qbw --format iif --placeholder-accounts --out books.iif

# Skip undecodable records instead of stopping; each is listed
$ openqbw --lenient export mybooks.qbw books.sqlite

# Reverse-engineering diagnostics (output may change in any release)
$ openqbw debug indexes mybooks.qbw --fk-only --summary-only
$ openqbw debug nulls mybooks.qbw
$ openqbw debug validate-attribution mybooks.qbw
```

See [docs/docs/cli.md](docs/docs/cli.md) for the full subcommand reference.

Exit status: 0 on success, 1 on an error (including a file that is not a
supported company file, or an undecodable record without `--lenient`), 2
on a usage error, and 3 when `verify` finds a check that differs.

### Output formats

Every file set openqbw writes records the version of its layout:

- SQLite (`export`, `migrate --format sqlite`): `PRAGMA user_version` and
  `meta.format_version`, plus `meta.openqbw_version`.
- CSV (`migrate --format csv`): `manifest.json` with `format_version`,
  `openqbw_version` and each file's row count. `rejected.csv` is always
  written.

Both also record the counts behind the warnings printed on stderr, as
`count.<scan>.<name>` rows in `meta` and a `counts` object in `manifest.json`
(`<scan>.<name>` keys). `<scan>` is `catalog`, `line_items` or `headers`;
the names are the warning codes (`records_outside_live_rows`,
`line_items_not_read`, `unattributed_records`, `header_rows_without_id`,
`invalid_root_pages`, `keystream_unverified_pages`,
`low_confidence_sector_pages`, `unconfirmed_directory_pages`, `opaque_pages`,
`undecoded_values`, `attribution_gap`) plus `headers.duplicate_header_ids` and
`headers.duplicate_header_ids_conflicting_types`. Each scan's usual counts
are written even when zero. The totals a count is out of (`..._decoded`,
`..._extent`, `..._total`) and the `undecoded_values_*` breakdown appear
only when the count is nonzero. IIF files
have no place for metadata; `migrate --format iif` prints
`duplicate_headers` and the warnings on stderr only.

The layout version is 1. Adding a table, a column, a CSV file or a manifest
key leaves it unchanged; renaming or removing one, or changing what a value
means, increments it. Readers should ignore tables, columns and keys they do
not know. Codes stored in the exports (`kind` in `scan_rejections` and
`rejected.csv`, `flags`, `value_undecoded`, `date_source`) are stable
snake_case strings; the `message` text is for people and may change.
NULL (an empty CSV field) means a value was not decoded, never zero.

Line items carry `value` as an exact decimal string (SQLite `TEXT`, never a
float), `value_undecoded` (the reason, when it is not decoded: `zero_length`,
`digit_over_99`, `exponent_out_of_range` or `sector_boundary`) and
`value_raw_hex`. CSV and SQLite carry the raw date value (`txn_date_raw`)
and how it was found (`date_source`), never a converted calendar date. `transactions.value_sum` is the exact sum of the decoded
line values of one parent, not an invoice total. `source_table` is
position-based and unverified; exports record that in
`meta.source_table_attribution` and in `manifest.json`.

## Library usage

```rust,no_run
use openqbw::{ReadOptions, Reader};

let reader = Reader::open("mybooks.qbw", &ReadOptions::default())?;
let lines = reader.line_items()?;
for w in &lines.warnings {
    eprintln!("warning: {w}");
}
// `value` is an exact `Decimal`; `None` means it was not decoded, not zero.
let decoded = openqbw::Decimal::checked_sum(lines.items.iter().filter_map(|li| li.value));
let undecoded = lines.items.iter().filter(|li| li.value.is_none()).count();
println!("sum of decoded values: {decoded:?}; {undecoded} lines not decoded");
# Ok::<(), openqbw::Error>(())
```

Reads are strict: a record the decoder cannot read with confidence is an
error. `ReadOptions::lenient()` skips such records and lists them in
`rejected`; records that were kept but deserve less trust carry `flags`.

## Documentation

- [SPECIFICATION.md](SPECIFICATION.md) - format specification (work in progress)
- [docs/docs/use-cases.md](docs/docs/use-cases.md) - extended use-case walkthroughs
- [docs/docs/migration-guide.md](docs/docs/migration-guide.md) - leaving the QuickBooks ecosystem
- [docs/docs/format-overview.md](docs/docs/format-overview.md) - high-level pointer into the spec
- [docs/docs/cli.md](docs/docs/cli.md) - full CLI reference
- [docs/docs/python-api.md](docs/docs/python-api.md) - Python API reference

## Legal and ethical

- **No raw corpus repository is published.** During corpus collection
  we found that several public GitHub repositories had accidentally
  committed real business financial data, including personal names,
  home addresses, phone numbers, and US Social Security / tax
  identification numbers. Affected repository owners have been
  contacted directly. Out of caution the full downloaded corpus is
  kept private and is not redistributed.
- QuickBooks(R) is a registered trademark of Intuit Inc. OpenQBW is
  an independent project and is not affiliated with, endorsed by, or
  sponsored by Intuit Inc.
- License: [Apache-2.0](LICENSE). See also [NOTICE](NOTICE) and
  [CONTRIBUTING.md](CONTRIBUTING.md).

## Companion projects

- **[OpenSQLAnywhere](https://github.com/Sigilweaver/OpenSQLAnywhere)** --
  the lower-level SA17 page-store reader OpenQBW depends on.
