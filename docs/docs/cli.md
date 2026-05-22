# CLI reference

```console
$ openqbw --help
```

| Subcommand            | What it does                                                   |
|-----------------------|----------------------------------------------------------------|
| `catalog`             | Print the SYSTABLE catalog (table_id, name, root page).        |
| `schema`              | Print the columns of a table (via SYSCOLUMN bridged to SYSTABLE).|
| `nulls`               | Histogram of SYSCOLUMN nulls_flag bytes with sample columns.   |
| `indexes`             | List SYSINDEX and cross-validate position attribution.         |
| `fkgraph`             | Print heuristic foreign-key edges (name-based fallback).       |
| `validate-attribution`| Validate position attribution against SYSCOLUMN width bands.   |
| `export`              | Export transactions and line items to SQLite.                  |
| `migrate`             | Export to CSV, SQLite, or IIF for data liberation.             |
| `forensics`           | File-level discovery report (pages, ap coverage, anomalies).   |
| `verify`              | Validate an export against known invariants.                   |

## Subcommand details

### `catalog`

```console
$ openqbw catalog mybooks.qbw
```

Prints one line per user table from SYSTABLE. Useful as a first
look at any file.

### `schema <table>`

```console
$ openqbw schema mybooks.qbw abmc_invoice_lineitem
```

Lists the columns for the named table, bridging SYSCOLUMN to
SYSTABLE via the SYSOBJECT catalog.

### `indexes [--fk-only] [--summary-only]`

```console
$ openqbw indexes mybooks.qbw
$ openqbw indexes mybooks.qbw --fk-only
$ openqbw indexes mybooks.qbw --summary-only
```

Lists every index from SYSINDEX (creator 0x01 0x46). With
`--fk-only`, restricts to indexes whose name suggests a foreign
key. With `--summary-only`, prints only the cross-validation
result (agree / disagree / missing / orphan counts).

### `export --out <PATH>`

```console
$ openqbw export mybooks.qbw --out books.sqlite
```

Writes a SQLite database containing the catalog, lineitems, and
transaction headers. The output is deterministic for the same
input.

### `migrate --out <PATH> [--format csv|sqlite|iif]`

```console
$ openqbw migrate mybooks.qbw --out books.sqlite --format sqlite
$ openqbw migrate mybooks.qbw --out out_csv     --format csv
$ openqbw migrate mybooks.qbw --out books.iif   --format iif
```

Data-liberation export with three target formats:

- `csv` (default): writes `catalog.csv`, `transactions.csv`,
  and `lineitems.csv` into the directory given by `--out`
  (created if missing). Fields are RFC 4180 quoted only when
  needed.
- `sqlite`: alias for `export --out <PATH>`. Single
  deterministic SQLite database.
- `iif`: writes a single Intuit Interchange Format file with
  CRLF line endings. Line items are grouped by their parent
  invoice id; each group becomes one `TRNS` followed by `SPL`
  rows and an `ENDTRNS`. When a matching transaction header is
  available the header's transaction type is used, otherwise the
  group is emitted as `GENERAL JOURNAL`. SPL amounts are negated
  per IIF's double-entry convention.

### `forensics`

```console
$ openqbw forensics mybooks.qbw
```

File-level discovery report covering:

- File and page-store stats (size, page count, AP learned
  block coverage).
- Catalog summary (total tables, user tables).
- Business-record summary (transaction headers, line items,
  distinct parent ids, orphan parents, childless headers,
  lineitem grand total).

A non-zero orphan-parent count is a discovery signal: the file
may contain partially purged records or a header table this
build does not parse yet.

### `verify`

```console
$ openqbw verify mybooks.qbw
```

Runs the regression invariants:

- Invoice grand-total reconciliation (Phase 5)
- Per-table page-coverage check
- SYSINDEX cross-validation summary (WP-6Z.3)

Exit code is non-zero if any invariant fails.

### `validate-attribution`

```console
$ openqbw validate-attribution mybooks.qbw
```

Cross-checks the SYSTABLE-driven page->table map against the
width-band attribution derived from SYSCOLUMN.

### `fkgraph`

```console
$ openqbw fkgraph mybooks.qbw
```

Falls back to a name-heuristic FK graph (`*_id`, `*_id_h`) for
files where SYSINDEX is unparseable. Superseded by `indexes`
when SYSINDEX is available.

### `nulls`

```console
$ openqbw nulls mybooks.qbw
```

Histogram of `SYSCOLUMN.nulls_flag` byte values with sample
column names per value. Useful for understanding the null /
default-marker discipline of a file.
