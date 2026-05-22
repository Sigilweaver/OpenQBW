# Security Policy

## Supported Versions

| Version | Supported |
| ------- | --------- |
| latest  | Yes       |
| older   | No        |

Only the latest published release receives security updates.

## Reporting a Vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Report privately via [GitHub Security Advisories](https://github.com/Sigilweaver/OpenQBW/security/advisories/new).

Include:

- A description of the vulnerability and its potential impact.
- Steps to reproduce or a proof of concept.
- The affected crate or package (`openqbw`, `openqbw-cli`, or
  `openqbw` Python wheel).
- The OS, Rust toolchain or Python version, and version you were
  running.

Expect an initial acknowledgment within 7 days.

## Sensitive-data note for reproducers

`.QBW` files are live accounting databases. They contain personal
names, addresses, bank-routing numbers, tax IDs, and Social Security
Numbers. **Do not attach a real `.QBW` file to a bug report.**
Either reproduce the issue against the publicly-available Rock
Castle sample, or hand-construct a minimal failing input. If a real
file is unavoidable, request a private channel via the Security
Advisory before sending anything.

## Scope

In scope:

- **Parser correctness on malicious input.** Crashes (panics,
  out-of-bounds reads, infinite loops), arbitrary file writes, or
  memory corruption triggered by a crafted `.QBW` file are in scope.
  The pure-Rust crates are `#![forbid(unsafe_code)]`.
- **Path-traversal or arbitrary-file-write bugs** in `openqbw`
  CLI's `migrate` exporter, or in the Python wheel.
- **Supply-chain integrity** of published artifacts on crates.io and
  PyPI: tampered manifests, missing provenance, unsigned releases.

Out of scope:

- Denial of service via legitimately oversized company files.
- Vulnerabilities in third-party crates (notably `pyo3`, `rusqlite`,
  `rusqlite`'s bundled SQLite) with no demonstrated exploit path
  through this stack. Forward those upstream.
- Format-spec inaccuracy or coverage gaps - file those as regular
  GitHub issues.

## Disclosure

We follow coordinated disclosure. Reporters are credited in the
release notes unless they prefer to remain anonymous. We aim to ship
a fix within 30 days of confirming a high or critical issue.

## Reverse-engineering and trademarks

OpenQBW is clean-room: it is derived from observation of public-domain
`.QBW` artifacts and from published SAP SQL Anywhere documentation.
It ships with no Intuit or SAP code or binaries. QuickBooks(R) is a
trademark of Intuit Inc. OpenQBW is independent. Bug reports about
parser inaccuracy or unsupported schema versions are welcome but are
not security issues.
