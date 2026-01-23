# Scenario Coverage Playbook

This document describes a **principled, repeatable process** for expanding
scenario-based coverage of a CLI tool’s documented behavior, and for turning
those scenarios into a trustworthy `.SH EXAMPLES` section in a generated man
page.

It is written as “agent instructions” you can paste into a coding agent prompt,
but it is also intended to be readable by humans.

## Quick Start (for agents)

Inputs to read:

- `scenarios/<binary>.json` (current scenarios)
- `out/man/<binary>/help.txt` (current “option surface” snapshot)
- `out/man/<binary>/examples_report.json` + `out/man/<binary>/<binary>.1` (what changed)

Main command:

```
cargo run --bin bman -- <binary> --run-scenarios
```

Outputs to inspect:

- `out/man/<binary>/examples_report.json` (pass/fail + excerpts)
- `out/man/<binary>/<binary>.1` (what got published into `.SH EXAMPLES`)
- `out/man/<binary>/coverage_ledger.json` (coverage status + blockers)

## Definition Of Done (coverage pass)

Don’t stop after smoke tests. A “coverage pass” is done only when:

- you state an explicit coverage target (options vs behaviors vs doc claims)
- you produce a small coverage ledger (total, covered, uncovered + reasons)
- you either reach the target, or explain why specific items are out-of-scope

## Coverage Model (avoid combinatorics)

Treat “coverage” as a few explicit tiers instead of “test every flag combo”:

1) **Option acceptance coverage**
   - Proves an option is accepted (or rejected) with expected exit status.
   - Cheap + broad; good for enumerating the surface area.
   - Use `coverage_tier: "rejection"` when an option appears in help but is
     expected to be rejected (e.g., `--coreutils-prog`).

2) **Behavior coverage**
   - Proves an option changes behavior in an observable way.
   - Requires deterministic fixtures (filesystem state, timestamps, locale).

3) **Doc-claim coverage**
   - Proves specific claims made (or implied) by the man page actually hold.
   - Often maps 1:1 to “real examples we want to publish”.

Only tier (3) should normally be rendered into the man page; tiers (1–2) are
mostly for confidence + tracking.

## Project Artifacts

- Scenario catalog: `scenarios/<binary>.json` (or `<doc-pack>/scenarios/<binary>.json`)
- Fixtures: `fixtures/...` (or `<doc-pack>/fixtures/...`)
- Usage lens templates: `queries/<binary>_usage_evidence.sql` (or `<doc-pack>/queries/<binary>_usage_evidence.sql`)
- Runs + evidence: `out/packs/<binary>/binary.lens/runs/` (or `<doc-pack>/binary.lens/runs/`)
- Validation report: `out/man/<binary>/examples_report.json` (or `<doc-pack>/man/examples_report.json`)
- Rendered man page: `out/man/<binary>/<binary>.1` (or `<doc-pack>/man/<binary>.1`)
- Coverage ledger: `out/man/<binary>/coverage_ledger.json` (or `<doc-pack>/coverage_ledger.json`)

## Current Behavior (important)

- `binary_man` renders `.SH EXAMPLES` from **passing scenarios with
  `"publish": true`** in the most recent `examples_report.json`.
  - Use `"publish": false` for “acceptance-only” or noisy scenarios so you can
    track coverage without bloating the man page.

## Workflow (agent)

### 1) Establish the option inventory

- Use the pack-derived help text (what `binary_man` already extracted) as the
  canonical option list for the current binary.
- Normalize into **option IDs** (group aliases together, e.g. `-a/--all`).
- Tag each option ID with:
  - `tier`: `common` | `advanced` | `edge`
  - `stability`: `stable-output` | `volatile-output` | `platform-dependent`
  - `fixture_needed`: `none` | `basic-fs` | `rich-fs` (or similar)

### 2) Decide what “good coverage” means for this milestone

Pick targets that you can actually keep deterministic:

- Tier 1 target: `% of common option IDs with acceptance scenarios`
- Tier 2 target: `N` high-value behaviors validated with fixtures
- Man page target: `M` published examples (short, readable, representative)

Default heuristic (when not specified):

- If the option surface is “small” (≈100 option IDs or fewer): aim for **100%
  option acceptance coverage** (one scenario per option ID, `publish:false`).
- Otherwise: cover all `common` options and explicitly list what you skipped.

### 3) Add scenarios intentionally

Design scenarios as small, named “contracts”:

- **Smoke/identity**: `--help`, `--version`
- **Failure modes**: invalid option, missing argument, missing file/dir
- **High-value behaviors** (fixture-backed): formatting, sorting, recursion, etc.

Keep scenario outputs bounded (snippet limits) and expectations lightweight but
meaningful (exit status + a few stable substrings/regexes).

### 4) Make behavior deterministic (fixtures)

Behavior scenarios should *not* depend on host state.

If output is volatile, first try to stabilize it with **inputs/env** before
reaching for brittle matchers:

- force C locale (`LC_ALL=C`, `LANG=C`)
- fix width (`COLUMNS=80`)
- disable color (e.g., `--color=never` or equivalent env)
- avoid timestamps/inodes unless you control them

If the behavior depends on filesystem contents, define a fixture spec:

- file/dir names (include tricky cases: spaces, leading dot, unicode if needed)
- permissions/ownership (if observable and stable)
- timestamps (set them explicitly)
- sizes (write exact bytes)
- symlinks (both valid + broken if relevant)

Scenario fixtures can be seeded declaratively:

- `seed_dir`: path to a fixture tree copied into the per-run work dir
- `cwd`: relative working directory inside the seeded tree (defaults to `.`)

`seed_dir` is resolved relative to the doc-pack root (the parent of
`scenarios/`), so doc packs remain relocatable.

Prefer relative paths in argv (e.g., `.` or `subdir/file`) to avoid absolute
path leakage in captured output.

If the harness can’t currently seed fixtures, record that as a “friction item”
and either (a) avoid that behavior for now, or (b) propose the minimal harness
feature needed to unlock it.

### 5) Run + validate + curate

- Run scenarios and regenerate outputs.
- Inspect `examples_report.json`:
  - Do failures indicate a real doc bug, a flaky expectation, or nondeterminism?
  - Are snippets short and free of host-specific leakage (paths, usernames)?
- Curate which scenarios become published man page examples:
  - Prefer scenarios that demonstrate doc claims and read well.
  - Avoid redundant examples even if they increase “coverage”.

### 6) Track coverage explicitly (don’t guess)

Maintain a “coverage ledger” in one of these ways:

- **Preferred (simple):** add *conventions* inside each scenario object:
  - `intent`: one sentence of what this proves
  - `covers_options`: list of option IDs covered by this scenario (consumed)
  - `covers_behaviors`: list of behavior IDs demonstrated
  - `covers_doc_claims`: list of claim IDs (freeform, but stable strings)

`binary_man` emits a `coverage_ledger.json` by combining help text + scenarios.
It uses `coverage_tier` (`acceptance`, `behavior`, or `rejection`) and
`covers_options` when present, and falls back to argv parsing otherwise. Use
`coverage_ignore: true` for scenarios that should not affect coverage (e.g.,
negative tests or non-option probes), while still allowing them to appear in
`examples_report.json` and the man page.

Use the catalog-level `coverage.blocked` list to record options that are
blocked for deterministic *behavior* validation. Blocked entries attach a
reason to the ledger but do not prevent acceptance or rejection coverage.

If you add new metadata keys, ensure the scenario loader tolerates them (unknown
fields must not break parsing).

Iterate: if your ledger still has uncovered items, add scenarios and rerun until
you hit your stated target (or mark items out-of-scope with reasons).

## Stop & Ask (don’t thrash)

Stop and ask for feedback (or log it as “friction”) if:

- you can’t make an example deterministic without a fixture mechanism
- the tool’s output is inherently platform-dependent in this environment
- the expectation you need would require full-output matching
- “coverage targets” are ambiguous (options vs behaviors vs doc claims)

## Coverage Tracking Templates

### Option inventory table (example)

Keep a table (in the PR description or a companion doc) like:

| option_id | aliases | tier | acceptance_scenarios | behavior_scenarios | notes |
|---|---|---:|---|---|---|
| all | `-a`, `--all` | common | `show-hidden` | `show-hidden` | needs fixture |

### Behavior inventory table (example)

| behavior_id | observable signal | fixture | scenarios | notes |
|---|---|---|---|---|
| hidden-files | dotfile appears | basic-fs | `show-hidden` | stable |

## Man Page Example Quality Bar

Published examples should:

- be short (ideally 5–15 output lines)
- avoid volatile data (timestamps unless controlled, inode numbers, locale)
- avoid host leakage (absolute store paths, usernames, $PWD)
- show exactly the command line run, plus the key output signal

If you can’t make an example deterministic, don’t publish it; keep it as a
non-rendered behavior scenario (or defer it).

## Feedback Outlet (“Friction Log”)

At the end of a coverage-expansion pass, write a short friction log (PR body or
companion doc) so we can improve the workflow/tools:

- **What blocked new scenarios?**
  - missing fixture seeding, output volatility, unclear doc claim, runner limits
- **What made expectations hard to write?**
  - need regex vs substring, unstable stderr wording, env sensitivity
- **What should we change in the harness next?**
  - minimal feature that unlocks deterministic behavior coverage
- **What should we *not* attempt?**
  - options/behaviors that are too platform-specific or too noisy to publish

Template:

- Coverage goal for this pass:
- What new scenarios were added (and why):
- What became a published example (and why):
- What failed or was flaky (and why):
- Harness/tooling gaps:
- Follow-up tasks (smallest useful next step):

## Kickoff Prompt Template (copy/paste)

Use this as a starting point for a coding agent:

> Goal: expand scenario coverage for `<binary>` and improve `.SH EXAMPLES` using
> real captured runs, while keeping examples deterministic and auditable.
>
> Read `docs/COVERAGE.md` and follow its workflow.
>
> Deliverables:
> - State an explicit coverage target up front (options vs behaviors vs doc
>   claims). If not specified, default to comprehensive option acceptance
>   coverage when feasible.
> - Update `scenarios/<binary>.json` with new scenarios:
>   - acceptance scenarios for uncovered option IDs (prefer `publish:false`)
>   - at least one fixture-backed behavior scenario if feasible
> - Run scenarios via `cargo run --bin bman -- <binary> --run-scenarios` and
>   ensure `out/man/<binary>/examples_report.json` reflects the new coverage.
> - Curate `.SH EXAMPLES` so it stays readable (only publish high-value,
>   deterministic scenarios).
> - Provide a short “coverage ledger” summary and a “friction log” describing
>   what was hard and what harness improvements would help.
