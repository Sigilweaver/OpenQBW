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

Wheels are published for Python 3.10+ on Linux, macOS, and Windows.

## Rust library

```toml
[dependencies]
openqbw = "0.2"
```

MSRV: Rust 1.95.

## From source

```sh
git clone https://github.com/Sigilweaver/OpenQBW
cd OpenQBW
cargo build --release --locked
./target/release/openqbw --help
```

The `opensqlany` dependency comes from crates.io; no sibling checkout is
needed. To build the Python extension from source, see the
[Python API reference](./python-api).
