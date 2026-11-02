---
title: CLI quickstart
sidebar_label: CLI quickstart
---

# CLI quickstart

```sh
openqbw --lenient catalog Company.QBW
openqbw --lenient schema  Company.QBW abmc_invoice_header
openqbw --lenient export  Company.QBW company.sqlite
openqbw --lenient migrate Company.QBW --format csv --out company_csv/
openqbw --lenient migrate Company.QBW --format iif --placeholder-accounts --out company.iif
```

Strict is the default and stops at the first record it cannot decode. Real files often need lenient mode, which skips those records and lists them. Each skipped record is printed on stderr and listed
in the export. Drop `--lenient` to make any command fail on the first one.

Export commands write to a temporary file and replace the output only on
success, and refuse to write over the input file or a QuickBooks file.

See the full [CLI reference](./cli.md) for every subcommand
and flag.
