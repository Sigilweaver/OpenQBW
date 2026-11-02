---
title: Python quickstart
sidebar_label: Python quickstart
---

# Python quickstart

```sh
pip install openqbw
```

```python
import openqbw

# Strict is the default and raises at the first record it cannot decode.
# Real files often need lenient mode, which skips and lists those records.
r = openqbw.open("Company.QBW", lenient=True)
print(r.page_count, "pages,", r.file_size, "bytes")

for t in r.tables():
    print(t.table_id, t.name)

lines = r.line_items()
for li in lines[:5]:
    # value is a decimal.Decimal, or None when it was not decoded.
    print(li.qb_id_parent, li.value, li.source_table)

undecoded = sum(1 for li in lines if li.value is None)
print(f"{len(lines)} line items, {undecoded} with an undecoded value")

for w in lines.warnings:
    print("warning:", w.kind, w.message)
for rej in lines.rejected:
    print("skipped:", rej.kind, rej.page_number, rej.page_offset, rej.message)
```

`value` is the line's numeric value field. That it is the line's amount is
not established, and `source_table` is a position-based, unverified
attribution.

## Skip records that cannot be decoded

Reads are strict by default: `openqbw.open("Company.QBW")` raises
`RecordError` at the first record that cannot be decoded, and the message says
to open the file with `lenient=True`. With `lenient=True`, such records are
skipped and listed in `rejected` instead of raising.

## Export to SQLite

Use the command line tool, which writes the same data that these methods
read:

```sh
openqbw export Company.QBW company.sqlite
```

See the [Python API reference](./python-api) for every attribute.
