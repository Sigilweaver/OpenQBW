# Changelog

All notable changes to this project will be documented here. The
format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Docs: Python API reference page (`python-api`) covering `openqbw.open`,
  `Reader`, and its `tables()` / `indexes()` / `line_items()` /
  `transactions()` methods, registered in the sidebar. Fixes #1. (@Nabejo)

### Changed

- Upgraded `pyo3` from 0.22 to 0.29, clearing RUSTSEC-2025-0020 and
  RUSTSEC-2026-0177; the `--ignore` workarounds in the audit workflow
  are removed accordingly.

### Fixed

- `SysIndexIter`, `SysTableIter`, `SysColumnIter`, `TransactionHeaderIter`,
  and `bridge_owners_to_tables` validated the E-page bv oracle
  (`oracle_bv_e_page`) with a `candidate[0] == 0` check that is always
  true by construction, regardless of whether the guessed bv is correct.
  Whenever the higher-confidence C.36 anchor didn't fire, a wrong guess
  was silently accepted, corrupting catalog and header page decoding and
  cascading into wrong table attribution, orphaned parents, and an
  inflated invoice total on some files. All five call sites now go
  through `recover_bv_any`, which validates with an actual zero-density
  check instead. Fixes #15. Root cause identified and diagnosed by
  @pete-green.
- CI was red on `main`: a newer `clippy` (rust 1.97) added the
  `byte_char_slices` lint, which fired on two pre-existing test-only
  byte-array literals in `attribution_schema.rs`. Rewrote them as byte
  string literals per clippy's own suggestion. Unrelated to the pyo3
  bump above - this was already broken before it.

## [0.1.4] - 2026-07-06

### Changed

- PyPI package now declares `keywords` (`quickbooks`, `qbw`,
  `accounting`, `parser`) so the package is findable via PyPI search;
  previously only the crates.io side had them.

## [0.1.3] - 2026-07-04

### Fixed

- Bundle the `LICENSE` file into the sdist so source-based installs
  and conda-forge packaging carry the license text.

## [0.1.2] - 2026-07-04

### Changed

- Release workflow now builds and publishes a source distribution
  (sdist) to PyPI alongside the wheels, enabling source-based
  installs and conda-forge packaging.

## [0.1.1] - 2026-05-31

### Added

- `CITATION.cff`: author identity (Nathan Riley + ORCID) and a
  scaffolded `identifiers:` block ready for the Zenodo concept DOI.

### Changed

- README badge block unified across the Sigilweaver portfolio.

## [0.1.0] - 2026-05-22

First publication-ready release.

### Added

- `openqbw` library: read-only parser for QuickBooks Desktop `.QBW`
  company files. Peels Intuit's additive-progression obfuscation off
  the underlying SA17 page store via the `opensqlany` companion crate,
  then enumerates user tables, parses `SYSTABLE` / `SYSCOLUMN` /
  `SYSINDEX` / `SYSOBJECT` system catalogs, and extracts invoice
  headers + line items.
- `openqbw` CLI: `inspect`, `tables`, `migrate` (CSV / SQLite / IIF),
  and `forensics` subcommands.
- `openqbw` Python wheel (`pip install openqbw`): pyo3 bindings
  exposing the high-level reader API, abi3-py39, classifiers for
  Office/Business :: Financial :: Accounting.
- `SPECIFICATION.md` covering the `.QBW` on-disk format and the
  reverse-engineering record in `re/NOTES.md`.
- Workspace metadata, MSRV 1.87, `unsafe_code = "forbid"` on
  pure-Rust crates (excluded on the pyo3 binding crate by necessity).
- CI matrix (Linux + macOS + Windows): `cargo fmt`, `cargo clippy
  --workspace --all-targets -- -D warnings`, `cargo test`, and a
  per-OS `maturin build` wheel job.
- Tag-triggered release workflow: publishes `openqbw` and
  `openqbw-cli` to crates.io via trusted publishing, plus
  `maturin publish` of the Python wheel to PyPI.
- `CHANGELOG.md`, `CONTRIBUTING.md`, `SECURITY.md`, `NOTICE`.
- Documentation site at <https://sigilweaver.app/openqbw/docs/>.

### Validated

- On the publicly-available Rock Castle sample, the line-item
  extractor reaches 13,375 / 13,375 invoices and a grand total of
  $399,914,792.78, matching the value QuickBooks itself reports
  for the same file.

[Unreleased]: https://github.com/Sigilweaver/OpenQBW/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Sigilweaver/OpenQBW/releases/tag/v0.1.0
