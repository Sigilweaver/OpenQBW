# OpenQBW

> Open specification and open-source parser for the **QuickBooks Desktop company file** format

Intuit has announced end-of-life for QuickBooks Desktop, forcing small
businesses to migrate to a more expensive cloud subscription or lose
access to their own books. OpenQBW is an effort to reverse engineer
the on-disk file format from publicly available files, write it up as a
specification, and ship a Rust parser so that the accounting data a
business has already paid for stays accessible.

## Non-goals

- Shipping, linking, or distributing any Intuit code or trademarks.
- Breaking passwords / DRM. OpenQBW targets the on-disk layout of
  company files that the lawful owner can already open.
- Writing `.QBW` files.

## Legal & ethical

- **No sources repository is published.** During corpus collection we
  found that several public GitHub repositories had accidentally committed
  real business financial data, including personal names, home addresses,
  phone numbers, and US Social Security / tax identification numbers.
  Affected repository owners have been contacted directly. Out of caution
  the full downloaded corpus is kept private and is not redistributed.
- QuickBooks® is a trademark of Intuit Inc. OpenQBW is independent.
- License: Apache-2.0. See [`LICENSE`](LICENSE).
