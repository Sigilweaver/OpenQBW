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

OpenQBW ships these export targets (`openqbw export` and
`openqbw migrate`; see `openqbw --help`):

| Format | Use when                                            |
|--------|------------------------------------------------------|
| SQLite | You want to query the data, or import to BI tools    |
| CSV    | You want a lowest-common-denominator handoff         |
| IIF    | You want to import to another QB-compatible product  |

## Step 4: Export

```console
# SQLite (one file, all tables)
$ openqbw export mybooks.qbw books.sqlite

# CSV (a directory of files)
$ openqbw migrate mybooks.qbw --format csv --out csv-out/

# IIF (Intuit Interchange Format). Account names are not decoded, so the
# file uses placeholder accounts
$ openqbw migrate mybooks.qbw --format iif --placeholder-accounts --out books.iif
```

## Step 5: Verify

Open the export in your target tool and reconcile a few totals
against what QuickBooks itself reported (a saved P&L PDF, a
sales-by-customer report, or a trial balance) before relying on
the export.

OpenQBW keeps values as exact decimals (it does not introduce
floating-point rounding). The value is the line's numeric value
field; that it is the line amount is not established, so reconcile
before relying on it.

## Mapping notes

- **Dates** are stored as SA-day numbers in the on-disk format. Exports
  write the raw number as `txn_date_raw` with a `date_source` column
  recording how it was found (a record with no date, or two candidate
  dates, has none). They are not converted to calendar dates, because the
  conversion offset is fitted to one sample and not checked against a date
  known from another source.
- **Values** are exact decimals (`+/- coef * 100^e`). Exports write
  them as decimal text, with the raw bytes in hex beside them.
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
