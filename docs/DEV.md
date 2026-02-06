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
cargo run --bin bman -- apply --doc-pack /tmp/ls-docpack
cargo run --bin bman -- status --doc-pack /tmp/ls-docpack
cargo run --bin bman -- status --doc-pack /tmp/ls-docpack --json
```

Status-first bootstrap (empty dir, no pack yet):

```
cargo run --bin bman -- status --doc-pack /tmp/empty --json
cargo run --bin bman -- init --doc-pack /tmp/empty --binary ls
cargo run --bin bman -- apply --doc-pack /tmp/empty
```

`enrich/config.json` declares the inputs and requirements to evaluate. `bman`
drives the loop with `apply` (auto-runs validate + plan as needed), and `status`
reports a decision of `complete`, `incomplete`, or `blocked` based on
evidence-linked requirements and blockers. `apply` writes `enrich/report.json`
with the latest evaluation.
Runtime policy is intentionally binary-agnostic: semantic meaning belongs in pack
artifacts (`enrich/semantics.json`, `queries/*.sql`, `scenarios/plan.json`,
`inventory/surface.overlays.json`), while Rust remains orchestration.
Use `validate`/`plan` directly only for debugging or intermediate inspection.
Usage extraction is configured by the single
`enrich/config.json.usage_lens_template` path (default:
`queries/usage_from_scenarios.sql`).

Scenarios are defined under `<doc-pack>/scenarios/plan.json`. Scenario runs are
appended to the pack under `<doc-pack>/binary.lens/runs/` and summarized in
`<doc-pack>/man/examples_report.json` when planned and when publishable examples
exist. Usage + discovery templates are installed under `<doc-pack>/queries/` and
used directly by the tool.
Enrichment control and state live under `<doc-pack>/enrich/`, including
`config.json`, `lock.json`, and `plan.out.json`.
Surface inventory lives under `<doc-pack>/inventory/surface.json` with scenario
evidence in `<doc-pack>/inventory/scenarios/*.json`; the inventory records raw
forms plus invocation shape for each item. `coverage_ledger.json` is a derived
view and not used for gating. `verification_ledger.json` is an execution-backed
verification view reused by `plan`/`status` when fresh. Optionally, agents can provide surface overlays under
`<doc-pack>/inventory/surface.overlays.json`, including invocation hints.

Use `--lens-flake <ref>` to point at a different `binary_lens` flake if needed.
`bman init` requires `--binary` when `binary.lens/manifest.json` is missing.

Fixture-backed scenarios can declare:

- `seed_dir`: path to a fixture tree copied into the per-run work dir
- `seed`: inline seed spec (entries with `path`, `kind`, and `contents`/`target`/`mode` as required by kind)
- `defaults.seed`: shared inline seed applied to scenarios that omit `seed` and `seed_dir` (use this to avoid repeating the same fixture across baseline/variant scenarios)
- `cwd`: relative working directory inside the seeded tree (defaults to `.`)

`seed_dir` is resolved relative to the doc-pack root (the parent of the
`scenarios/` directory), so doc packs can be moved without path fixes. Inline
`seed` entries are materialized into an isolated per-run directory.

Seed entry rules:
- `kind: "file"` defaults `contents` to `""` when omitted.
- `kind: "symlink"` requires `target`.
- `kind: "dir"` must not include `contents` or `target`.

Verification is enabled by default (opt-out by editing
`enrich/config.json.requirements`). Auto-verification is configured in
`scenarios/plan.json.verification.policy` with `kinds` (e.g. `"option"`,
`"subcommand"`) and `max_new_runs_per_apply`. Represent objective skips only as
`scenarios/plan.json.verification.queue[]` entries with `intent: "exclude"` +
non-empty `prereqs[]` (enum tags) and an optional reason.

Default `status --json` is slim/actionability-first; use `status --json --full`
only for triage diagnostics/evidence. Behavior assertion output normalization is
configured by `enrich/semantics.json.behavior_assertions`
(`strip_ansi`, `trim_whitespace`, `collapse_internal_whitespace`).

For canonical behavior-loop semantics, use
`prompts/enrich_agent_prompt.md` (**Small-LM Behavior Card**). That card is the
single source of truth for the small-LM behavior loop and merge contract.

For behavior merge patches, prefer the helper command over manual JSON merging:

```
bman status --doc-pack /tmp/ls-pack --json | \
  bman merge-behavior-edit --doc-pack /tmp/ls-pack --from-stdin
```

For a principled approach to expanding scenario coverage (options vs behaviors
vs doc claims) and curating `.SH EXAMPLES`, see `docs/COVERAGE.md`.

## Distraction-Free Mode

Use this when handing a doc pack to a small LM and minimizing context switching.

1) Follow `prompts/enrich_agent_prompt.md` (**Small-LM Behavior Card**) as canonical instructions.
2) Use slim status by default (`bman status --doc-pack <pack> --json`), switching to `--json --full` only for triage.
3) For behavior merge edits, prefer:
   - `bman merge-behavior-edit --doc-pack <pack> --status-json <status.json>`
   - or `bman status ... --json | bman merge-behavior-edit --doc-pack <pack> --from-stdin`
