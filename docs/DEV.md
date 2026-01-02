# Dev Environment

This repo uses Nix flakes and direnv for a reproducible development shell.

## Prereqs

- Nix with flakes enabled
- direnv

## Enter the dev shell

1) Allow direnv in this repo:

```
direnv allow
```

2) Alternatively, enter the shell directly:

```
nix develop
```

The shell provides the Rust toolchain plus helpers for binary inspection and tracing.

## Build and run

```
cargo build
cargo run -- --help
```

## Basic workflow (stubs for now)

```
# Synthesize claims from existing docs into a claims JSON
cargo run -- claims --man ./path/to/manpage.1 --help-text ./help.txt --out ./claims.json

# Validate claims by executing the binary under controlled env constraints
cargo run -- validate --binary /usr/bin/ls --claims ./claims.json --out ./validation.json

# Render a man page view and a machine-readable report
cargo run -- regenerate --binary /usr/bin/ls --claims ./claims.json --results ./validation.json --out-man ./ls.1 --out-report ./report.json

# Inspect claims and validation results in a TUI
cargo run -- inspect --claims ./claims.json --results ./validation.json

# TUI keys
# - t: toggle claims list vs source view
# - tab: cycle claims on the selected source line
```

## Notes

- The dev shell exports `RUST_BACKTRACE=1` for better diagnostics.
- `reports/` and `out/` are gitignored; `.gitkeep` keeps the directories in the repo.
