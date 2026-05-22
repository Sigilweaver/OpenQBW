---
title: Install
sidebar_label: Install
---

# Install

## Command-line tool (recommended for most users)

```sh
cargo install openqbw-cli
openqbw --help
```

## Python

```sh
pip install openqbw
```

Wheels are published for Python 3.9+ on Linux, macOS, and Windows.

## Rust library

```toml
[dependencies]
openqbw = "0.1"
```

MSRV: Rust 1.87.

## From source

```sh
git clone https://github.com/Sigilweaver/OpenQBW
cd OpenQBW
cargo build --release
./target/release/openqbw --help
```

Note: from-source builds also need OpenSQLAnywhere checked out
as a sibling directory (`../OpenSQLAnywhere`), because Cargo
resolves it as a path dependency in development.
