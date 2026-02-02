# bman doc-pack enrichment agent prompt (v2)

You are operating inside a **binary_man doc pack**. Assume your current working directory is the doc pack root.

## Goal
Make `bman status --doc-pack . --json` report `decision: "complete"` by satisfying the current requirements in `enrich/config.json`.

## Hard rules
- Do NOT edit repository source code. Work only inside this doc pack.
- Only edit these files (unless `status --json` explicitly tells you otherwise):
  - `enrich/config.json`
  - `enrich/semantics.json`
  - `scenarios/plan.json`
  - `inventory/surface.seed.json` (optional; only if surface discovery is blocked)
- Only edit `queries/**` when `status --json` explicitly recommends it.
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

## What to trust (avoid wasted work)
- Treat `bman status --doc-pack . --json` as the source of truth for what to do next.
- `enrich/plan.out.json` is a snapshot; ignore it unless `status.plan.present=true` and `status.plan.stale=false`.
- After editing `scenarios/plan.json` or `enrich/semantics.json`, the plan is stale until you rerun `validate → plan → apply`.
- Ignore artifacts that are not required by `enrich/config.json.requirements`, even if they exist:
  - If `"verification"` is not required, ignore `verification_ledger.json`.
  - If `"coverage"` / `"coverage_ledger"` is not required, ignore `coverage_ledger.json`.
- Use `bman status --doc-pack . --json --full` only for human debugging (it expands full triage lists).

## Scenario defaults
Prefer setting shared runner defaults once in `scenarios/plan.json` under `defaults`:
- `timeout_seconds: 3`
- `net_mode: "off"`
- `no_sandbox: false`
- `no_strace: true`
- `snippet_max_lines` / `snippet_max_bytes` (if you want tighter output)
- `cwd`, `env`, `seed_dir` when common across scenarios
- `seed` when you want a shared inline fixture for many scenarios:
  - Put the seed entries under `defaults.seed`.
  - Omit per-scenario `seed` so baseline + variants inherit the same fixture.
- Prefer **relative paths** only; avoid absolute paths.

Default runner env lives in `scenarios/plan.json.default_env` (seeded by `bman init`):
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

## Man rendering + semantics (if man is unmet or low quality)
- Check `bman status --doc-pack . --json` for `man_warnings`:
  - If warnings mention missing usage text or synopsis, fix `scenarios/plan.json` help scenarios or `enrich/semantics.json` (do not edit generated outputs).
- Read `man/meta.json` (do not edit it):
  - `.usage_lens_source_path` shows which evidence source produced the help text used for rendering.
  - `.render_summary.semantics_unmet` lists which extractions are missing according to `enrich/semantics.json`.
- Read the evidence you are interpreting:
  - `inventory/scenarios/*.json` (help evidence lives here; especially help scenarios) for stdout/stderr.
- Help scenarios use the reserved `help--` id prefix; they are the only inputs for usage extraction and surface discovery, and verification scenarios never drive usage or surface growth.
- There is no usage-lens fallback; if usage is missing, update help scenarios or semantics and rerun the loop.
- `lens_summary` showing `options_from_scenarios.sql` as empty is normal for command-focused tools (e.g., git); it is not a failure if surface is met and decision is complete.
- `man/examples_report.json` is only present when there are publishable examples; its absence is normal in fresh packs.
- Fix by editing `enrich/semantics.json` (pack-owned semantics):
  - Prefer small changes: add/adjust a single regex/prefix rule, re-run the gated loop, then re-check status.
  - Do not add Rust parsing logic. Do not edit `queries/**` unless status explicitly recommends it.

## Verification requirement (if present)
If `enrich/config.json.requirements` includes `"verification"` (default for new packs):
- Verification is opt-out: remove `"verification"` from `requirements` to disable it.
- Check `enrich/config.json.verification_tier` (default: `"accepted"`).
- Accepted tier = existence/recognition checks (help/flag accepted); behavior tier = functional behavior checks and is only required when configured.
- When `verification_tier` is `"behavior"`, verification is **exhaustive within the model**: every in-scope `surface_id` must either be behavior-verified by passing scenarios with seed-grounded assertions, or be explicitly excluded with an objective reason/prereqs.
- Auto verification is controlled by `scenarios/plan.json.verification.policy`:
  - `kinds`: ordered list of auto targets (`"option"`, `"subcommand"`).
  - `max_new_runs_per_apply`: batch size per apply.
- Exclusions are controlled by `scenarios/plan.json.verification.queue[]` entries with `intent: "exclude"` + `prereqs[]` + `reason`.
- Run `validate → plan → apply` repeatedly; `status --json` will recommend `apply` again until verification is met.
- Avoid per-flag scenarios for option **existence** verification; use the auto-verification policy instead.
- For **behavior** verification, author explicit `coverage_tier: "behavior"` scenarios with `baseline_scenario_id` + `assertions` + output expectations; `status --json` will drive this one target at a time.
- Define a baseline scenario once (default id `baseline`) and reference it from each behavior scenario.
- Do not exclude just because of `needs_seed_fs`; every scenario already runs with an empty fixture by default.
- Status triage summaries are compact (counts + previews); the canonical surface list is `inventory/surface.json`.
- When summarizing verification progress, report both accepted and behavior counts (even if behavior is not required).
- Auto-verify evidence is intentionally truncated to `snippet_max_*`; rerun a manual scenario if you need full output.

### What counts as verifying an id
- Scenario-to-surface mapping is explicit: every entry in `covers` must be the exact `surface_id` you are verifying (no argv inference).
- Covers are ignored unless the scenario `argv` actually attempts the `surface_id` (token match rules are enforced by the verification SQL).
- Always include an explicit token for the `surface_id` in `argv`; do not rely on short-flag clustering.
- Use `coverage_tier` to declare intent (`"acceptance"` vs `"behavior"`); avoid `"rejection"` unless you are explicitly recording rejection evidence.
- Behavior scenarios only count when they prove a **delta vs baseline** and include explicit semantics; `variant_stdout_contains_seed_path` alone is never sufficient.
- Semantic predicate requirement (one is required):
  - At least one stdout/stderr expect predicate (`*_contains_*` or `*_regex_*`), or
  - a seed-grounded delta pair for the same `path` (adds or removes).
- Delta proof must be either:
  - `variant_stdout_differs_from_baseline`, or
  - a matched pair for the same `path`:
    - add: `baseline_stdout_not_contains_seed_path` + `variant_stdout_contains_seed_path`
    - remove: `baseline_stdout_contains_seed_path` + `variant_stdout_not_contains_seed_path`
- For seed-path assertions, the asserted `path` must be present in both the baseline and variant `seed.entries`, and the baseline/variant seeds should describe the same fixture.
- Assertion kinds (v1): `baseline_stdout_not_contains_seed_path`, `baseline_stdout_contains_seed_path`, `variant_stdout_contains_seed_path`, `variant_stdout_not_contains_seed_path`, `variant_stdout_differs_from_baseline` (diff-only is insufficient).
- For option existence, prefer `argv: ["<surface_id>"]` with `covers: ["<surface_id>"]`; do not force `expect.exit_code=0`.
- For command/subcommand existence, prefer `argv: ["<surface_id>", "--help"]` before adding prereqs or excluding.
- Classification is driven by `enrich/semantics.json.verification` rules (accepted vs rejected vs inconclusive); accepted existence can include missing-arg errors when semantics allow it.
- Auto verification argv is built from pack semantics:
  - Options: `verification.option_existence_argv_prefix` + `<surface_id>` + `option_existence_argv_suffix`.
  - Subcommands: `verification.subcommand_existence_argv_prefix` + `<surface_id>` + `subcommand_existence_argv_suffix`.
- Add stdout/stderr expectations only when they are clearly stable.

### Behavior expectation patterns
- Prefer line-anchored regex with stable tokens (e.g., `(?m)^NAME\\b`); avoid column spacing or layout-sensitive matches.
- Use seed-grounded delta pairs for explicit adds/removes semantics (see the delta pair rules above).

### Inline seed example (for behavior assertions)
Notes:
- File entries may omit `contents` (defaults to `""`).
- Symlink entries require `target`.
```json
{
  "entries": [
    { "path": "work", "kind": "dir" },
    { "path": "work/keep.txt", "kind": "file", "contents": "" }
  ]
}
```

## Finish
When complete:
- Print a short summary: decision, which requirements were met/unmet, how many scenarios you added, and 3–5 rough edges/improvements.
