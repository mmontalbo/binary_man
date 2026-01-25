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
```

Optionally generate a doc pack with `binary_lens` (useful for debugging pack output):

```
nix run <lens-flake> -- <binary> -o /tmp/ls-docpack
```

Bootstrap a doc pack (pack + templates + config) and run the enrichment loop:

```
cargo run --bin bman -- init --doc-pack /tmp/ls-docpack --binary ls
cargo run --bin bman -- validate --doc-pack /tmp/ls-docpack
cargo run --bin bman -- plan --doc-pack /tmp/ls-docpack
cargo run --bin bman -- apply --doc-pack /tmp/ls-docpack
cargo run --bin bman -- status --doc-pack /tmp/ls-docpack
cargo run --bin bman -- status --doc-pack /tmp/ls-docpack --json
```

Status-first bootstrap (empty dir, no pack yet):

```
cargo run --bin bman -- status --doc-pack /tmp/empty --json
# edit enrich/bootstrap.json (set binary)
cargo run --bin bman -- init --doc-pack /tmp/empty
cargo run --bin bman -- validate --doc-pack /tmp/empty
cargo run --bin bman -- plan --doc-pack /tmp/empty
cargo run --bin bman -- apply --doc-pack /tmp/empty
```

`enrich/config.json` declares the inputs and requirements to evaluate. `bman`
enforces the loop with `validate → plan → apply`, and `status` reports a
decision of `complete`, `incomplete`, or `blocked` based on evidence-linked
requirements and blockers. `apply` writes `enrich/report.json` with the latest
evaluation.

Scenarios are defined under `<doc-pack>/scenarios/plan.json`. Scenario runs are
appended to the pack under `<doc-pack>/binary.lens/runs/` and summarized in
`<doc-pack>/man/examples_report.json` when planned. Usage + discovery templates
are installed under `<doc-pack>/queries/` and referenced by `enrich/config.json`,
with the pack's `binary.lens/views/queries/` templates used as fallbacks.
Enrichment control and state live under `<doc-pack>/enrich/`, including
`bootstrap.json`, `config.json`, `lock.json`, and `plan.out.json`.
Surface inventory lives under `<doc-pack>/inventory/surface.json` with scenario
evidence in `<doc-pack>/inventory/scenarios/*.json`; `coverage_ledger.json` is a
derived view and not used for gating. Optionally, agents can provide a surface seed
under `<doc-pack>/inventory/surface.seed.json`.

Use `--lens-flake <ref>` to point at a different `binary_lens` flake if needed.
`bman init` requires `--binary` (or `enrich/bootstrap.json`) when
`binary.lens/manifest.json` is missing.

Fixture-backed scenarios can declare:

- `seed_dir`: path to a fixture tree copied into the per-run work dir
- `cwd`: relative working directory inside the seeded tree (defaults to `.`)

`seed_dir` is resolved relative to the doc-pack root (the parent of the
`scenarios/` directory), so doc packs can be moved without path fixes.

For a principled approach to expanding scenario coverage (options vs behaviors
vs doc claims) and curating `.SH EXAMPLES`, see `docs/COVERAGE.md`.
