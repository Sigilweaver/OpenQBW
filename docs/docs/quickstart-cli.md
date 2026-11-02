---
title: CLI quickstart
sidebar_label: CLI quickstart
---

# CLI quickstart

```sh
openqbw catalog Company.QBW
openqbw schema  Company.QBW abmc_invoice_header
openqbw export  Company.QBW company.sqlite
openqbw migrate Company.QBW --format csv --out company_csv/
openqbw migrate Company.QBW --format iif --placeholder-accounts --out company.iif
```

Add `--lenient` to any command to skip records that cannot be decoded
instead of stopping; each is listed on stderr and in the export.

See the full [CLI reference](./cli.md) for every subcommand
and flag.
