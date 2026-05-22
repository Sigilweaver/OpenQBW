---
title: CLI quickstart
sidebar_label: CLI quickstart
---

# CLI quickstart

```sh
openqbw inspect Company.QBW
openqbw tables  Company.QBW
openqbw migrate sqlite Company.QBW --out company.sqlite
openqbw migrate csv    Company.QBW --out company_csv/
openqbw migrate iif    Company.QBW --out company.iif
```

See the full [CLI reference](./cli.md) for every subcommand
and flag.
