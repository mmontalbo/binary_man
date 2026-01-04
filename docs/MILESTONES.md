# Milestones

This document records the current milestone plan and status. It is the canonical
sequence for the project.

Current focus: M5 — Fast Binary-Only Surface Extraction (T0 + T1) (in progress).

## M0 — Scaffold & Invariants (done)

Goal: Make the project reproducible and lock epistemic rules before any validation logic.

What landed:
- Nix flake + direnv dev environment (Linux-safe tooling).
- Rust CLI skeleton (claims, validate, regenerate).
- Schemas with explicit provenance and unknown handling.
- Binary identity hashing and env snapshots.
- Repo hygiene (fixtures/, out/, reports/, gitignore).
- Commit discipline tooling.

Invariants established:
- The binary is the source of truth.
- Docs/help are claims, not truth.
- Man pages are rendered views; the claim set and validation results are the core artifact.
- Unknowns are first-class; no guessing.

## M1 — Surface Claim Ingestion (done)

Goal: Turn documentation into auditable, deterministic surface claims.

What landed:
- Conservative --help parser.
- Canonical option IDs (prefer long options).
- Separate claims for option existence and explicit parameter binding (Tier 1) from syntax (=ARG, [=ARG] only).
- Full audit fields (extractor, raw_excerpt, source).
- Golden snapshot test for ls --help.

Explicitly not done:
- No behavior semantics.
- No validation.
- No inference beyond explicit syntax.

## M2 — Surface Validation: Option Existence (done)

Goal: Validate that claimed options actually exist in the binary.

Scope:
- Validate only claim:option:*:exists.
- Execute the binary under controlled env.
- Classify each claim as confirmed/refuted/undetermined.
- Record evidence for every attempt.

Deliverable:
- ValidationReport tied to a concrete BinaryIdentity.

Deferred:
- Tier-1 parameter binding validation.
- Behavior validation.
- Rendered views (man page).

## M2.5 — Tier-1 Parameter Binding Validation (done)

Goal: Validate only what the docs explicitly claim about parameter binding.

Scope:
- Validate Tier-1 parameter binding claims where syntax is explicit.
- Required vs optional values only.
- Still no semantics.

Deliverable:
- Extended ValidationReport with Tier-1 parameter binding results.

## M3 — Minimal Regeneration (done)

Goal: Prove the pipeline can render a truthful view from validated claims.

Scope:
- Generate a minimal man page view that includes:
  - Confirmed T0/T1 claims (option existence + parameter binding)
  - Refuted T0/T1 claims (flagged)
  - Undetermined T0/T1 claims (explicitly listed)
- Include binary hash/version header.
- Intentionally barebones.

## M4 — Conceptual Alignment (done)

Goal: Align implementation with the claim-centric model and input modes while
expanding validation coverage using GNU coreutils `ls` as the benchmark.

What landed:
- Binary-as-source-of-truth is validated in practice.
- Tier-0 (option existence) and Tier-1 (parameter binding) are finite and closeable.
- Probe design is the primary determinant of success; parsing docs is not.
- Under the env contract (LC_ALL=C, TZ=UTC, TERM=dumb), Tier-1 uncertainty can be driven to zero for `ls`.
- Unknowns are meaningful only when pushed explicitly into higher tiers by design.
- Tier-1 probes were tightened to close remaining `ls` undetermined bindings.

Why M4 is closed:
- Scenario frameworks and progressive affordance exploration explode scope.
- Parameter domain/type and behavior semantics require fixtures and per-binary logic.
- Bespoke parsing for help/man text does not scale and undermines binary-agnostic speed.

Explicitly deferred:
- Scenario-based progressive exploration (old M5).
- Tier-2+ parameter form/domain/behavior semantics.

## M5 — Fast Binary-Only Surface Extraction (T0 + T1)

Purpose:
- Deliver a fast, binary-agnostic pipeline that, given only a binary,
  produces a closed, validated Tier-0 and Tier-1 interface surface
  and a minimal documentation view in seconds.

Scope:
- Inputs:
  - binary path only
  - controlled environment contract (LC_ALL=C, TZ=UTC, TERM=dumb)
- Outputs:
  1) Validated surface contract (T0 existence, T1 parameter binding)
  2) Minimal rendered view (man-like or markdown)
  3) Evidence sufficient for auditability
- Performance target:
  - coreutils-scale tools complete in ≤5 seconds
  - cacheable by (binary hash, env contract, planner version, probe library version)

Explicit non-goals:
- No scenario framework or fixtures
- No parameter form/domain/behavior semantics (T2–T4)
- No man-page parsing or source parsing
- No progressive exploration
- No per-binary validation profiles

Role of the LM Planner in M5:

The LM is required, not a source of truth, and not a claim generator.

Its role in M5 is narrowly defined as:
- Probe planner and prioritizer for Tier-0 and Tier-1 validation only.

The LM MUST:
- Consume binary self-report and a fixed probe library.
- Honor a probe budget and stop rules.
- Output JSON only (schema-validated, persisted, failure-closed).

The LM MUST NOT:
- Propose new options not present in binary self-report
- Propose semantics, domains, or scenarios
- Assert claims as true or false
- Generate documentation text

The LM backend must be:
- Required and swappable
- Failure-closed (invalid output → abort fast-pass)
- Small and fast (seconds-level inference)
