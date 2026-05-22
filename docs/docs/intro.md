---
title: Introduction
sidebar_label: Intro
slug: /
---

# OpenQBW

**Pure-Rust reader and open specification for Intuit QuickBooks
Desktop `.QBW` company files.**

OpenQBW is a clean-room project. It is derived from observation of
the on-disk bytes of `.QBW` files and from public documentation
of the underlying database engine. It ships with no Intuit or SAP
code or binaries.

## Why

Intuit has announced end-of-life dates for QuickBooks Desktop.
Companies that have kept books in QuickBooks Desktop for decades
need a way to migrate that data to other accounting products, or
just to preserve it in an open format, **without** depending on
the QuickBooks Desktop application continuing to install and run.

OpenQBW reads the `.QBW` file directly and exports to CSV, SQLite,
or IIF.

## What you can do today

- Open a `.QBW` file with no QuickBooks installed.
- Enumerate user tables via the `SYSTABLE` catalog.
- Parse `SYSCOLUMN`, `SYSINDEX`, `SYSOBJECT`.
- Extract invoice line items, transaction headers, and width-band
  attribution metadata.
- Export to CSV, SQLite, or Intuit's own IIF interchange format.

Validated on the Rock Castle Construction sample file:
**13,375 / 13,375** invoices recovered, grand total
**$399,914,792.78**.

## How it stacks

```
.QBW file
   |
   v
 OpenQBW                 (QuickBooks schema, invoice extraction, migrate)
   |
   v
 OpenSQLAnywhere         (SA17 page store + AP deobfuscation primitive)
```

`OpenSQLAnywhere` is the companion project; see
[https://sigilweaver.app/opensqlanywhere/docs/](https://sigilweaver.app/opensqlanywhere/docs/).

## Get started

- [Install](./install.md)
- [CLI quickstart](./quickstart-cli.md)
- [Rust quickstart](./quickstart-rust.md)
- [Python quickstart](./quickstart-python.md)
- [Migration guide](./migration-guide.md) - the recommended path
  for accountants and end users
