# Migration guide: leaving QuickBooks Desktop

This guide shows how to use OpenQBW to extract your books from a
`.qbw` file in a format you can take to another accounting product
or hand to your accountant.

## Step 1: Make a backup

Copy the `.qbw` file to a working directory. OpenQBW is read-only,
but it's good practice:

```console
$ cp mybooks.qbw mybooks.qbw.bak
```

## Step 2: Inventory what is in the file

```console
$ openqbw catalog mybooks.qbw
```

This prints every user table. Look for the ones you care about:

- `abmc_*_header` -- transaction headers (invoices, bills, payments)
- `abmc_*_*line*` -- line items
- Customer, vendor, item, and account lists also live in `abmc_*`
  tables.

## Step 3: Pick an output format

OpenQBW today ships these export targets via the `openqbw migrate`
subcommand (when available -- see `openqbw --help` for the version
you have installed):

| Format | Use when                                            |
|--------|------------------------------------------------------|
| SQLite | You want to query the data, or import to BI tools    |
| CSV    | You want a lowest-common-denominator handoff         |
| IIF    | You want to import to another QB-compatible product  |

## Step 4: Export

```console
# SQLite (one file, all tables)
$ openqbw export mybooks.qbw --out books.sqlite

# CSV (one file per table; see migrate subcommand)
$ openqbw migrate mybooks.qbw --format csv --out csv-out/

# IIF (Intuit Interchange Format, importable by many tools)
$ openqbw migrate mybooks.qbw --format iif --out books.iif
```

## Step 5: Verify

Open the export in your target tool and reconcile a few totals
against what QuickBooks itself reported (a saved P&L PDF, a
sales-by-customer report, or a trial balance) before relying on
the export.

OpenQBW preserves cents (it does not introduce floating-point
rounding), so reconciliation to the penny is expected.

## Mapping notes

- **Dates** are stored as SA-days (days since 1900-01-01 minus an
  offset) in the on-disk format. OpenQBW emits them as ISO-8601
  date strings.
- **Amounts** are i64 cents on the QB side. Exports preserve
  this; CSV/IIF outputs include a separate decimal column for
  readability.
- **Foreign keys** are surfaced from the SA17 SYSINDEX catalog
  where present, and via a name-heuristic fallback otherwise.
- **Deleted records** are not recoverable in general. OpenQBW
  parses pages that the page-store currently considers live.

## Limitations

- Password-protected files: structural metadata only; row content
  remains obfuscated until QuickBooks decrypts it on open.
- Multi-user (.QBX, .QBA) files: not yet supported.
- Custom user-defined fields are exported as-is (raw bytes) when
  the encoding cannot be determined from the catalog.

If you hit a limitation, please open an issue with the file
size, QB version, and the failing command. **Do not attach the
file -- attach the redacted output only.**
