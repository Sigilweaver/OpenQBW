# Contributing to OpenQBW

Thanks for your interest. OpenQBW is a reverse-engineering and parser
project for the QuickBooks Desktop company file format.

## Scope

OpenQBW only accepts contributions that target the **on-disk file
format** of company files that the lawful owner can already open. We do
**not** accept:

- Code or assets derived from disassembled Intuit binaries.
- Password recovery, DRM bypass, or other tools that defeat access
  controls on files the contributor does not own.
- Anything that ships Intuit trademarks or copyrighted content.

## Contributing code (pull requests)

PRs are welcome for changes of any size, including large or breaking ones -
there's no requirement to open an issue first. That said, for larger changes
you may want to open an issue before writing code, especially if you're
unsure whether it fits the project's direction: a large PR that conflicts
with the roadmap can still be rejected even if the code itself is solid, and
an issue is a cheap way to check alignment before investing the time.

For any PR:

- Scope it to one logical change.
- Fork and branch from `main`.
- Run `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace` before pushing.
- Update [CHANGELOG.md](CHANGELOG.md) under `## [Unreleased]` with a
  short bullet describing the user-visible change.
- Prefer [Conventional Commits](https://www.conventionalcommits.org/)
  (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`).
- Code is ASCII only and `#![forbid(unsafe_code)]`.

## Vendor software and clean-room policy

This project is maintained clean-room: format knowledge must come from
your own analysis of files you have a right to read, not from Intuit's
SDK, documentation, or software. Do not run, depend on, or validate your
implementation against QuickBooks Desktop itself, or any tool that reads
the format through Intuit's own libraries - not in CI, not in tests, not
in local development. Correctness is argued only from independent
analysis of the on-disk format and roundtrip/self-consistency invariants.

**Pull requests that were written or verified with the help of
proprietary vendor software will not be accepted**, regardless of code
quality, since accepting them would compromise the project's clean-room
provenance. If you've found a bug this way, or you'd simply rather not
write the fix yourself, please open an issue instead. Describe the
symptom on the input that triggers it - what's wrong, and on what file -
without pasting vendor tool output, vendor source, or values you learned
by running vendor software. We'll investigate and fix it from
independent analysis. Detailed issue reports are genuinely useful and
will be acted on.

## Reverse-engineering notes

If you discover new format details, append a numbered entry to
`re/NOTES.md` (rather than rewriting prior notes). Keep evidence, sample
byte ranges, and the file used. Treat the corpus as confidential -- never
commit raw QBW files or extracted PII.

## Security

Please report security vulnerabilities privately via GitHub Security
Advisories - see [SECURITY.md](SECURITY.md). Do not open public issues
for vulnerabilities.

## DCO

By submitting a contribution you certify that you have the right
to submit the work under the project license (Apache-2.0) and
agree to the
[Developer Certificate of Origin](https://developercertificate.org/).

## License

By submitting a contribution, you agree it is licensed under the
Apache License, Version 2.0 (the same license as the rest of the
project). See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
