# openqbw (Python)

Python bindings for [OpenQBW](https://github.com/Sigilweaver/OpenQBW), a
read-only parser for QuickBooks `.qbw` files. Targets data-liberation
and forensic workflows.

> Prototype quality. See the parent repository's `README.md` and
> `SPECIFICATION.md` for what is and is not yet supported.

## Install

```bash
pip install openqbw
```

Python 3.10 or later. Wheels (abi3) are published for Linux, macOS and
Windows.

To build from a clone instead:

```bash
pip install maturin
cd OpenQBW/crates/openqbw-py
maturin develop --release
```

This builds the extension and installs it into the active Python
environment.

## Quick start

```python
import openqbw

# Strict is the default and raises at the first record it cannot decode.
# Real files often need lenient mode, which skips and lists those records.
r = openqbw.open("/path/to/file.qbw", lenient=True)
print(r.page_count, "pages,", r.file_size, "bytes")

for t in r.tables()[:5]:
    print(t.table_id, t.name)

lines = r.line_items()
for li in lines[:3]:
    # value is a decimal.Decimal, or None when the field was not decoded; it
    # is never 0 for "unknown". It is not established as the line amount, and
    # source_table is a position-based, unverified attribution.
    print(li.qb_id_parent, li.value, li.value_undecoded, li.source_table, li.flags)
for w in lines.warnings:
    print("warning:", w.kind, w.message)

for h in r.transactions()[:3]:
    print(h.qb_id, h.txn_type)

# Each skipped record is listed.
for rej in lines.rejected:
    print(rej.kind, rej.page_number, rej.page_offset, rej.message)
```

## API

`openqbw.open(path, *, lenient=False) -> Reader`

Reads are strict: a record or page that cannot be decoded with confidence
raises (the message says to open the file with `lenient=True`). With
`lenient=True` such records are skipped, listed in each result's `rejected`,
and an `openqbw.OpenQBWWarning` is issued.

`Reader` attributes: `path`, `lenient`, `page_count`, `file_size`,
`created_unix_seconds` (the stored count taken as UTC), `attribution_gap`.

`LineItem.txn_date_days_since_unix` is a converted date, best effort: the
offset is fitted to one sample and not checked against a date known from
another source. The raw value is `txn_date_raw`. `LineItem.value_undecoded` is
`None` or one of `zero_length`, `digit_over_99`, `exponent_out_of_range` or
`sector_boundary`.

`Reader` methods, each returning a `ScanReport`:

- `tables()` - `Table` rows of the SYSTABLE catalog
- `indexes()` - `Index` rows of the SYSINDEX catalog
- `line_items()` - `LineItem` records, attributed to source tables by page position (unverified)
- `transactions()` - `Transaction` headers

A `ScanReport` is a sequence of its `items` and also has `rejected` (a list
of `Rejected`: `kind`, `page_number`, `page_offset`, `message`) and
`warnings` (a list of `ReadWarning`: `kind`, `message`). Records have
attributes, a `flags` list naming the reasons a record deserves less trust,
and `to_dict()`.

Exceptions: `OpenQBWError` (a `ValueError`) is the base class.
`OpenQBWIOError` (also an `OSError`) means the file could not be read;
`FormatError` means it is not a supported company file or a page is
damaged; `RecordError` (a `FormatError`) means a record could not be
decoded; `ContentError` (a `FormatError`) means the content is not
readable. Every exception has `kind`, `page_number` and `page_offset`.

Type stubs (`.pyi`) and `py.typed` ship with the package.

## Tests

```bash
pip install pytest
pytest crates/openqbw-py/tests
```
