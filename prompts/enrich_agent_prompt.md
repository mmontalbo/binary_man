# bman doc-pack enrichment agent prompt (v1)

You are operating inside a **binary_man doc pack**. Assume your current working directory is the doc pack root.

## Goal
Make `bman status --doc-pack . --json` report `decision: "complete"` by satisfying the current requirements in `enrich/config.json`.

## Hard rules
- Do NOT edit repository source code. Work only inside this doc pack.
- Only edit these files (unless `status --json` explicitly tells you otherwise):
  - `enrich/config.json`
  - `scenarios/plan.json`
  - `inventory/surface.seed.json` (optional; only if surface discovery is blocked)
- Do NOT edit tool outputs directly:
  - `inventory/surface.json`
  - `inventory/scenarios/*.json`
  - `man/*`
  - `coverage_ledger.json`
  - `verification_ledger.json`
  - `enrich/lock.json`
  - `enrich/plan.out.json`
  - `enrich/report.json`
- After every edit that “counts”, run the gated loop: `validate → plan → apply`.
- Never use `--force` unless explicitly instructed.

## Scenario defaults
Prefer setting shared runner defaults once in `scenarios/plan.json` under `defaults`:
- `timeout_seconds: 3`
- `net_mode: "off"`
- `no_sandbox: false`
- `no_strace: true`
- `snippet_max_lines` / `snippet_max_bytes` (if you want tighter output)
- `cwd`, `env`, `seed_dir` when common across scenarios
- Prefer **relative paths** only; avoid absolute paths.

Runner env normalization (applied unless you override in `defaults.env` or scenario `env`):
`LC_ALL=C`, `LANG=C`, `TERM=dumb`, `NO_COLOR=1`, `PAGER=cat`, `GIT_PAGER=cat`

## Mechanical loop (always follow this)
1) Run: `bman status --doc-pack . --json`
2) Read `.next_action`:
   - If `kind=="command"`: run the command exactly; then go back to step 1.
   - If `kind=="edit"`: edit the file at `.path`.
     - Start from `.content` (the tool-provided stub) and make the smallest change.
3) Run:
   - `bman validate --doc-pack .`
   - `bman plan --doc-pack .`
   - `bman apply --doc-pack .` (incremental; use `--rerun-all` or `--rerun-failed` when needed)
4) Go back to step 1 until complete.

## Verification requirement (if present)
If `enrich/config.json.requirements` includes `"verification"`:
- Check `enrich/config.json.verification_tier` (default: `"accepted"`).
- If tier is `"accepted"`: use `verification_ledger.json.unverified_ids` and add **acceptance** scenarios.
- If tier is `"behavior"`: use `verification_ledger.json.behavior_unverified_ids` and add **behavior** scenarios.

### What counts as verifying an id
For each added scenario where `coverage_ignore=false`, every id you list in `covers` must be **actually invoked** by that scenario’s argv:
- If a cover is `--opt`: argv must contain `--opt` OR `--opt=value` (and for value-taking options, prefer `--opt=value`).
- If a cover is `-x`: argv must contain `-x`, or a short-option cluster containing it (e.g. `-xyz` covers `-x`), or an attached form like `-Ipattern` / `-T4`.
- If a cover is scoped like `subcommand.--opt`:
  - Set `scope: ["subcommand"]`
  - Start argv with `["subcommand", ...]`
  - Also include `--opt` (or `--opt=value`) in argv.
- Pick required values/operands mechanically from evidence:
  - `inventory/scenarios/*.json` stdout/stderr
  - `inventory/surface.json` item descriptions
- Minimum expectation for existence/acceptance verification:
  - `expect.exit_code = 0`
  - Add stdout/stderr expectations only when they are clearly stable.

## Finish
When complete:
- Print a short summary: decision, which requirements were met/unmet, how many scenarios you added, and 3–5 rough edges/improvements.
