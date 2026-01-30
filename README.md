# binary_man

Static-first man page generator that consumes `binary_lens` context packs and
deterministically renders a comprehensive, plausible man page from usage
evidence. Optional scenario runs can be used to populate a real `.SH EXAMPLES`
section from captured stdout/stderr, proving documented invocations behave as
described.

Note: help-text extraction is used only for derived rendering; inventory and
gating rely on `inventory/surface.json` plus scenario evidence and run artifacts.

## Usage

Bootstrap a doc pack (pack + templates + config), then run the enrichment loop:

```
bman init --doc-pack /tmp/ls-docpack --binary ls
bman validate --doc-pack /tmp/ls-docpack
bman plan --doc-pack /tmp/ls-docpack
bman apply --doc-pack /tmp/ls-docpack
bman status --doc-pack /tmp/ls-docpack
bman status --doc-pack /tmp/ls-docpack --json
```

Status-first bootstrap (empty dir, no pack yet):

```
bman status --doc-pack /tmp/empty --json
# edit enrich/bootstrap.json (set binary)
bman init --doc-pack /tmp/empty
bman validate --doc-pack /tmp/empty
bman plan --doc-pack /tmp/empty
bman apply --doc-pack /tmp/empty
```

Enrichment config lives in `<doc-pack>/enrich/config.json`; `bman validate`
writes a lock snapshot, `bman plan` writes `plan.out.json`, and `bman apply`
executes transactionally. `bman status` reports a decision of `complete`,
`incomplete`, or `blocked` based on evidence-linked requirements and blockers.
Verification is enabled by default (opt-out by removing `"verification"` from
`enrich/config.json.requirements`). Option existence auto-verification is
configured in `scenarios/plan.json.verification.policy` (mode
`"verify_all_options"` with a bounded `max_new_runs_per_apply`). Follow the
deterministic `status --json` next action and rerun `apply` until verification
is met; use `verification.queue` only for manual scenarios when needed.

Flags:
- `--doc-pack <dir>`: doc pack root for init/validate/plan/apply/status
- `--force`: ignore missing/stale lock.json (recorded in report/plan)
- `--refresh-pack`: regenerate the pack before apply using the pack manifest
- `--binary <bin>`: binary to analyze when bootstrapping a new pack (init only; or set `enrich/bootstrap.json`)
- `--lens-flake <ref>`: override the `binary_lens` flake ref (init/apply; default: `../binary_lens#binary_lens`)
- `--json`: emit machine-readable status output
- `--verbose`: emit a verbose transcript of the workflow

Commands:
- `init --doc-pack <dir> [--binary <bin>]`: generate a pack (if missing) and write a starter `enrich/config.json` (reads `enrich/bootstrap.json` when `--binary` is omitted)
- `validate --doc-pack <dir>`: validate inputs and write `enrich/lock.json`
- `plan --doc-pack <dir>`: evaluate requirements and write `enrich/plan.out.json`
- `apply --doc-pack <dir>`: apply the plan transactionally (writes `enrich/report.json`)
- `status --doc-pack <dir> [--json]`: summarize requirements and emit a deterministic next action

Multi-command CLIs (example: `git`):

```
bman init --doc-pack /tmp/git-docpack --binary git
bman validate --doc-pack /tmp/git-docpack
bman plan --doc-pack /tmp/git-docpack
bman apply --doc-pack /tmp/git-docpack
bman status --doc-pack /tmp/git-docpack --json
```

If `status --json` returns a `next_action` edit for `scenarios/plan.json`,
apply it (typically adds help scenarios), then rerun validate/plan/apply.

## Outputs

Doc pack layout under `<doc-pack>/`:

- `<doc-pack>/binary.lens/` (pack)
- `<doc-pack>/scenarios/plan.json` (scenario plan)
- `<doc-pack>/fixtures/...` (fixture trees)
- `<doc-pack>/queries/` (project templates installed by init, including usage + subcommand extraction lenses)
- `<doc-pack>/enrich/config.json` (enrichment config)
- `<doc-pack>/enrich/agent_prompt.md` (tool-provided prompt for LM agents)
- `<doc-pack>/enrich/bootstrap.json` (optional bootstrap seed; used when pack is missing)
- `<doc-pack>/enrich/lock.json` (validated input snapshot)
- `<doc-pack>/enrich/plan.out.json` (planned actions + requirement eval)
- `<doc-pack>/enrich/report.json` (evidence-linked decision report)
- `<doc-pack>/enrich/history.jsonl` (append-only history)
- temporary: `<doc-pack>/enrich/txns/<txn_id>/...` (staging + backups for atomic apply; cleaned on success)
- optional: `<doc-pack>/inventory/surface.seed.json` (agent-provided surface seed)
- `<doc-pack>/inventory/surface.json` (canonical surface inventory; items are `option`/`command`/`subcommand`)
- `<doc-pack>/inventory/scenarios/*.json` (scenario evidence)
- `<doc-pack>/man/<binary>.1` (man page)
- `<doc-pack>/man/examples_report.json` (derived scenario validation + run refs; only when scenarios are run)
- `<doc-pack>/coverage_ledger.json` (derived coverage ledger; updated on apply; never a gate)
- `<doc-pack>/verification_ledger.json` (derived verification ledger; updated on apply; never a gate)
- `<doc-pack>/man/meta.json` (provenance metadata)

## binary_lens integration

`bman init` generates a pack via `binary_lens` when `binary.lens/manifest.json` is missing.
Supply `--binary` or create `enrich/bootstrap.json` with a `binary` value.
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
`<doc-pack>/queries/`. `bman` uses the scenario-based usage lens by default:

1. `queries/usage_from_scenarios.sql`

DuckDB is invoked via `nix run nixpkgs#duckdb --`. This help text is used for
rendering only; surface inventory is separate.

Surface discovery uses `queries/subcommands_from_scenarios.sql` and
`queries/options_from_scenarios.sql` to extract items from scenario stdout into
`inventory/surface.json`. If subcommands are missing for a multi-command CLI,
add help scenarios in `scenarios/plan.json` or adjust the query template, then
re-run the loop.

## Rendering

`bman` renders `ls.1` directly from the usage lens output and the extracted
help text. No external LM is invoked.
