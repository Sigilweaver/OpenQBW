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

- Open a `.qbw` file and decode all pages (SA17 page-store + Intuit's
  additive-progression obfuscation).
- Enumerate user tables via the SA17 `SYSTABLE` catalog.
- Parse `SYSCOLUMN`, `SYSINDEX`, `SYSOBJECT` system catalogs.
- Extract invoice **line items** and **transaction headers** at row
  granularity.
- Attribute pages to tables using a width-band + content-signature
  validator (Phase 6, WP-6Z).
- Export the parsed catalog and lineitems to a SQLite database.

The line-item extractor reads invoice and journal line items from page
bytes. Amounts are decoded for the invoice and general-journal line-item
tables only; every other line item is exported with no amount, and
exports report how many.

A full write-up of the format work lives in [SPECIFICATION.md](SPECIFICATION.md)
and the empirical notebook in [re/NOTES.md](re/NOTES.md).

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

OpenQBW reads the page store through the
[OpenSQLAnywhere](https://github.com/Sigilweaver/OpenSQLAnywhere) crate,
which Cargo fetches. Build from a clone:

```console
$ git clone https://github.com/Sigilweaver/OpenQBW.git
$ cd OpenQBW
$ cargo build --release --locked
$ ./target/release/openqbw --help
```

`Cargo.lock` is committed, so `--locked` builds the CLI and the Python
wheel from the dependency versions CI tested.

Supported toolchains: Rust 1.95 or later (the `rust-version` in
`Cargo.toml`, checked in CI) and Python 3.10 or later (abi3 wheels built
against the 3.10 stable ABI). A release that raises either floor says so in
the CHANGELOG.

### Python bindings

A PyO3-based extension lives in `crates/openqbw-py` and ships as a
package named `openqbw`. To build and install into the active Python
environment:

```console
$ pip install maturin
$ cd crates/openqbw-py
$ maturin develop --release
$ python -c "import openqbw; r = openqbw.open('mybooks.qbw'); print(r.page_count, 'pages')"
```

See [crates/openqbw-py/README.md](crates/openqbw-py/README.md) for the
full Python API.

## CLI quickstart

```console
# Inventory the user tables in a company file
$ openqbw catalog mybooks.qbw

# Columns recovered for one table
$ openqbw schema mybooks.qbw abmc_invoice_header

# Export transactions and line items to SQLite for inspection in any
# SQL tool (DB Browser, Datasette, pandas, ...)
$ openqbw export mybooks.qbw books.sqlite

# CSV directory, or IIF with placeholder accounts
$ openqbw migrate mybooks.qbw --format csv --out books/

# Skip undecodable records instead of stopping; each is listed
$ openqbw --lenient export mybooks.qbw books.sqlite

# Reverse-engineering diagnostics (output may change in any release)
$ openqbw debug indexes mybooks.qbw --fk-only --summary-only
$ openqbw debug nulls mybooks.qbw
$ openqbw debug validate-attribution mybooks.qbw
```

See [docs/cli.md](docs/cli.md) for the full subcommand reference.

Exit status: 0 on success, 1 on an error (including a file that is not a
supported company file, or an undecodable record without `--lenient`), 2
on a usage error, and 3 when `verify` finds a check that differs. The
`verify` regression baseline is specific to the Rock Castle Construction
sample; pass `--no-regression-baseline` for other files.

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
`attribution_gap`) plus `headers.duplicate_header_ids` and
`headers.duplicate_header_ids_conflicting_types`. Each scan's usual counts
are written even when zero. The totals a count is out of (`..._decoded`,
`..._extent`, `..._total`) appear only when the count is nonzero. IIF files
have no place for metadata; `migrate --format iif` prints
`duplicate_headers` and the warnings on stderr only.

The layout version is 1. Adding a table, a column, a CSV file or a manifest
key leaves it unchanged; renaming or removing one, or changing what a value
means, increments it. Readers should ignore tables, columns and keys they do
not know. Codes stored in the exports (`kind` in `scan_rejections` and
`rejected.csv`, `flags`, `amount_encoding`, `date_source`) are stable
snake_case strings; the `message` text is for people and may change.
NULL (an empty CSV field) means a value was not decoded, never zero.

## Library usage

```rust,no_run
use openqbw::{ReadOptions, Reader};

let reader = Reader::open("mybooks.qbw", &ReadOptions::default())?;
let lines = reader.line_items()?;
for w in &lines.warnings {
    eprintln!("warning: {w}");
}
// `None` means the amount was not decoded, not zero.
let decoded: i64 = lines.items.iter().filter_map(|li| li.amount_cents).sum();
let undecoded = lines.items.iter().filter(|li| li.amount_cents.is_none()).count();
println!("decoded total: {decoded} cents; {undecoded} lines not decoded");
# Ok::<(), openqbw::Error>(())
```

Reads are strict: a record the decoder cannot read with confidence is an
error. `ReadOptions::lenient()` skips such records and lists them in
`rejected`; records that were kept but deserve less trust carry `flags`.

## Documentation

- [SPECIFICATION.md](SPECIFICATION.md) - format specification (work in progress)
- [docs/use-cases.md](docs/use-cases.md) - extended use-case walkthroughs
- [docs/migration-guide.md](docs/migration-guide.md) - leaving the QuickBooks ecosystem
- [docs/format-overview.md](docs/format-overview.md) - high-level pointer into the spec
- [docs/cli.md](docs/cli.md) - full CLI reference
- [re/NOTES.md](re/NOTES.md) - the reverse-engineering lab notebook (C.1...C.57)

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
