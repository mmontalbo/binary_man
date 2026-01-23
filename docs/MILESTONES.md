# Milestones

This document tracks the static-first roadmap for generating man pages from
`binary_lens` packs. Dynamic execution is optional and used for scenario-backed
validation, coverage tracking, and (eventually) a structured “enrichment loop”
that supports iterative static + dynamic passes from portable doc packs.

Current focus: M9 — Doc-Pack Enrichment Loop (coverage-driven) (in progress).

## M9 — Doc-Pack Enrichment Loop (coverage-driven) (in progress)

Goal: Make enrichment an **iterable, coverage-driven workflow** over doc-pack
artifacts so agents can:
- run static analysis with pack-local lens templates (and add/adjust them)
- run dynamic scenarios with deterministic fixtures (and add/adjust them)
- stop with an explicit “done / blocked” decision recorded in the doc pack

Motivation:
- Some binaries require multiple static/dynamic passes to converge on accurate docs.
- “Coverage” is only useful if it drives next actions and produces a stable ledger.
- Doc packs should be the unit of work: portable, self-contained, and resumable.

Deliverables:
- Define an explicit doc-pack enrichment interface:
  - agent-edited inputs (pack-local): `queries/`, `scenarios/`, `fixtures/`
  - tool-owned state (pack-local): `enrich/state.json`, `enrich/history.jsonl`
  - coverage outputs: `coverage_ledger.json` + a final decision artifact
- Add an enrichment control file (doc-pack-local), e.g. `enrich/plan.yaml`, that declares:
  - which static passes to run (lens templates in `<doc-pack>/queries/`)
  - which scenario sets to run (catalogs in `<doc-pack>/scenarios/`)
  - coverage goals + stop conditions (acceptance/behavior/doc-claim)
  - publish policy for `.SH EXAMPLES`
- Add a coverage-first UX surface in `binary_man`:
  - `bman status --doc-pack <dir>` summarizes coverage + blockers + “next gaps”
  - `bman enrich --doc-pack <dir> --step <id>` runs one planned step (`static`, `dynamic`, `coverage`, `render`)
- Ensure doc-pack self-containment is enforced:
  - no hidden dependencies on repo-root `queries/` or other project paths
  - treat repo `queries/`, `scenarios/`, `fixtures/` as *seed templates only*
    (authoritative copies live in the doc pack once created)
- Write a structured “completion report” into the doc pack capturing:
  - goals met / not met
  - blockers (capability tags) and why they matter
  - smallest “next unlock” needed to advance coverage further

Acceptance criteria:
- Starting from a moved doc pack (arbitrary CWD), an agent can iterate:
  `status → edit doc-pack inputs → enrich` until goals are met or blocked,
  without modifying anything outside the doc pack.
- The workflow supports returning to static analysis after dynamic findings
  (e.g., adding/updating `<doc-pack>/queries/*.sql` and rerunning a static pass).

Out of scope:
- Fully interactive wizard/REPL UI.
- Automatic scenario synthesis (LM-driven).

## M8 — Broad Dynamic Validation + Coverage Ledger (ls) (in progress)

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
  - usage lens templates (`queries/<binary>_usage_evidence.sql`)
  - generated man page + `examples_report.json` + `meta.json`
- Make scenario fixture paths resolve relative to the doc pack (or the scenario file), not the process working directory.
- Add a `--lens-flake <ref>` override for pack generation and scenario runs.

Acceptance criteria:
- A doc pack containing `ls` (pack + scenarios + fixtures + queries) can be moved to an arbitrary directory and rerun successfully.
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
  `out/packs/ls/binary.lens/runs/` (e.g., `--help`, `--version`, invalid option),
  and `out/man/ls/ls.1` includes a corresponding `.SH EXAMPLES` section.
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
- Fresh `binary_lens` pack under `out/`.
- `out/man/ls/ls.1` rendered from the pack + lens output.
- Provenance artifacts (`help.txt`, `meta.json`).
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
