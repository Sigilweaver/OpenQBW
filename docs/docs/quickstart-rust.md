---
title: Rust quickstart
sidebar_label: Rust quickstart
---

# Rust quickstart

```rust
use openqbw::Company;

fn main() -> Result<(), openqbw::Error> {
    let company = Company::open("Company.QBW")?;

    for table in company.tables()? {
        println!("{:>4} rows  {}", table.row_count(), table.name());
    }

    for inv in company.invoices()? {
        println!("{}  {}  ${}", inv.txn_date, inv.customer, inv.total);
    }

    Ok(())
}
```

`Company::open` peels the additive-progression obfuscation layer
(via `opensqlany::ApModel`), then walks the underlying SA17 page
store, then layers the QuickBooks schema on top.
