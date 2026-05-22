# Contributing to OpenQBW

Thanks for your interest. OpenQBW is a reverse-engineering and parser
project for the QuickBooks Desktop company file format.

## Scope

OpenQBW only accepts contributions that target the **on-disk file
format** of company files that the lawful owner can already open. We
do **not** accept:

- Code or assets derived from disassembled Intuit binaries.
- Password recovery, DRM bypass, or other tools that defeat access
  controls on files the contributor does not own.
- Anything that ships Intuit trademarks or copyrighted content.

## Workflow

1. Open an issue describing what you want to change.
2. Fork and branch from `main`.
3. Run `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
   and `cargo test --workspace` before pushing.
4. Open a pull request. Small, focused PRs land faster.

## Licensing

By submitting a contribution, you agree it is licensed under the
Apache License, Version 2.0 (the same license as the rest of the
project). See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).

## Reverse-engineering notes

If you discover new format details, append a numbered entry to
`re/NOTES.md` (rather than rewriting prior notes). Keep evidence,
sample byte ranges, and the file used. Treat the corpus as
confidential -- never commit raw QBW files or extracted PII.
