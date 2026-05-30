# OpenQBW

[![CI](https://github.com/Sigilweaver/OpenQBW/actions/workflows/ci.yml/badge.svg)](https://github.com/Sigilweaver/OpenQBW/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/openqbw.svg)](https://crates.io/crates/openqbw)
[![PyPI](https://img.shields.io/pypi/v/openqbw.svg)](https://pypi.org/project/openqbw/)
[![docs.rs](https://img.shields.io/docsrs/openqbw)](https://docs.rs/openqbw)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Rust MSRV](https://img.shields.io/badge/rust-1.87%2B-orange.svg)](https://www.rust-lang.org)
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

On the Rock Castle sample file the lineitem extractor reaches
**13,375 / 13,375 invoices and a grand total of $399,914,792.78**,
matching the value QuickBooks itself reports for the same file.

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

OpenQBW depends on the [OpenSQLAnywhere](https://github.com/Sigilweaver/OpenSQLAnywhere)
crate, which lives in a sibling directory via a relative path. Clone
both side by side:

```console
$ git clone https://github.com/Sigilweaver/OpenSQLAnywhere.git
$ git clone https://github.com/Sigilweaver/OpenQBW.git
$ cd OpenQBW
$ cargo build --release
$ ./target/release/openqbw --help
```

Rust 1.85+ is required (workspace uses edition 2024).

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

# Cross-validate page-to-table attribution against SYSINDEX
$ openqbw verify mybooks.qbw

# List indexes (FK indexes only, summary mode)
$ openqbw indexes mybooks.qbw --fk-only --summary-only

# Export the catalog and lineitems to SQLite for inspection in any
# SQL tool (DB Browser, Datasette, pandas, ...)
$ openqbw export mybooks.qbw --out books.sqlite

# Other introspection subcommands
$ openqbw schema mybooks.qbw
$ openqbw nulls mybooks.qbw
$ openqbw validate-attribution mybooks.qbw
```

See [docs/cli.md](docs/cli.md) for the full subcommand reference.

## Library usage

```rust,no_run
use openqbw::{iter_lineitems_with_attribution, PageAttribution};
use opensqlany::{ApModel, PageStore};

let store = PageStore::open("mybooks.qbw")?;
let model = ApModel::learn(&store);
let attrib = PageAttribution::build(&store, &model)?;

let mut total_cents: i128 = 0;
for li in iter_lineitems_with_attribution(&store, &model, &attrib) {
    total_cents += li.amount_cents as i128;
}
println!("invoice grand total: ${:.2}", total_cents as f64 / 100.0);
# Ok::<(), anyhow::Error>(())
```

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
