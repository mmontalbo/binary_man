# Binary-Validated Man Pages

Generate trustworthy surface contracts by probing binaries and rendering minimal views.

The binary on disk is the source of truth. Self-report output (--help/--version/usage errors) is
used only to plan probes; validation relies on controlled execution. The core artifact is a
validated T0/T1 surface report plus probe evidence.

## Motivation

Man pages drift from actual behavior, omit defaults, and diverge across versions. When docs are
wrong or incomplete, users and models must guess. This project replaces guesswork with measured
validation.

## Goal

- Capture binary self-report under a controlled env contract.
- Use an LM planner to select and order T0/T1 probes.
- Execute probes deterministically and record evidence.
- Render a minimal view tied to a specific binary identity.

## Current Focus (M5)

- Fast, binary-only surface extraction for T0/T1.
- Inputs: binary path + controlled env contract.
- Outputs: validated surface contract, minimal rendered view, and audit-ready evidence.
- Scenario frameworks, higher-tier semantics, and doc/source parsing are deferred.

## Fast-Pass Flow (M5)

1) Capture self-report (--help, --version, usage error).
2) LM planner emits a probe plan (JSON, schema-validated).
3) Execute probes and synthesize the T0/T1 surface report.
4) Render a minimal view; higher tiers are marked not evaluated.

## Usage (M5)

```
BVM_PLANNER_CMD=/path/to/planner bvm surface /path/to/bin --out-dir ./out
BVM_PLANNER_PLAN=/path/to/plan.json bvm surface /path/to/bin --out-dir ./out
```

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

## Source of Truth

- Binary identity is recorded (path, hash, platform, env).
- Self-report output is non-authoritative and only used to plan probes.
- Rendered views are derived from validated probe evidence.

## Validation and Outputs

- Validation runs under the controlled environment contract; fixtures are deferred in M5.
- Outputs include a machine-readable surface report and a minimal rendered view.

## Required LM Planner (M5)

The LM planner is required and narrowly scoped:

- Inputs: binary self-report, fixed probe library, budget, stop rules.
- Output: JSON-only probe plan (schema-validated, persisted, failure-closed).
- It must not propose new options, semantics, or documentation text.

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
