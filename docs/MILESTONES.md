# Milestones

This document tracks the static-first roadmap for generating man pages from
`binary_lens` packs. Dynamic execution is optional and used for scenario-backed
validation, coverage tracking, and (eventually) a structured “enrichment loop”
that supports iterative static + dynamic passes from portable doc packs.

Current focus: TBD (M9 complete; define next milestone).

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
  - `<doc-pack>/inventory/probes/plan.json` (probe plan; strict schema; agent-editable)
  - optional: `<doc-pack>/inventory/surface.seed.json` (agent-provided surface seed; stable IDs)
  - `<doc-pack>/queries/`, `<doc-pack>/binary.lens/views/queries/`, `<doc-pack>/scenarios/`, `<doc-pack>/fixtures/`
- Tool-written evidence (append-only / evidence-first):
  - `<doc-pack>/inventory/probes/*.json` (mechanical probe outputs, captured as structured evidence)
  - `<doc-pack>/binary.lens/runs/index.json`, `<doc-pack>/binary.lens/runs/**` (scenario run evidence index + artifacts)
- Tool-written canonical inventory:
  - `<doc-pack>/inventory/surface.json` (canonical surface inventory; stable IDs + evidence refs)
- Tool-written workflow/state:
  - `<doc-pack>/enrich/lock.json` (authoritative input snapshot: selected inputs + hashes/snapshot id)
  - `<doc-pack>/enrich/state.json` (authoritative pointer to last committed txn)
  - `<doc-pack>/enrich/history.jsonl` (authoritative event log; JSONL)
  - `<doc-pack>/enrich/plan.out.json` (derived plan; must match `lock.json`)
  - `<doc-pack>/enrich/report.json` (derived report; evidence-linked)
  - `<doc-pack>/enrich/txns/<txn_id>/**` (staging + committed outputs)
- Derived outputs (not authoritative for decisions):
  - `<doc-pack>/man/**` (rendered man page artifacts)
  - `<doc-pack>/coverage_ledger.json` (derived convenience view; never a progress gate)

Commands (clean break):
- `bman init --doc-pack <dir> [--binary <bin>]` writes a schema-valid starter `<doc-pack>/enrich/config.json` (and generates the pack if missing; uses `enrich/bootstrap.json` if `--binary` is omitted).
- `bman validate --doc-pack <dir>` validates inputs and writes `<doc-pack>/enrich/lock.json`.
- `bman plan --doc-pack <dir>` writes `<doc-pack>/enrich/plan.out.json`.
- `bman apply --doc-pack <dir>` applies transactionally and updates `<doc-pack>/enrich/state.json`.
- `bman status --doc-pack <dir> [--json]` reports issues and the deterministic next action (stable machine-readable contract in `--json` mode).
- `bman enrich` is removed; use `init/validate/plan/apply/status`.

Mechanical gating:
- `plan/apply` refuse if `lock.json` is missing or stale (unless `--force`, recorded in `history.jsonl` and `report.json`).
- `status --json` always emits a machine-readable next action (even when lock is missing/stale).
- `apply` refuses if `plan.out.json` does not match the current `lock.json` (same snapshot/hashes).

Surface discovery (first-class, no “confidence”):
- Goal: produce a canonical `<doc-pack>/inventory/surface.json` with stable item IDs and evidence refs (even when runtime help is missing/stripped).
- Tool collects help/usage evidence mechanically (bounded probe set and/or scenarios) into `<doc-pack>/inventory/probes/*.json` and run artifacts under `<doc-pack>/binary.lens/runs/**`.
- Do not treat derived man artifacts as canonical help evidence; only accept probe/run outputs as help/usage evidence inputs.
- `surface.json` records the discovery attempts taken (as stable event codes) and the evidence artifacts each attempt produced/consumed.
- Probe outputs are captured as `<doc-pack>/inventory/probes/*.json` and referenced from `surface.json` as evidence.
- Every discovered item includes evidence refs (paths + hashes, and run IDs where applicable).
- Subcommand discovery is driven by a pack-local SQL template (`queries/subcommands_from_probes.sql`) so parsing remains editable.
- When discovery is underconstrained, emit explicit blocker codes plus an evidence-linked “next unlock” action.
- V1 simplification: treat options/commands/subcommands as `surface.json` item kinds (no separate `options.json`, `commands.json`, …).

Evidence > scores:
- Requirements are predicates over canonical inventory IDs (`inventory/surface.json`) and canonical evidence indices (`inventory/probes/*.json`, `binary.lens/runs/index.json`).
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
  - fail with explicit blocker codes and an evidence-linked smallest “next unlock” action (probe/fixture/manual seed).
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
- Provenance artifacts (`usage_evidence.json`, `meta.json`).
- Evidence trail (`usage_evidence.json`, `usage_lens.template.sql`, `usage_lens.sql`).

Out of scope:
- Dynamic execution or sandbox validation.
- Scenario runners, probes, or inference loops.

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
