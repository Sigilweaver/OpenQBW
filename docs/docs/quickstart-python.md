---
title: Python quickstart
sidebar_label: Python quickstart
---

# Python quickstart

```python
import openqbw

company = openqbw.Company("Company.QBW")

for table in company.tables():
    print(f"{table.row_count:>4} rows  {table.name}")

for inv in company.invoices():
    print(inv.txn_date, inv.customer, inv.total)
```

## Migrate to SQLite

```python
import openqbw

openqbw.migrate_sqlite("Company.QBW", "company.sqlite")
```

The Python wheel is an `abi3-py39` build of the same Rust code that
backs the CLI - same parser, same correctness guarantees, same
performance.
