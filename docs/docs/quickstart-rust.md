---
title: Rust quickstart
sidebar_label: Rust quickstart
---

# Rust quickstart

```toml
[dependencies]
openqbw = "0.2"
```

```rust,no_run
use openqbw::{Decimal, ReadOptions, Reader};

fn main() -> Result<(), openqbw::Error> {
    // Strict is the default (`ReadOptions::default()`) and stops at the first
    // record it cannot decode. Real files often need `ReadOptions::lenient()`,
    // which skips those records and lists them in `rejected`.
    let reader = Reader::open("Company.QBW", &ReadOptions::lenient())?;

    for table in &reader.catalog().items {
        println!("{:>6}  {}", table.table_id, table.name);
    }

    let lines = reader.line_items()?;
    for w in &lines.warnings {
        eprintln!("warning: {w}");
    }
    for e in &lines.rejected {
        eprintln!("skipped: {e}");
    }

    // `value` is an exact `Decimal`; `None` means it was not decoded, not zero.
    let decoded = Decimal::checked_sum(lines.items.iter().filter_map(|li| li.value));
    let undecoded = lines.items.iter().filter(|li| li.value.is_none()).count();
    println!("sum of decoded values: {decoded:?}; {undecoded} lines not decoded");

    Ok(())
}
```

`Reader::open` checks the file's magic, version and page checksums, decodes
the pages (opensqlany removes the additive-progression obfuscation), and reads
the `SYSTABLE` catalog. `line_items()`, `transactions()`, `indexes()`,
`columns()` and `schema(table)` read from it.

Reads are strict with `ReadOptions::default()`: a record the decoder cannot
read with confidence is an error. `ReadOptions::lenient()` skips such records
and lists them in `rejected`.

`value` is the line's numeric value field. That it is the line's amount is
not established, and `source_table` is a position-based, unverified
attribution. See [docs.rs/openqbw](https://docs.rs/openqbw) for the full API.
