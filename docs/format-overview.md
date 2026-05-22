# Format overview

This is a high-level orientation map. The detailed specification
is in [SPECIFICATION.md](../SPECIFICATION.md), and the empirical
notebook is in [re/NOTES.md](../re/NOTES.md).

## Three layers

A `.qbw` file is an onion:

```
+-------------------------------------------+
|  QuickBooks business layer                |
|  (invoices, transactions, customers, ...) |
|  -- parsed by `openqbw` crate             |
+-------------------------------------------+
|  SA17 page-store catalog                  |
|  (SYSTABLE, SYSCOLUMN, SYSINDEX, ...)     |
|  -- parsed by `openqbw` crate             |
+-------------------------------------------+
|  SA17 raw page store + AP obfuscation     |
|  (4096-byte pages, slot directories)      |
|  -- parsed by `opensqlany` crate          |
+-------------------------------------------+
```

## Layer 1: raw page store

Provided by [OpenSQLAnywhere](https://github.com/Sigilweaver/OpenSQLAnywhere).
The file is divided into 4096-byte pages. Each page has a trailer
at offset 0xFF0..0xFFF that includes a CRC and a one-byte page
type (`A` alloc, `E` extent, `C` catalog, `I` index, ...).

QuickBooks applies an **additive-progression cipher** on top: the
plaintext page byte at offset `i` in block `b` is `obfuscated[i]
- ap_table[b][i]`. The `ApModel` in `opensqlany` learns the
per-block additive table from the known-plaintext trailer.

## Layer 2: SA17 catalog

SQL Anywhere uses on-page system tables (SYSTABLE, SYSCOLUMN,
SYSINDEX, SYSOBJECT) instead of a separate metadata file. OpenQBW
parses these to enumerate user tables and their columns.

Empirical finding (C.57): all index root pages cluster in a
separate dbspace extent at file end, not adjacent to their owning
tables. This means SYSINDEX can validate index ownership at the
row level but **not** page-to-table attribution at the page level.

## Layer 3: QuickBooks business layer

QuickBooks stores each business object (invoice, line item, ...)
as a row in an `abmc_*` table. OpenQBW's `lineitem.rs` and
`transaction_header.rs` modules know the row layouts for the
key tables.

Page-to-table attribution is handled by `page_attribution.rs`
(SYSTABLE-driven) with `attribution_schema.rs` and
`attribution_content.rs` as schema-driven and content-signature
validators respectively. The combined width-band attribution is
the production answer (see WP-6Z.2).

## Where to read the code

- `crates/openqbw/src/lib.rs` -- top-level re-exports
- `crates/openqbw/src/lineitem.rs` -- invoice line-item record
- `crates/openqbw/src/transaction_header.rs` -- transaction header
- `crates/openqbw/src/page_attribution.rs` -- page-to-table map
- `crates/openqbw/src/sysindex.rs` -- index catalog
- `crates/openqbw-cli/src/main.rs` -- the CLI front-end
