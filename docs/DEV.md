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

The shell provides the Rust toolchain and bwrap.

## Build and run

```
cargo build
cargo run --bin bman -- ls
```

To generate scenario-backed examples (runtime captured stdout/stderr), run:

```
cargo run --bin bman -- ls --run-scenarios
```

To generate a portable doc pack (pack + scenarios + fixtures + outputs) and run
scenarios:

```
cargo run --bin bman -- ls --doc-pack /tmp/ls-docpack --run-scenarios
```

Scenarios are defined per-binary under `scenarios/<binary>.json` (or
`<doc-pack>/scenarios/<binary>.json` when `--doc-pack` is set) and can be
overridden with `--scenarios <file>`. Scenario runs are appended to the existing
pack under `out/packs/<binary>/binary.lens/runs/` and summarized in
`out/man/<binary>/examples_report.json` (or `<doc-pack>/man/examples_report.json`).
Doc packs also include usage lens templates under `<doc-pack>/queries/`.

Use `--lens-flake <ref>` to point at a different `binary_lens` flake if needed.

Fixture-backed scenarios can declare:

- `seed_dir`: path to a fixture tree copied into the per-run work dir
- `cwd`: relative working directory inside the seeded tree (defaults to `.`)

`seed_dir` is resolved relative to the doc-pack root (the parent of the
`scenarios/` directory), so doc packs can be moved without path fixes.

For a principled approach to expanding scenario coverage (options vs behaviors
vs doc claims) and curating `.SH EXAMPLES`, see `docs/COVERAGE.md`.
