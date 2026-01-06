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

The shell provides the Rust toolchain.

## Build and run

```
cargo build
cargo run -- /path/to/bin --out-dir ./out
```

The help artifact is written under `out/context/<binary-name>/help.txt`.

Additional artifacts:

```
out/context/<binary-name>/help.stdout.txt
out/context/<binary-name>/help.stderr.txt
out/context/<binary-name>/context.json
```

`context.json` includes a binary hash along with the capture metadata.
