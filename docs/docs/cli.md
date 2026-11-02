# CLI reference

```console
$ openqbw --help
```

| Subcommand            | What it does                                                   |
|-----------------------|----------------------------------------------------------------|
| `catalog`             | Print the SYSTABLE catalog (table_id, name, root page).        |
| `schema`              | Print the columns of a table (via SYSCOLUMN bridged to SYSTABLE).|
| `export`              | Export transactions and line items to SQLite.                  |
| `migrate`             | Export to CSV, SQLite, or IIF for data liberation.             |
| `verify`              | Export, then report coverage and undecoded values.             |
| `debug`               | Reverse-engineering diagnostics (`fkgraph`, `nulls`, `validate-attribution`, `indexes`, `forensics`). |

## Global option

`--lenient` is accepted by every subcommand. By default a read is strict: a
record or page that cannot be decoded is an error and the command exits with
status 1 at the first one. With `--lenient`, each such record is skipped,
printed on stderr, and listed in the export (`scan_rejections` table in
SQLite, `rejected.csv` in CSV). Real files often need `--lenient`; the Rock
Castle sample skips 55 records.

When a strict read stops on a record or page that `--lenient` can get past,
the command prints a separate line after the error:

```text
hint: this file has records or pages the reader cannot decode. Rerun with --lenient to skip them; each skipped one is printed and listed in the export.
```

The hint is not printed for a bad magic, an unsupported version or a bad
checksum, which `--lenient` does not get past.

Warnings on stderr are labeled by scan (`catalog:`, `line items:`,
`headers:`), and a warning an earlier scan of the same run already printed is
not repeated.

## Exit status

0 on success; 1 on an error, including a file that is not a supported company
file or an undecodable record without `--lenient`; 2 on a usage error; 3 when
`verify` finds that the export read-back differs from the scan.

## Subcommand details

### `catalog [--user-only]`

```console
$ openqbw catalog mybooks.qbw
```

Prints one line per table from SYSTABLE. With `--user-only`, skips
`SYS*`, `ISYS*`, `RS_*` and `dbo*` tables. Useful as a first look at any
file. A root page past the end of the file, or not on a data page, is shown as
`-` and the `flags` column reads `root_page_invalid`.

### `schema <input> <table>`

```console
$ openqbw schema mybooks.qbw abmc_invoice_header
```

Lists the columns for the named table, bridging `SYSCOLUMN` to
`SYSTABLE` via the table-id back-reference in the `SYSTABLE` row
prefix (`SPECIFICATION.md` section 5.1), and falling back to the `SYSOBJECT`
catalog scan for owners that does not cover. The header line names the
owner and the bridge that resolved it:

```console
table: abmc_invoice_header  columns: 46  (SYSCOLUMN owner 3073, via SYSTABLE back-reference)
```

Not every table's columns are recoverable yet. When the recovered
`column_id`s have holes the command says so on stderr, and when no
owner can be bridged to the table it reports which stage came up
empty - catalog presence, `SYSCOLUMN` recovery, or the bridge -
rather than a bare "not found".

### `export <input> <output>`

```console
$ openqbw export mybooks.qbw books.sqlite
```

Writes a SQLite database containing the transactions and line items, plus
`scan_rejections` and `meta` tables. The input and output are positional.
The export is written to a temporary file and replaces `<output>` only when it
succeeds, so a failed run leaves an earlier export untouched. Nothing is
deleted or truncated before the input has opened and scanned.

The command refuses, touching nothing, to write to the input file (the same
path, a symlink or a hard link to it):

```text
refusing to write ...: it is the input file.
```

It also refuses to overwrite an existing file that starts with the SQL
Anywhere superblock magic, which is a QuickBooks company file, as happens when
the arguments are swapped:

```text
refusing to overwrite ...: it is a QuickBooks company file (or another SQL Anywhere database). Check the argument order: the input comes first, the output second. Nothing was changed.
```

`migrate --format sqlite|csv|iif` and `verify --output` build their output the
same way and apply the same refusals.

### `migrate <input> --out <PATH> [--format csv|sqlite|iif]`

```console
$ openqbw migrate mybooks.qbw --out books.sqlite --format sqlite
$ openqbw migrate mybooks.qbw --out out_csv     --format csv
$ openqbw migrate mybooks.qbw --out books.iif   --format iif --placeholder-accounts
```

Data-liberation export with three target formats:

- `csv` (default): writes `catalog.csv`, `transactions.csv`,
  `lineitems.csv`, `rejected.csv` and `manifest.json` into the directory
  given by `--out` (created if missing). Fields are RFC 4180 quoted only when
  needed. Dates are the raw `txn_date_raw` value with a `date_source` column,
  not a converted calendar date. Columns:
  - `lineitems.csv`: `qb_id_parent`, `item_qb_id`, `value`, `value_undecoded`,
    `value_raw_hex`, `txn_date_raw`, `counter`, `date_source`, `flags`,
    `source_table`, `page_number`, `page_offset`
  - `transactions.csv`: `qb_id`, `txn_type`, `source_table`, `txn_date_raw`,
    `counter`, `date_source`, `page_number`, `page_offset`
  - `catalog.csv`: `table_id`, `name`, `column_count`, `data_root_page`,
    `last_page`, `flags`
  - `rejected.csv`: `page_number`, `page_offset`, `kind`, `message`
- `sqlite`: equivalent to `export <input> <PATH>`.
- `iif`: account names and transaction types are not decoded, so the
  command exits with an error unless `--placeholder-accounts` is given. With
  it, the file has CRLF line endings. Line items are grouped by their parent
  transaction id; each group becomes one `TRNS` followed by `SPL` rows and an
  `ENDTRNS`. Every `TRNS` posts to the placeholder account `Accounts
  Receivable` and every `SPL` uses the internal source-table name as its
  account. A line whose value is not decoded is left out and counted, and a
  transaction with no decoded line is left out. The value is not established
  as the line amount. IIF requires a `DATE` column, so unlike CSV and SQLite
  the IIF file carries a converted date: the first dated line of the parent,
  converted with an offset fitted to one sample and not checked against a
  date known from another source, or empty when no line has a date.

### `verify <input> [--output <PATH>] [--strict-attribution]`

```console
$ openqbw verify mybooks.qbw
```

Exports the file to `--output`, or to a private temporary directory that is
removed when the command ends, including on error. It then reads the SQLite
file back and compares its line-item rows, rows with no decoded value and
stored rejections with the scan (the "Export read-back" section: `line_items`,
`undecoded_values` and `scan_rejections`, each `ok` or `DIFF`). Everything
else it prints is a report:

- Per-table line-item counts and the sum of decoded values
  (`source_table` is position-based and unverified)
- Undecoded values by reason
- How many parent groups of three or more decoded lines sum to
  exactly zero (informational)
- With `--strict-attribution`, per-page agreement between the position-based
  attribution and a content-signature attribution

Exit code is 3 when the export read-back differs from the scan. The other
sections are informational and never change the exit status.

## `debug`

```console
$ openqbw debug --help
```

Reverse-engineering diagnostics. Names, flags and output of these commands
may change in any release.

### `debug indexes [--fk-only] [--summary-only]`

```console
$ openqbw debug indexes mybooks.qbw
$ openqbw debug indexes mybooks.qbw --fk-only
$ openqbw debug indexes mybooks.qbw --summary-only
```

Lists every index from SYSINDEX (creator 0x01 0x46) and cross-validates
position attribution against index root pages. With `--fk-only`, restricts to
indexes whose name starts with `fkey_`. With `--summary-only`, prints only
the cross-validation result (agree / disagree / missing / orphan counts).

### `debug forensics`

```console
$ openqbw debug forensics mybooks.qbw
```

File-level discovery report covering:

- File and page-store stats (size, page count, AP learned
  block coverage).
- Catalog summary (total tables, user tables).
- Business-record summary (transaction headers, line items,
  distinct parent ids, orphan parents, childless headers,
  the sum of decoded line values, which is a sum of values and not
  of invoice totals).

A non-zero orphan-parent count is a discovery signal: the file
may contain partially purged records or a header table this
build does not parse yet.

### `debug validate-attribution`

```console
$ openqbw debug validate-attribution mybooks.qbw
```

Cross-checks the SYSTABLE-driven page->table map against the
width-band attribution derived from SYSCOLUMN.

### `debug fkgraph [--resolved-only]`

```console
$ openqbw debug fkgraph mybooks.qbw
```

Prints a name-heuristic foreign-key graph (`*_id`, `*_id_h`). With
`--resolved-only`, prints only edges that resolved to a target table.

### `debug nulls`

```console
$ openqbw debug nulls mybooks.qbw
```

Histogram of `SYSCOLUMN.nulls_flag` byte values with sample
column names per value. Useful for understanding the null /
default-marker discipline of a file.
