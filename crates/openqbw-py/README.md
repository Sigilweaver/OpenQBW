# openqbw (Python)

Python bindings for [OpenQBW](https://github.com/Sigilweaver/OpenQBW), a
read-only parser for QuickBooks `.qbw` files. Targets data-liberation
and forensic workflows.

> Prototype quality. See the parent repository's `README.md` and
> `SPECIFICATION.md` for what is and is not yet supported.

## Install (from source)

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

r = openqbw.open("/path/to/file.qbw")
print(r.page_count, "pages,", r.file_size, "bytes")

for t in r.tables()[:5]:
    print(t["table_id"], t["name"])

for li in r.line_items()[:3]:
    print(li["invoice_id"], li["amount_cents"], li["source_table"])

for h in r.transactions()[:3]:
    print(h["qb_id"], h["txn_type"])
```

## API

`openqbw.open(path) -> Reader`

`Reader` attributes:
- `path`, `page_count`, `file_size`

`Reader` methods (each returns a list of dicts):
- `tables()` — SYSTABLE catalog rows
- `indexes()` — SYSINDEX entries
- `line_items()` — invoice line items, attributed to source tables
- `transactions()` — transaction headers
