# bman doc-pack enrichment agent prompt (v2)

You are operating inside a **binary_man doc pack**. Assume your current working directory is the doc pack root.

## Goal
Make `bman status --doc-pack . --json` report `decision: "complete"` by satisfying the current requirements in `enrich/config.json`.
Keep `enrich/config.json.usage_lens_template` as a single relative path (default: `queries/usage_from_scenarios.sql`).

## Operating model
- `bman` runtime is binary-agnostic; do not assume command-specific behavior in Rust.
- Semantic interpretation is pack-owned in `enrich/semantics.json`, `queries/*.sql`,
  `scenarios/plan.json`, and optional `inventory/surface.overlays.json`.
- Keep semantic adjustments in those pack files; never request Rust semantic hardcoding.

## Small-LM Behavior Card (first screen, canonical)
Use this checklist when `enrich/config.json.verification_tier` is `"behavior"`.

1) Deterministic loop:
   - run `bman status --doc-pack . --json`
   - execute exactly one `.next_action`
   - run `bman apply --doc-pack .`
   - repeat until `decision: "complete"`
2) Action handling:
   - if `next_action.kind=="command"`: run `.command` exactly, then continue loop
   - if `next_action.kind=="edit"`: edit only `.path`, starting from `.content`
3) Edit strategy handling:
   - `edit_strategy=="replace_file"`: replace target file with `.content` (then minimal edits)
   - `edit_strategy=="merge_behavior_scenarios"`: apply merge contract below
4) Merge contract for `merge_behavior_scenarios`:
   - patch payload lives in `next_action.content` (not a full plan file)
   - merge optional `defaults` into `scenarios/plan.json.defaults`
   - upsert `upsert_scenarios[]` by `scenario.id`
   - never replace the full `scenarios/plan.json` with the patch payload object
5) Merge helper (preferred over manual JSON surgery):
   - file mode: `bman merge-behavior-edit --doc-pack . --status-json /tmp/status.json`
   - pipe mode: `bman status --doc-pack . --json | bman merge-behavior-edit --doc-pack . --from-stdin`
6) Required fields for each behavior variant scenario:
   - `coverage_tier: "behavior"`
   - `covers` (must include exact `surface_id`)
   - `baseline_scenario_id`
   - non-empty `assertions[]`
7) Assertion quick starters (at least one delta + one semantic predicate):
   - add behavior: `baseline_stdout_not_contains_seed_path` + `variant_stdout_contains_seed_path`
   - remove behavior: `baseline_stdout_contains_seed_path` + `variant_stdout_not_contains_seed_path`
   - change behavior: `variant_stdout_differs_from_baseline` + stable `expect.stdout_*`/`expect.stderr_*`
8) For short tokens (`.`, `..`, punctuation), prefer exact-line assertions:
   - `*_stdout_has_line` / `*_stdout_not_has_line` with `stdout_token`
9) Baseline auto-inclusion:
   - baseline scenario is auto-included when missing; do not create separate baseline-only loop steps
10) Surface overlays are optional refinement:
   - `inventory/surface.overlays.json` helps argv examples (`invocation.value_examples`)
   - overlays are not the first blocking step for behavior authoring
11) Keep output mode distraction-free:
   - default `status --json` is slim/actionability-first
   - use `status --json --full` only for triage (scenario/assertion diagnostics, reason codes, fix hints)

## Behavior quickstart (verification_tier: "behavior")
1) Set `enrich/config.json.verification_tier` to `"behavior"`.
2) Follow the **Small-LM Behavior Card** exactly.
3) Existence auto-verification finishes first; behavior scenarios are added after that.
4) Baseline auto-inclusion handles missing baseline setup; keep focus on variant assertions.
5) `inventory/surface.overlays.json` is optional refinement for argv examples, not a first-pass blocker.

## Hard rules
- Do NOT edit repository source code. Work only inside this doc pack.
- Only edit these files (unless `status --json` explicitly tells you otherwise):
  - `enrich/config.json`
  - `enrich/semantics.json`
  - `scenarios/plan.json`
  - `inventory/surface.overlays.json` (optional refinement)
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
- After every edit that “counts”, run: `bman apply --doc-pack .`.
- Never use `--force` unless explicitly instructed.

## What to trust (avoid wasted work)
- Treat `bman status --doc-pack . --json` as the source of truth for what to do next.
- `enrich/plan.out.json` is a snapshot; ignore it unless `status.plan.present=true` and `status.plan.stale=false`.
- After editing `scenarios/plan.json` or `enrich/semantics.json`, the plan is stale until you rerun `bman apply --doc-pack .`.
- Ignore artifacts that are not required by `enrich/config.json.requirements`, even if they exist:
  - If `"verification"` is not required, ignore `verification_ledger.json`.
  - If `"coverage"` / `"coverage_ledger"` is not required, ignore `coverage_ledger.json`.
- Default `bman status --doc-pack . --json` is actionability-first (slim payload).
- Use `bman status --doc-pack . --json --full` for rich triage/evidence diagnostics (including exact behavior failure scenario/assertion context when available).

## Scenario defaults
Prefer setting shared runner defaults once in `scenarios/plan.json` under `defaults`:
- `timeout_seconds: 3`
- `net_mode: "off"`
- `no_sandbox: false`
- `no_strace: true`
- `snippet_max_lines` / `snippet_max_bytes` (if you want tighter output)
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
     - For `edit_strategy=="merge_behavior_scenarios"`, use `bman merge-behavior-edit` (preferred) or the merge contract in **Small-LM Behavior Card**.
3) Run:
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
- Behavior tier is exhaustive for options: finish existence auto-verification, then author per-option behavior scenarios.
- Missing `invocation.value_examples` does not block first-pass behavior authoring; surface overlays remain optional refinement.
- Behavior tier uses a shared inline fixture under `scenarios/plan.json.defaults.seed` for baseline + variants.
- Auto verification is controlled by `scenarios/plan.json.verification.policy`:
  - `max_new_runs_per_apply`: batch size per apply.
- Exclusions (accepted tier only) are `scenarios/plan.json.verification.queue[]` entries with `intent: "exclude"` + non-empty `prereqs[]` (enum tags: `needs_arg_value`, `needs_seed_fs`, `needs_repo`, `needs_network`, `needs_interactive`, `needs_privilege`) and an optional reason.
- Exclusions are only for concrete blockers; if you do not want to verify something yet, leave it unqueued.
- Run `bman apply` repeatedly; `status --json` will recommend `apply` again until verification is met.
- Avoid per-flag scenarios for option **existence** verification; use the auto-verification policy instead.
- For **behavior** verification, author explicit `coverage_tier: "behavior"` scenarios with `baseline_scenario_id` + `assertions` + output expectations; `status --json` provides a single-target scaffold.
- For behavior edits with `edit_strategy: "merge_behavior_scenarios"`, use the merge contract in **Small-LM Behavior Card**.
- Status stubs are skeletons; fill `assertions` (`assertions[]` must be non-empty).
- Baseline auto-inclusion handles missing baseline setup; assertions belong on the variant behavior scenario.
- Do not exclude just because of `needs_seed_fs`; every scenario already runs with an empty fixture by default.
- Status triage summaries are compact (counts + previews); the canonical surface list is `inventory/surface.json`.
- When summarizing verification progress, report both accepted and behavior counts (even if behavior is not required).
- Auto-verify evidence is intentionally truncated to `snippet_max_*`; rerun a manual scenario if you need full output.

### Behavior reason-code to quick fix
- `no_scenario`: merge the status stub into `scenarios/plan.json`, then fill assertions.
- `assertion_failed`: add or fix assertions so they pass; ensure stable stdout/stderr predicates.
- `outputs_equal`: strengthen variant argv/fixture so output differs meaningfully from baseline, then rerun apply.
- `scenario_error`: fix argv/seed/expect so both baseline and variant runs pass before re-checking behavior.

### What counts as verifying an id
- Scenario-to-surface mapping is explicit: every entry in `covers` must be the exact `surface_id` you are verifying (no argv inference).
- Covers are ignored unless the scenario `argv` actually attempts the `surface_id` (token match rules are enforced by the verification SQL).
- Always include an explicit token for the `surface_id` in `argv`; do not rely on short-flag clustering.
- Use `coverage_tier` to declare intent (`"acceptance"` vs `"behavior"`); avoid `"rejection"` unless you are explicitly recording rejection evidence.
- Behavior scenarios only count when they prove a **delta vs baseline** and include explicit semantics; `variant_stdout_contains_seed_path` alone is never sufficient.
- Semantic predicate requirement (one is required):
  - At least one stdout/stderr expect predicate (`*_contains_*` or `*_regex_*`), or
  - a seed-grounded delta pair for the same `seed_path` (adds or removes).
- Delta proof must be either:
  - `variant_stdout_differs_from_baseline`, or
  - a matched pair for the same `seed_path`:
    - add: `baseline_stdout_not_contains_seed_path` + `variant_stdout_contains_seed_path`
    - remove: `baseline_stdout_contains_seed_path` + `variant_stdout_not_contains_seed_path`
- For seed-path assertions, `seed_path` must be a path in `seed.entries[].path` for both baseline and variant (fixture identity). If the program prints something else (basename, ".", etc.), use `stdout_token`.
- `stdout_token` is an optional verbatim stdout/stderr match token; when omitted, matching uses `seed_path`. `*_stdout_has_line`/`*_stdout_not_has_line` treat it as an exact line token (not substring).
- For short/ambiguous `stdout_token` values (<= 2 chars or mostly punctuation), prefer `*_stdout_has_line` / `*_stdout_not_has_line` to avoid substring matches.
- Example (directory listing): `seed_path: "work/file.txt", stdout_token: "file.txt"`.
- Assertion kinds:
  - **Stdout assertions**: `baseline_stdout_not_contains_seed_path`, `baseline_stdout_contains_seed_path`, `variant_stdout_contains_seed_path`, `variant_stdout_not_contains_seed_path`, `baseline_stdout_has_line`, `baseline_stdout_not_has_line`, `variant_stdout_has_line`, `variant_stdout_not_has_line`, `variant_stdout_differs_from_baseline` (diff-only is insufficient).
  - **File assertions** (variant-only, for commands that create/remove files/directories): `file_exists` (file was created), `file_missing` (file does NOT exist), `dir_exists` (directory was created), `dir_missing` (directory does NOT exist), `file_contains` (file contains pattern, requires `pattern` field). Use relative paths without `..`.
- Example (exact-line match for short tokens):
```json
{
  "argv": ["--option", "work"],
  "baseline_scenario_id": "baseline",
  "assertions": [
    { "kind": "baseline_stdout_not_has_line", "seed_path": "work/file.txt", "stdout_token": "X" },
    { "kind": "variant_stdout_has_line", "seed_path": "work/file.txt", "stdout_token": "X" }
  ]
}
```
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

### File assertions (for side-effect commands)
Use file assertions when a command's primary output is filesystem changes, not stdout:
- `touch`, `mkdir`, `cp`, `mv` — use `file_exists`, `dir_exists`
- `rm` — use `file_missing`
- `rmdir` — use `dir_missing`
- Commands that write to files — use `file_contains` with a `pattern` to match

Example (touch command):
```json
{
  "id": "touch-creates-file",
  "coverage_tier": "behavior",
  "covers": ["FILE"],
  "argv": ["newfile.txt"],
  "assertions": [
    { "kind": "file_exists", "path": "newfile.txt" }
  ]
}
```

Example (mkdir -p):
```json
{
  "id": "mkdir-p-creates-parents",
  "coverage_tier": "behavior",
  "covers": ["-p"],
  "argv": ["-p", "a/b/c"],
  "assertions": [
    { "kind": "dir_exists", "path": "a" },
    { "kind": "dir_exists", "path": "a/b" },
    { "kind": "dir_exists", "path": "a/b/c" }
  ]
}
```

File assertions are variant-only (no baseline comparison needed). The `path` must be relative and cannot contain `..`.

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

### Prerequisites and fixtures for verification

When options fail auto-verification, analyze the failure pattern to determine the right fix.

**Failure categories** (ordered by how to fix):
1. **Argument issues** — option needs a value or action; fix by adjusting scenario argv
2. **Environment issues** — needs filesystem context; fix by adding seed fixture
3. **Capability issues** — needs interactive/network/privilege; exclude with prereq

**Prereq workflow** (documentation-first):

1. **Analyze documentation**: Read help/man to understand what context an option requires:
   - Does it mention "project", "repository", "workspace"? → needs project structure
   - Does it say "opens editor", "interactive", "prompt"? → needs interactive, exclude
   - Does it require a config file, manifest, or input file? → needs filesystem seed

2. **Classify the failure**: Check `auto_verify_stderr` in decisions output:
   - Error mentions missing file/directory → `filesystem` prereq, create seed
   - Error says "requires value" or "no action" → not a prereq issue, fix argv
   - Command would wait for input → `interactive` prereq, exclude

3. **Define prereqs** in `enrich/semantics.json` (binary-specific):
```json
{
  "prereqs": {
    "project_root": {
      "category": "filesystem",
      "description": "Minimal project structure for this tool",
      "seed": { "entries": [ /* tool-specific files */ ] }
    },
    "interactive_mode": {
      "category": "interactive",
      "description": "Requires user interaction",
      "exclude_reason": "Cannot simulate interaction in sandbox"
    }
  }
}
```

4. **Annotate surface items** in `inventory/surface.overlays.json`:
```json
{
  "overlays": [
    { "id": "--edit", "prereqs": ["interactive_mode"] },
    { "id": "--config", "prereqs": ["project_root"] }
  ]
}
```

5. **Author scenarios** with appropriate seeds copied from prereq definitions.

**Prereq categories** (use for consistency):
- `filesystem` — needs files/directories to exist
- `config` — needs configuration values
- `state` — needs prior state (history, installed packages)
- `interactive` — requires user input (exclude)
- `network` — requires network access (exclude)
- `privilege` — requires elevated permissions (exclude)

**Decision tree**:
```
auto_verify failed
  ├─ stderr mentions missing file/directory?
  │    └─ Yes → create seed with required structure
  ├─ stderr says "requires value" or similar?
  │    └─ Yes → fix scenario argv, not a prereq issue
  ├─ option documented as interactive/editor?
  │    └─ Yes → exclude with interactive prereq
  └─ otherwise → analyze stderr, define appropriate prereq
```

**When to exclude vs fix**:
- **Exclude**: `interactive`, `network`, `privilege` — cannot be simulated in sandbox
- **Fix with seed**: `filesystem`, `config` — create the required structure
- **Fix with argv**: argument/value issues — add required tokens to scenario

## Finish
When complete:
- Print a short summary: decision, which requirements were met/unmet, how many scenarios you added, and 3–5 rough edges/improvements.
