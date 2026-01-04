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

## Basic workflow (M5 fast-pass)

```
# Provide an LM planner command (JSON in/out) and extract the surface.
BVM_PLANNER_CMD=/path/to/planner cargo run -- surface /usr/bin/ls --out-dir ./out

# Or replay a precomputed plan JSON (still required for the run).
BVM_PLANNER_PLAN=/path/to/plan.json cargo run -- surface /usr/bin/ls --out-dir ./out
```

## Notes

- The dev shell exports `RUST_BACKTRACE=1` for better diagnostics.
- `reports/` and `out/` are gitignored; `.gitkeep` keeps the directories in the repo.
