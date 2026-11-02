# Python API reference

The `openqbw` wheel is a PyO3 extension built from the same Rust core
that backs the CLI. It exposes a small, read-only surface: open a file,
then read the catalog, indexes, line items, and transaction headers as
`ScanReport` sequences of record objects. Type stubs (`.pyi`) and `py.typed`
ship with the package.

```python
import openqbw

r = openqbw.open("/path/to/file.qbw", lenient=True)
print(r.page_count, "pages,", r.file_size, "bytes")
```

## `openqbw.open(path, *, lenient=False) -> Reader`

Opens a `.qbw` file, checks its magic, version and page checksums, and
builds the catalog and page attribution. `path` is a `str` or path-like
object.

Reads are strict by default: a record or page that cannot be decoded raises
at the first one, and the message says to open the file with `lenient=True`.
Real files often need lenient mode: with `lenient=True` such records are
skipped and listed in each result's `rejected`, and an
`openqbw.OpenQBWWarning` is issued. The option applies to every read from the
returned `Reader`.

## `Reader`

| Member | Type | Description |
|---|---|---|
| `path` | `str` | The filesystem path the reader was opened from. |
| `lenient` | `bool` | Whether the reader skips undecodable records. |
| `page_count` | `int` | Number of pages in the underlying page-store. |
| `file_size` | `int` | File size in bytes. |
| `created_unix_seconds` | `int \| None` | Database creation time as Unix seconds, taken as UTC, or `None` when the stamp at superblock offset 0x2A is outside 1990..2100. |
| `attribution_gap` | `str \| None` | Why page-to-table attribution is unavailable, as a snake_case code, or `None`. |
| `tables()` | `ScanReport[Table]` | SYSTABLE catalog rows. |
| `indexes()` | `ScanReport[Index]` | SYSINDEX entries. |
| `line_items()` | `ScanReport[LineItem]` | Line items, attributed to a source table by page position (unverified). |
| `transactions()` | `ScanReport[Transaction]` | Transaction headers. |

## `ScanReport`

A `ScanReport` is a sequence of its records: it supports `len()`,
iteration, indexing and slicing. It also has:

- `items` - the list of records
- `rejected` - a list of `Rejected` (`kind`, `page_number`, `page_offset`,
  `message`) for records a lenient read skipped; always empty when strict
- `warnings` - a list of `ReadWarning` (`kind`, `message`) for conditions
  that apply to the whole read

Records are frozen objects with attributes and `to_dict()`. Every record has
a `flags` list naming the reasons it deserves less trust.

### `Table`

`table_id`, `name`, `col_count`, `data_root_page`, `last_page`,
`page_number`, `stamp_unix_seconds` (Unix seconds, taken as UTC, or `None`
when the row stamp is outside 1990..2100), `flags`.

`flags` includes `root_page_invalid` or `last_page_invalid` when that page
number is past the end of the file or not a data page; the number is kept as
read.

```python
for t in r.tables()[:5]:
    print(t.table_id, t.name)
```

### `Index`

`name`, `table_id`, `root_page`, `is_foreign_key`, `page_number`, `flags`.

```python
for idx in r.indexes()[:5]:
    print(idx.name, idx.table_id)
```

### `LineItem`

`qb_id_parent`, `item_qb_id`, `value` (a `decimal.Decimal`, or `None` when
the field was not decoded; never zero for "unknown"), `value_undecoded` (the
reason, or `None`), `value_raw` (bytes), `txn_date_raw`, `counter`,
`date_source`, `txn_date_days_since_unix`, `source_table`, `page_number`,
`page_offset`, `flags`. `to_dict()` uses the same names, so the parent id key
is `qb_id_parent`.

`txn_date_days_since_unix` is a converted date, best effort: the offset is
fitted to one sample and not checked against a date known from another
source. The raw value is `txn_date_raw`; CSV and SQLite exports carry only
that.

The value is not established as the line amount, and `source_table` is a
position-based, unverified attribution. A value field that does not fit the
SQL Anywhere NUMERIC layout, or sits on a 512-byte sector boundary, is
undecoded; `value_undecoded` is then one of `zero_length`, `digit_over_99`,
`exponent_out_of_range` or `sector_boundary`.

```python
lines = r.line_items()
for li in lines[:3]:
    print(li.qb_id_parent, li.value, li.source_table)
for w in lines.warnings:
    print("warning:", w.kind, w.message)
```

### `Transaction`

`qb_id`, `source_table`, `txn_type`, `txn_date_raw`, `counter`,
`date_source`, `txn_date_days_since_unix` (offset-fitted and unverified, as
above), `page_number`, `page_offset`, `flags`.

```python
for h in r.transactions()[:3]:
    print(h.qb_id, h.txn_type)
```

## Exceptions

`OpenQBWError` (a `ValueError`) is the base class.

- `OpenQBWIOError` (also an `OSError`): the file could not be read.
- `FormatError`: the file is not a supported company file, or a page is
  damaged.
- `RecordError` (a `FormatError`): a record could not be decoded.
- `ContentError` (a `FormatError`): the content is not readable.

Every exception has `kind`, `page_number` and `page_offset`.

## Build from source

```sh
pip install maturin
cd crates/openqbw-py
maturin develop --release
```

## Next

- [Python quickstart](./quickstart-python)
- [CLI reference](./cli)
- [Format specification](./specification)
