# Binary-Validated Man Pages

Generate trustworthy documentation by synthesizing claims, validating them against a specific
binary, and rendering human-oriented views such as man pages.

The binary on disk is the source of truth. Man pages, --help output, and source excerpts are
treated as claims with provenance. We validate those claims through controlled execution; we do
not validate man pages. Man pages are one rendered view over the validated claim set.

The core artifact is the synthesized, provenance-tracked claim set plus its validation results.

## Motivation

Man pages drift from actual behavior, omit defaults, and diverge across versions. When docs are
wrong or incomplete, users and models must guess. This project replaces guesswork with measured
validation.

## Goal

- Synthesize a unified, provenance-tracked claim set from enabled inputs.
- Execute the binary under controlled environments to validate claims.
- Classify each claim as confirmed, refuted, or undetermined.
- Render man pages and other views from validated claims, tied to a specific binary identity.

## Current Focus (M5)

- Fast, binary-only surface extraction for T0/T1.
- Inputs: binary path + controlled env contract.
- Outputs: validated surface contract, minimal rendered view, and audit-ready evidence.
- Scenario frameworks, higher-tier semantics, and doc/source parsing are deferred.

## Two-Phase Process

Phase A: Claim synthesis.
Inputs include binary observations and binary self-reports (--help). Other sources (docs,
annotations, source excerpts) are treated as optional claims and are deferred in M5. Output is a
single unified claim set.

Phase B: Validation + rendering.
Claims are confirmed/refuted/undetermined via controlled binary execution. Outputs include a
validation report, a rendered man page, and (future) other views.

Both binary-only and binary + docs modes use this same pipeline; they differ only in which inputs
are enabled.

## Input Modes

Minimum input (binary only) yields sparse, maximally trustworthy documentation and is the current
focus. Augmented input (binary + existing docs + annotations) yields richer documentation, still
constrained by validation, but is deferred beyond M5.

## Parameter Surface Tiers

Option parameters are evaluated as a tiered surface:

- T0: Option existence.
- T1: Parameter binding (required vs optional value).
- T2: Parameter form (attachment style, repeatability).
- T3: Parameter domain/type (enum, numeric, path-like).
- T4: Behavioral semantics.

The tiered surface is a coverage accounting model for large parameter spaces, not an attempt at
exhaustive semantics.

Only T0 and T1 are in scope today. Higher tiers may remain not evaluated indefinitely.

## What "Comprehensive" Means

A rendered man page view is comprehensive when every user-visible surface is either validated at
its tier or explicitly marked as undetermined/not evaluated. The goal is accounting and honesty,
not completeness.

Requirements:

- Tiered surface coverage: report % confirmed/undetermined for T0/T1; higher tiers are marked not evaluated.
- Large parameter spaces are accounted for via coverage + unknowns, not exhaustive enumeration.
- Behavioral semantics (T4) only included when validated; otherwise out of scope.
- Observational grounding: every statement traceable to evidence or marked unknown.
- Negative space: document limits, variability, and untested cases.

## Source of Truth and Claims

- Binary identity is recorded (path, hash, platform, env).
- Documentation inputs are non-authoritative claims until validated.
- Man pages are rendered views, not authoritative inputs.

## Validation and Outputs

- Validation runs under the controlled environment contract; fixtures are deferred in M5.
- Outputs include a machine-readable validation report and rendered views (man page today; other
  views later).

## Small LM Backend (M5)

An optional small LM may be used only to plan and prioritize Tier-0/Tier-1 probes based on binary
self-report and known probe types. It must be swappable, failure-closed, and never treated as a
source of truth or documentation text.

## Environment Contract

Validation is tied to a controlled execution contract:

- LC_ALL=C
- TZ=UTC
- TERM=dumb
- temp fs fixtures (when required)

Results are valid only under this contract. Environment-sensitive behavior is classified as
undetermined.

## Scope

- Initial target: a single coreutils-style binary (e.g. ls).
- Current validation scope: T0 option existence and T1 parameter binding.
- Stop when tiered surface completeness is reached and remaining gaps are documented.

See `docs/MILESTONES.md` for the current plan and status and `docs/SCHEMAS.md` for schema
definitions and the tiered surface model.

## Evaluation Criteria

- Only observed behaviors are documented.
- Defaults are explicit.
- Discrepancies are justified with evidence.
- Unknowns are clearly marked.
- Outputs are tied to a specific binary hash.
