# Python API reference

The `openqbw` wheel is a PyO3 extension built from the same Rust core
that backs the CLI. It exposes a small, read-only surface: open a file,
then pull the catalog, indexes, line items, and transaction headers as
lists of dicts.

```python
import openqbw

r = openqbw.open("/path/to/file.qbw")
print(r.page_count, "pages,", r.file_size, "bytes")
```

## `openqbw.open(path) -> Reader`

Opens a `.qbw` file, decodes the page store, and learns the additive
progression model needed to attribute pages to tables. Raises
`OSError` if the file cannot be opened.

## `Reader`

| Member | Type | Description |
|---|---|---|
| `path` | `str` | The filesystem path the reader was opened from. |
| `page_count` | `int` | Number of pages in the underlying page-store. |
| `file_size` | `int` | File size in bytes. |
| `created_unix_seconds` | `int \| None` | Database creation time as Unix seconds (UTC), or `None` when the stamp at superblock offset 0x2A is outside 1990..2100. |
| `tables()` | `list[dict]` | SYSTABLE catalog rows. |
| `indexes()` | `list[dict]` | SYSINDEX entries. |
| `line_items()` | `list[dict]` | Line items, attributed to a source table by page position (unverified). |
| `transactions()` | `list[dict]` | Transaction headers. |

### `tables()`

Each dict has `table_id`, `name`, `col_count`, `data_root_page`,
`last_page`, `page_number`, `stamp_unix_seconds` (Unix seconds, UTC, or
`None` when the row stamp is outside 1990..2100).

```python
for t in r.tables()[:5]:
    print(t["table_id"], t["name"])
```

### `indexes()`

Each dict has `name`, `table_id`, `root_page`, `page_number`.

```python
for idx in r.indexes()[:5]:
    print(idx["name"], idx["table_id"])
```

### `line_items()`

Each dict has `invoice_id`, `item_qb_id`, `value` (a
`decimal.Decimal`, or `None` when the field was not decoded; never
zero for "unknown"), `value_undecoded` (the reason, or `None`),
`value_raw` (bytes), `txn_date_days_since_unix`, `source_table`,
`page_number`, `page_offset`. The value is not established as the
line amount, and `source_table` is a position-based, unverified
attribution.

```python
for li in r.line_items()[:3]:
    print(li["invoice_id"], li["value"], li["source_table"])
```

### `transactions()`

Each dict has `qb_id`, `source_table`, `txn_type`, `page_number`,
`page_offset`.

```python
for h in r.transactions()[:3]:
    print(h["qb_id"], h["txn_type"])
```

## Next

- [Python quickstart](./quickstart-python)
- [CLI reference](./cli)
- [Format specification](./specification)
