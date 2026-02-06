# binary_man

Static-first man page generator that consumes `binary_lens` context packs and
deterministically renders a comprehensive, plausible man page from usage
evidence. Optional scenario runs can be used to populate a real `.SH EXAMPLES`
section from captured stdout/stderr, proving documented invocations behave as
described.

Note: help-text extraction is used only for derived rendering; inventory and
gating rely on `inventory/surface.json` plus scenario evidence and run artifacts.

Design principles:
- Runtime is binary-agnostic: no per-command (`ls`/`git`/etc.) branching in Rust.
- Semantic interpretation is pack-owned in:
  - `enrich/semantics.json`
  - `queries/*.sql`
  - `scenarios/plan.json`
  - `inventory/surface.overlays.json`
- Rust orchestrates validation/planning/execution/status and keeps
  `status --json` + transactional `apply` deterministic.

## Usage

Bootstrap a doc pack (pack + templates + config), then run the enrichment loop:

```
bman init --doc-pack /tmp/ls-docpack --binary ls
bman apply --doc-pack /tmp/ls-docpack
bman status --doc-pack /tmp/ls-docpack
bman status --doc-pack /tmp/ls-docpack --json
```

Primary loop is `init -> apply -> status`. `apply` already runs validate + plan.

Status-first bootstrap (empty dir, no pack yet):

```
bman status --doc-pack /tmp/empty --json
bman init --doc-pack /tmp/empty --binary ls
bman apply --doc-pack /tmp/empty
```

Enrichment config lives in `<doc-pack>/enrich/config.json`; `bman apply`
auto-runs validate + plan, then executes transactionally. `bman status` reports
a decision of `complete`, `incomplete`, or `blocked` based on evidence-linked
requirements and blockers. Verification is enabled by default (opt-out by
removing `"verification"` from `enrich/config.json.requirements`).
Usage extraction is pack-owned via
`enrich/config.json.usage_lens_template` (single relative path).
Auto-verification is configured in `scenarios/plan.json.verification.policy`
with `kinds` (e.g. `["option"]` or `["option", "subcommand"]`) and a bounded
`max_new_runs_per_apply`. Follow the deterministic `status --json` next action
and rerun `apply` until verification is met; reserve `verification.queue` for
accepted-tier exclusions with prereq tags. Default `status --json` is slim and
actionability-first; use `status --json --full` only for triage diagnostics.
For canonical behavior-tier loop semantics (merge contract, required fields,
assertion starters, baseline auto-inclusion), see
`prompts/enrich_agent_prompt.md` (**Small-LM Behavior Card**).

Flags:
- `--doc-pack <dir>`: doc pack root for init/validate/plan/apply/status
- `--force`: command-specific override (`init`: overwrite config; `plan`/`status`: ignore missing/stale lock.json)
- `--refresh-pack`: regenerate the pack before apply using the pack manifest
- `--binary <bin>`: binary to analyze when generating a new pack (init only)
- `--lens-flake <ref>`: override the `binary_lens` flake ref (init/apply; default: `../binary_lens#binary_lens`)
- `--json`: emit machine-readable status output
- `--full`: with `status --json`, include rich diagnostics/evidence instead of the default slim payload
- `--verbose`: emit a verbose transcript of the workflow

Primary commands:
- `init --doc-pack <dir> [--binary <bin>]`: generate a pack (if missing) and write a starter `enrich/config.json`; `--binary` is required when creating a new pack
- `apply --doc-pack <dir>`: apply the plan transactionally (auto-runs validate/plan; writes `enrich/report.json`)
- `status --doc-pack <dir> [--json] [--full]`: summarize requirements and emit a deterministic next action

Advanced/debug commands:
- `validate --doc-pack <dir>`: validate inputs and write `enrich/lock.json` directly
- `plan --doc-pack <dir>`: evaluate requirements and write `enrich/plan.out.json` directly
- `merge-behavior-edit --doc-pack <dir> --status-json <file>`: apply `next_action` payload when `edit_strategy=="merge_behavior_scenarios"`
- `merge-behavior-edit --doc-pack <dir> --from-stdin`: stdin mode for `bman status --json | bman merge-behavior-edit ...`

Multi-command CLIs (example: `git`):

```
bman init --doc-pack /tmp/git-docpack --binary git
bman apply --doc-pack /tmp/git-docpack
bman status --doc-pack /tmp/git-docpack --json
```

If `status --json` returns a behavior edit with
`edit_strategy: "merge_behavior_scenarios"`, prefer the helper command instead
of manual JSON merging:

```
bman status --doc-pack /tmp/git-docpack --json | \
  bman merge-behavior-edit --doc-pack /tmp/git-docpack --from-stdin
```

## Outputs

Doc pack layout under `<doc-pack>/`:

- `<doc-pack>/binary.lens/` (pack)
- `<doc-pack>/scenarios/plan.json` (scenario plan)
- `<doc-pack>/fixtures/...` (fixture trees)
- `<doc-pack>/queries/` (project templates installed by init, including usage + subcommand extraction lenses)
- `<doc-pack>/enrich/config.json` (enrichment config)
- `<doc-pack>/enrich/agent_prompt.md` (tool-provided prompt for LM agents)
- `<doc-pack>/enrich/lock.json` (validated input snapshot)
- `<doc-pack>/enrich/plan.out.json` (planned actions + requirement eval)
- `<doc-pack>/enrich/report.json` (evidence-linked decision report)
- `<doc-pack>/enrich/history.jsonl` (append-only history)
- temporary: `<doc-pack>/enrich/txns/<txn_id>/...` (staging + backups for atomic apply; cleaned on success)
- optional: `<doc-pack>/inventory/surface.overlays.json` (agent-provided surface overlays for invocation hints)
- `<doc-pack>/inventory/surface.json` (canonical surface inventory; items are `option`/`command`/`subcommand` with forms + invocation shape)
- `<doc-pack>/inventory/scenarios/*.json` (scenario evidence)
- `<doc-pack>/man/<binary>.1` (man page)
- `<doc-pack>/man/examples_report.json` (derived scenario validation + run refs; only when scenarios are run)
- `<doc-pack>/coverage_ledger.json` (derived coverage ledger; refreshed by apply when surface/scenario/coverage actions run; never a gate)
- `<doc-pack>/verification_ledger.json` (derived verification ledger; refreshed by apply when surface/scenario actions run; reused by plan/status when fresh; never a gate)
- `<doc-pack>/man/meta.json` (provenance metadata)

## binary_lens integration

`bman init` generates a pack via `binary_lens` when `binary.lens/manifest.json` is missing.
Supply `--binary` for new packs.
You can still run `binary_lens` manually:

```
nix run <lens-flake> -- <binary> -o <doc-pack>
```

Scenario runs append runtime runs to the existing pack via:

```
nix run <lens-flake> -- run=1 <doc-pack>/binary.lens --help
```

## DuckDB extraction (lens-based)

Help/usage text is extracted via the lens templates installed under
`<doc-pack>/queries/`. The usage lens path is configured in
`enrich/config.json.usage_lens_template` (default shown below):

1. `queries/usage_from_scenarios.sql`

DuckDB is invoked via `nix run nixpkgs#duckdb --`. This help text is used for
rendering only; surface inventory is separate.

Surface discovery uses `queries/subcommands_from_scenarios.sql` and
`queries/options_from_scenarios.sql` to extract items from scenario stdout into
`inventory/surface.json`. If subcommands are missing for a multi-command CLI,
add help scenarios in `scenarios/plan.json` or adjust the query template, then
re-run the loop.

## Rendering

`bman` renders `<binary>.1` directly from the usage lens output and extracted
help evidence. No external LM is invoked.
