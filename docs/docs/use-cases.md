# Use cases

OpenQBW exists to keep QuickBooks Desktop data accessible after
Intuit's end-of-life announcement. This document walks through the
four most common scenarios.

## 1. Data liberation: leaving the QuickBooks ecosystem

You have a `.qbw` file, an expiring desktop license, and you want
your data out in a portable format so you can move to another
accounting product or just archive it.

```console
# Get a SQL database you can query with any tool
$ openqbw --lenient export mybooks.qbw books.sqlite

# Inspect from sqlite3 or DB Browser
$ sqlite3 books.sqlite "SELECT COUNT(*) FROM transaction_line_items;"
$ sqlite3 books.sqlite "SELECT COUNT(*) FROM transaction_line_items WHERE value IS NULL;"
```

Strict is the default and stops at the first record the reader cannot decode.
Real files often need `--lenient`, which skips those records and lists them in
the export.

See [migration-guide.md](migration-guide.md) for the
target-product-specific mapping advice.

## 2. Forensic accounting and litigation support

A `.qbw` file has been produced in discovery. The QuickBooks
application is not available on the analyst's workstation, or the
opposing party has not produced credentials.

OpenQBW reads the on-disk layout. It does **not** crack passwords
or bypass DRM. If the file has a password set in QuickBooks, you
will only see metadata and obfuscated rows -- not the cleartext
financial data. You still get:

- Table inventory and row counts
- Schema and index metadata
- File-level metadata that can support timeline analysis

For files without a QuickBooks-level password, you get the full
parsed transaction headers and line items.

## 3. Audit and discovery

You suspect a record was deleted or you want to inventory the
schema. OpenQBW can tell you:

```console
# Tables present in the file
$ openqbw --lenient catalog mybooks.qbw

# Index inventory (FK indexes only)
$ openqbw debug indexes mybooks.qbw --fk-only

# Per-table null-flag histogram (suggests defaults / deletions)
$ openqbw debug nulls mybooks.qbw

# Export to a temporary file and report coverage and undecoded values
$ openqbw --lenient verify mybooks.qbw
```

## 4. Long-term archival

QuickBooks files are sometimes the only complete record of a
business's books for the years before a SaaS migration. OpenQBW
gives you a deterministic, open-format snapshot you can keep
alongside the original file:

```console
$ openqbw --lenient export mybooks.qbw archive/books-2024.sqlite
$ sha256sum archive/books-2024.sqlite > archive/books-2024.sqlite.sha256
```

A SQLite file is bit-stable across decades and readable by every
mainstream programming language and analytics tool.
