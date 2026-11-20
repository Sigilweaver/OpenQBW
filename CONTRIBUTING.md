# Contributing to OpenQBW

Thanks for your interest. OpenQBW is a reverse-engineering and parser
project for the QuickBooks Desktop company file format.

## Scope

OpenQBW only accepts contributions that target the **on-disk file
format** of company files that the lawful owner can already open. I do
**not** accept:

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

## Provenance

Format knowledge must come from public files and your own analysis, not
from Intuit code, SDKs or confidential documents, and not from decompiling,
tracing or bypassing protection in Intuit software. The rules, including
what to put in a PR or issue, are in the shared
[format provenance policy](https://github.com/Sigilweaver/ops/blob/main/PROVENANCE.md).
The policy allows a disclosed black-box cross-check against vendor software
you are licensed to run, as a cross-check only and never the sole basis for
a value.

## Reverse-engineering notes

If you discover new format details, record them in `SPECIFICATION.md` in
the section they belong to, with the evidence: sample byte ranges, the file
used, and counts. Treat the corpus as confidential -- never commit raw QBW
files or extracted PII.

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
