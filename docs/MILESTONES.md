# Milestones

This document tracks the static-first roadmap for generating man pages from
`binary_lens` packs. Dynamic execution is optional and used for scenario-backed
validation, coverage tracking, and (eventually) a structured “enrichment loop”
that supports iterative static + dynamic passes from portable doc packs.

Current focus: M12 — Pack-Owned Semantics v1.

## M11.1 — Scenario Loop Rough-Edge Smoothing (done)

Goal: Keep “learn-by-executing scenarios” as the core agent job, but make the
loop cheaper and failures mechanically actionable (especially for small LMs).

Motivation:
- Scenario-based verification is the right direction, but can become slow and
  boilerplate-heavy as surface size grows.
- Small LMs should be able to progress mechanically from `status --json` without
  needing bespoke per-binary prompting or manual debugging.

Design constraints (non-negotiable for this milestone):
- JSON-only structured artifacts in the doc pack (JSONL permitted for history).
- Keep parsing semantics out of Rust: do not add help/CLI parsers; interpretation
  remains in pack-local SQL templates over scenario evidence.
- Evidence remains append-only; `apply` remains transactional.
- Safety defaults remain enforced (bounded timeouts/outputs + sandboxing).

Deliverables:
- Incremental scenario execution:
  - `apply` runs only new/changed/failed scenarios by default, keyed by a stable
    `scenario_digest` over the effective scenario + seed materialization inputs.
  - Provide explicit escape hatches: `--rerun-all` and `--rerun-failed`.
- Scenario plan `defaults` to reduce boilerplate:
  - Extend `scenarios/plan.json` to support a strict, schema-validated top-level
    `defaults` object (timeouts, net/sandbox/no_strace, snippet limits, cwd, env).
  - Evidence must record effective values so decisions remain reproducible.
- Runner environment normalization:
  - Apply safe, binary-agnostic env defaults (e.g. `LC_ALL=C`, `LANG=C`,
    `TERM=dumb`, `NO_COLOR=1`, `PAGER=cat`, `GIT_PAGER=cat`) unless overridden.
  - Record the final env used in scenario evidence.
- Status failure UX (deterministic next actions):
  - When a scenario fails, `status --json` includes a compact machine-readable
    failure summary and the evidence path(s), and recommends editing a single
    specific scenario ID.
- Two-tier verification (no scores):
  - Keep “accepted” (option/subcommand recognized) separate from “behavior”
    (seed + output/FS predicates), both evidence-linked in `verification_ledger.json`.
  - `enrich/config.json` can require either tier (default `accepted`).
- Pack-local agent prompt update:
  - Update `<doc-pack>/enrich/agent_prompt.md` to rely on incremental apply,
    plan defaults, and the accepted/behavior split; remove binary-specific argv hints.

Non-goals:
- Negative-testing framework or exhaustive combination testing.
- Auto-inference of option argument values or baked-in help parsing semantics.

Acceptance criteria:
- Editing one scenario and re-running `apply` re-executes only that scenario (and
  any required discovery), not the entire plan.
- A failing scenario yields a single deterministic next action plus evidence
  pointers sufficient for a small LM to proceed without extra narration.
- Haiku can reach “accepted” verification complete for `ls` using only
  `<doc-pack>/enrich/agent_prompt.md` + `status --json` loop, with stable iteration
  time due to incremental apply.

## M11 — Execution-Backed Verification v1 (done)

Goal: Move from “help-derived surface claims” to **execution-backed verification**
for surface IDs (starting with `ls`), using scenario evidence as the source of
truth. Keep decisions evidence-linked and avoid heuristic scoring.

Motivation:
- Help output is a claim, not evidence that an option/subcommand is accepted or
  behaves as documented.
- We want a simple LM to make progress mechanically by proposing scenarios (and
  inline seeds) without the tool baking in per-binary help/CLI parsing logic.

Design constraints (non-negotiable for this milestone):
- JSON-only structured artifacts in the doc pack (JSONL permitted for history).
- Scenarios remain the only execution concept; evidence is append-only.
- Keep parsing semantics out of Rust: interpretation lives in pack-local SQL
  templates over scenario evidence, not hardcoded parsers.
- Safety-first execution: bounded timeouts, bounded outputs, and sandboxing
  defaults remain enforced.

Deliverables:
- Scenario plan extensions (strict schema; schema bump):
  - Optional `scope` field on scenarios to support multi-command CLIs (e.g.
    `["commit"]` for `git commit`).
  - Inline `seed` specification on scenarios so agents can define deterministic
    filesystem fixtures without authoring `fixtures/**` trees by hand. The tool
    materializes seeds into an isolated per-run directory.
- Pack-local verification lens:
  - Install/standardize `queries/verification_from_scenarios.sql` that produces a
    deterministic, evidence-linked verification status per surface ID using:
    `<doc-pack>/inventory/scenarios/*.json`, `<doc-pack>/inventory/surface.json`,
    and `<doc-pack>/scenarios/plan.json`.
  - Verified status must come from scenario outcomes (not plan-only `covers`
    claims). No confidence scores.
- Evidence-linked verification ledger:
  - Emit `verification_ledger.json` that:
    - enumerates per-surface status (`verified`, `recognized`, `unknown`, `inconclusive`)
      and an explicit unverified list
    - links each decision to concrete evidence refs (`inventory/scenarios/*.json`,
      `inventory/surface.json`, `scenarios/plan.json`)
- Mechanical gating and deterministic next actions:
  - When verification is enabled as a requirement, `status --json` drives the
    smallest next action to reduce unverified IDs (edit/add a single scenario,
    then `validate → plan → apply`).

Acceptance criteria:
- `ls`: starting from help-derived surface, agents can mechanically add acceptance
  scenarios (with inline seeds where needed) until every surface ID is
  `verified` or explicitly `blocked` with evidence-linked reasons.
- `git`: scoped IDs are supported so verification can target `commit.--amend`
  style surface items without ambiguity (behavior verification may remain
  blocked until multi-step scenarios are supported).
- No scoring; all verification decisions and blockers cite concrete evidence.

## M10 — Scenario-Only Evidence + Coverage v1 (done)

Goal: Use a single concept — **scenarios** — for all execution-based evidence
(help/usage capture, surface discovery, examples, and optional coverage). Keep
decisions evidence-linked and avoid heuristic scoring.

Motivation:
- Reduce concepts and file formats a small LM must learn (scenarios only).
- Avoid baking help parsing semantics into the tool; keep parsing/editability in
  pack-local SQL templates.
- Make “coverage” mean “missing evidence items”, not a percent score.

Design constraints (non-negotiable for this milestone):
- JSON-only structured artifacts in the doc pack (JSONL permitted for history).
- Mechanical gating remains: edits don’t count until `validate` refreshes `lock.json`.
- Portability: everything runs from the doc pack, from any CWD.
- Keep it lean: do not add debug/provenance artifacts unless they’re needed as
  evidence inputs or hard requirements.

Deliverables:
- Scenario-only evidence:
  - Agent-edited: `<doc-pack>/scenarios/plan.json` (strict schema; includes help-style
    scenarios and behavior scenarios; includes optional `covers` claims).
  - Tool-written, append-only evidence: `<doc-pack>/inventory/scenarios/*.json`
    (normalized scenario results with bounded stdout/stderr).
- Lens-driven surface discovery from scenario evidence:
  - Install/standardize templates that read scenario evidence (not tool-parsed help):
    - `queries/usage_from_scenarios.sql`
    - `queries/subcommands_from_scenarios.sql`
    - `queries/options_from_scenarios.sql`
  - `inventory/surface.json` is derived from scenario evidence + optional seed and
    records discovery attempts and evidence refs; it blocks only when necessary
    (e.g., multi-command CLI detected but no subcommands extracted).
- Optional coverage gate (no scores):
  - Add an opt-in coverage requirement (not in `default_requirements`) that is met
    only when the uncovered surface ID list is empty (explicit list of missing items,
    evidence refs, and structured blockers/capability tags).
  - Coverage claims may be used as hints, but the tool must remain able to produce an
    uncovered list deterministically (no confidence scoring).

Acceptance criteria:
- Fresh `ls` and `git` packs can reach `decision=complete` for default requirements.
- When coverage is enabled, `status --json` drives the smallest next edit (scenario
  stubs or fixes) until uncovered is empty or blockers are explicit.
- Multi-command CLIs produce `.SH COMMANDS` or block with a single, concrete next action.
- Lock inputs include scenario plan + relevant lens templates so agents cannot
  “progress” by editing without re-validating.

Out of scope:
- Automatic scenario synthesis (LM-driven).
- A full interactive wizard/REPL UI.
- Perfect rollback of append-only evidence artifacts.

## M9 — Enrich v1 (JSON-only + Validate/Lock + Evidence-First Plan/Apply) (done)

Goal: Make doc-pack enrichment a **mechanically enforced** workflow with a
`init → validate → plan → apply` loop, where all structured artifacts are JSON
(JSONL permitted for history) and decisions are driven by evidence-linked
requirements (not heuristic scores).

Motivation:
- Agents can currently edit files and “progress” without a disciplined loop.
- Percent/goal heuristics are useful as derived convenience, but not as truth.
- JSON-only structured config/state reduces ambiguity and enables strict validation.
- Doc packs must remain portable: runnable from any CWD with no repo-root deps.

Design constraints (non-negotiable for this milestone):
- JSON-only structured artifacts in the doc pack (JSONL permitted for history).
- Clean break from `bman enrich`: bootstrap with `init`, then iterate with `validate/plan/apply/status`.
- Edits “don’t count” until `validate` produces a fresh `lock.json`.
- Decisions are evidence-linked: every unmet requirement and blocker points to concrete artifacts.

Artifacts (doc pack):
- Agent-edited inputs (locked by `validate`):
  - `<doc-pack>/enrich/config.json` (desired state; strict schema; invalid rejected)
  - `<doc-pack>/scenarios/plan.json` (scenario plan; strict schema; agent-editable)
  - optional: `<doc-pack>/inventory/surface.seed.json` (agent-provided surface seed; stable IDs)
  - `<doc-pack>/queries/`, `<doc-pack>/binary.lens/views/queries/`, `<doc-pack>/scenarios/`, `<doc-pack>/fixtures/`
- Tool-written evidence (append-only / evidence-first):
  - `<doc-pack>/inventory/scenarios/*.json` (mechanical scenario outputs, captured as structured evidence)
  - `<doc-pack>/binary.lens/runs/index.json`, `<doc-pack>/binary.lens/runs/**` (scenario run evidence index + artifacts)
- Tool-written canonical inventory:
  - `<doc-pack>/inventory/surface.json` (canonical surface inventory; stable IDs + evidence refs)
- Tool-written workflow/state:
  - `<doc-pack>/enrich/lock.json` (authoritative input snapshot: selected inputs + hashes/snapshot id)
  - `<doc-pack>/enrich/history.jsonl` (authoritative event log; JSONL)
  - `<doc-pack>/enrich/plan.out.json` (derived plan; must match `lock.json`)
  - `<doc-pack>/enrich/report.json` (derived report; evidence-linked)
  - temporary: `<doc-pack>/enrich/txns/<txn_id>/**` (staging + backups for atomic apply; cleaned on success)
- Derived outputs (not authoritative for decisions):
  - `<doc-pack>/man/**` (rendered man page artifacts)
  - `<doc-pack>/coverage_ledger.json` (derived convenience view; never a progress gate)

Commands (clean break):
- `bman init --doc-pack <dir> [--binary <bin>]` writes a schema-valid starter `<doc-pack>/enrich/config.json` (and generates the pack if missing; uses `enrich/bootstrap.json` if `--binary` is omitted).
- `bman validate --doc-pack <dir>` validates inputs and writes `<doc-pack>/enrich/lock.json`.
- `bman plan --doc-pack <dir>` writes `<doc-pack>/enrich/plan.out.json`.
- `bman apply --doc-pack <dir>` applies transactionally and writes `<doc-pack>/enrich/report.json`.
- `bman status --doc-pack <dir> [--json]` reports issues and the deterministic next action (stable machine-readable contract in `--json` mode).
- `bman enrich` is removed; use `init/validate/plan/apply/status`.

Mechanical gating:
- `plan/apply` refuse if `lock.json` is missing or stale (unless `--force`, recorded in `history.jsonl` and `report.json`).
- `status --json` always emits a machine-readable next action (even when lock is missing/stale).
- `apply` refuses if `plan.out.json` does not match the current `lock.json` (same snapshot/hashes).

Surface discovery (first-class, no “confidence”):
- Goal: produce a canonical `<doc-pack>/inventory/surface.json` with stable item IDs and evidence refs (even when runtime help is missing/stripped).
- Tool collects help/usage evidence mechanically into `<doc-pack>/inventory/scenarios/*.json` and run artifacts under `<doc-pack>/binary.lens/runs/**`.
- Do not treat derived man artifacts as canonical help evidence; only accept scenario/run outputs as help/usage evidence inputs.
- `surface.json` records the discovery attempts taken (as stable event codes) and the evidence artifacts each attempt produced/consumed.
- Every discovered item includes evidence refs (paths + hashes, and run IDs where applicable).
- Subcommand discovery is driven by a pack-local SQL template (`queries/subcommands_from_scenarios.sql`) so parsing remains editable.
- When discovery is underconstrained, emit explicit blocker codes plus an evidence-linked “next unlock” action.
- V1 simplification: treat options/commands/subcommands as `surface.json` item kinds (no separate `options.json`, `commands.json`, …).

Evidence > scores:
- Requirements are predicates over canonical inventory IDs (`inventory/surface.json`) and canonical evidence indices (`inventory/scenarios/*.json`, `binary.lens/runs/index.json`).
- `man/examples_report.json` and `coverage_ledger.json` may exist as derived views, but are never authoritative for gating decisions.
- Reports enumerate unmet requirements, missing evidence, and blockers as structured codes/tags with evidence refs.
- Metrics may be emitted only as derived summaries, never as authoritative decision inputs.

LLM UX helpers (tool-owned edits, not hand-crafted JSON):
- Provide scaffolding and formatting helpers so agents can follow `next_action` without manual multi-file JSON surgery.

Acceptance criteria:
- Starting from a moved doc pack (arbitrary CWD), an agent can iterate:
  `validate → plan → apply` until requirements are met or blocked, without modifying anything outside the doc pack.
- Starting from a doc pack with missing/stripped help output, the tool can still:
  - produce a surface inventory mechanically, or
  - fail with explicit blocker codes and an evidence-linked smallest “next unlock” action (scenario/fixture/manual seed).
- `status --json` always returns exactly one deterministic `next_action` that is either a single command to run or a single tool-owned edit to apply.
- `apply` is transactional: failures do not partially update state/output artifacts.
- `report.json` is evidence-linked (scenario IDs, run IDs, artifact paths) and records blockers as stable tags/codes.
- All structured config/state/report/lock/plan artifacts are JSON (JSONL permitted for history).

Out of scope:
- Fully interactive wizard/REPL UI.
- Automatic scenario synthesis (LM-driven).
- Full Terraform-style drift detection / predictive diffs over dynamic runs.
- Perfect rollback of append-only run artifacts (rollback operates on committed pointers/txns).

## M8 — Broad Dynamic Validation + Coverage Ledger (ls) (deferred; folded into M9)

Goal: Expand dynamic scenario execution so the generated `ls(1)` man page can be
backed by **real, sandboxed binary behavior** for as much of the option surface
as is practical, while explicitly tracking what remains blocked.

Motivation:
- `ls` has many options whose behavior is only meaningful with deterministic filesystem fixtures.
- Many outputs are inherently volatile (timestamps, uid/gid names, PTY-dependent behavior).

Deliverables:
- Maintain an `ls` doc pack that includes:
  - a scenario catalog with explicit coverage metadata (`coverage_tier`, `covers_options`, `coverage_ignore`)
  - deterministic fixture-backed behavior scenarios (`seed_dir` + `cwd`)
  - a coverage ledger that classifies option IDs as accepted/rejected/unknown and tracks behavior coverage separately
  - explicit “blockers” for behavior scenarios expressed as capability tags (timestamps, uid/gid mapping, PTY, etc.)
- Codify “listed-but-rejected” options (surface-area inventory vs runtime acceptance).
- Keep `.SH EXAMPLES` curation independent of raw coverage expansion (publish only high-value scenarios).

Acceptance criteria:
- For the current extracted option inventory, every option ID is classified as accepted/rejected/unknown, and unknowns are explained.
- Behavior coverage is non-trivial (fixture-backed examples exist) and remaining gaps are explicitly blocked with capability tags.

Out of scope:
- Making every option a published man page example.
- Capability unlock work beyond recording blockers (e.g., timestamps/ownership control, PTY capture).

## M7 — Portable Doc Packs (done)

Goal: Make per-binary documentation artifacts **portable and self-contained** so
scenario catalogs, fixtures, and usage lens templates live with the binary’s
documentation pack (not in the `binary_man` repo).

Deliverables:
- Define a doc-pack directory layout (per binary) that co-locates:
  - `binary.lens/` pack
  - scenario catalog(s)
  - fixture trees
- usage lens templates (`queries/*.sql` with `binary.lens/views/queries/*.sql` as fallback)
  - generated man page + `examples_report.json` + `meta.json`
- Make scenario fixture paths resolve relative to the doc pack (or the scenario file), not the process working directory.
- Add a `--lens-flake <ref>` override for pack generation and scenario runs.

Acceptance criteria:
- A doc pack containing `ls` (pack + scenarios + fixtures + pack-local queries) can be moved to an arbitrary directory and rerun successfully.
- No repo-root `scenarios/`, `fixtures/`, or `queries/` directories are required to reproduce scenario runs once the doc pack exists.

Out of scope:
- Packaging/distribution format (e.g., `.zip`) beyond a stable on-disk layout.

## M6 — Scenario-Backed EXAMPLES (done)

Goal: Populate the man page’s `EXAMPLES` section with **outputs from real runs**
to validate that documented invocations behave as described, using the runtime
scenario capture feature in `binary_lens` packs.

Deliverables:
- Scenario catalog (per binary) with explicit expectations:
  - argv, env overrides, timeout, and output excerpt policy
  - expected exit code and minimal stdout/stderr matchers (regex/substring)
- Runner that executes scenarios and appends them to an existing pack’s `runs/`
  overlay via `binary_lens run=1 <pack_root> ...` (no re-export).
- Validation report artifact (JSON) mapping scenario IDs → `runs/<run_id>/` refs
  + pass/fail status + observed exit code.
- Man page renderer emits `.SH EXAMPLES` from passing scenarios marked
  `"publish": true`:
  - show the exact command line as run
  - include a bounded stdout/stderr snippet and note non-zero exit status
- Provenance: extend `meta.json` schema to reference the runs index and the
  examples/validation report.
- Docs: document the workflow for (re)running scenarios and regenerating the man
  page.

Acceptance criteria (`ls` guinea pig):
- Running the examples workflow produces ≥3 captured runs in
  `<doc-pack>/binary.lens/runs/` (e.g., `--help`, `--version`, invalid option),
  and `<doc-pack>/man/ls.1` includes a corresponding `.SH EXAMPLES` section.
- Scenario results are reproducible under a controlled env (e.g., `LC_ALL=C`)
  and output is kept bounded via truncation rules.

Out of scope:
- Automatic scenario synthesis (LM-driven) from static analysis.
- Deep semantic assertions beyond exit status + lightweight output checks.
- Cross-platform sandbox parity; Linux-first is acceptable.

## M5 — Comprehensive `ls(1)` Man Page (done)

Goal: Generate a comprehensive, plausible `ls(1)` man page from a fresh
`binary_lens` pack, using deterministic rendering over lens output. Dynamic
validation is deferred to a later milestone (implemented for `EXAMPLES` in M6).

Deliverables:
- Fresh `binary_lens` pack under `<doc-pack>/binary.lens/`.
- `<doc-pack>/man/ls.1` rendered from the pack + lens output.
- Provenance artifact (`meta.json`).

Out of scope:
- Dynamic execution or sandbox validation.
- Scenario runners or inference loops.

## M4 — Provenance Bundle (done)

Goal: Make outputs auditable.

Deliverables:
- Store prompt, response, help text, and a metadata JSON pointing back to the pack.

Note: Prompt/response artifacts are deprecated in favor of the lens outputs in M5.

## M3 — LM Man Page Pipeline (done)

Goal: Produce a plausible, comprehensive man page from static evidence.

Deliverables:
- Assemble a prompt from pack metadata + extracted help text.
- Invoke the configured LM CLI (Claude default).
- Emit `ls.1` plus prompt/response provenance.

Note: LM synthesis is deprecated in favor of deterministic rendering in M5.

## M2 — Evidence Extraction (done)

Goal: Pull help/usage strings from static pack facts for LM context.

Deliverables:
- Query `facts/strings.parquet` via DuckDB to extract `ls` usage/help text.
- Preserve extracted help text as a first-class artifact.

Note: Raw-string extraction artifacts are deprecated in favor of the
lens-based evidence trail in M5.

## M1 — Pack Ingest (done)

Goal: Treat `binary_lens` packs as the canonical input artifact.

Deliverables:
- Accept a pack root (`binary.lens/`) or generate one via `nix run ../binary_lens#binary_lens`.
- Read pack manifest for binary identity and tool provenance.

## M0 — Static Reset (done)

Goal: Strip the project back to static analysis + LM-assisted documentation.

Deliverables:
- Remove sandboxed runner/scenario machinery.
- Keep only pack ingestion, evidence extraction, and man page generation.
