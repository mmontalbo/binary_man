# Milestones

This document records the current milestone plan and status. It is the canonical
sequence for the project.

Current focus: M4 — Conceptual Alignment (in progress).

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

## M4 — Conceptual Alignment (in progress)

Goal: Align implementation with the claim-centric model and input modes.

Scope:
- Synthesize a unified claim set from enabled inputs (help, man pages, source excerpts, annotations),
  with provenance preserved for each claim.
- Make minimum vs augmented input modes explicit configuration choices, not separate systems.
- Ensure rendering treats validation results as authoritative; missing results default to undetermined.
- Keep outputs tied to a specific binary identity and validation report.

First steps:
- Wire claims synthesis to consume all enabled inputs, even if some parsers are still minimal.
- Define a merge/dedup strategy for claims across sources while preserving provenance.
- Make input mode selection explicit in the CLI workflow and docs.
- Add a guard so rendering never treats unvalidated claims as authoritative.
