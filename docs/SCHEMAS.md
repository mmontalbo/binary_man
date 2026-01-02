# Schemas

These schemas define the minimal, audit-friendly data exchanged between pipeline stages.

## Notes

- Claim extraction is heuristic. Every claim must record its `extractor` and a `raw_excerpt` for auditability.
- Claims are not truth. They are unvalidated assertions until tested against the binary.
- Unknowns are first-class: use `undetermined` when evidence is insufficient.
- The goal is accounting and honesty, not completeness.

## Conceptual Model

The core artifact is the unified claim set plus validation results; man pages are rendered views
derived from that set. We synthesize claims from inputs and validate them against the binary; we do
not validate man pages.

Phase A: Claim synthesis. Inputs include binary observations, binary self-reports (--help), existing
docs, and optional annotations. Output is a single unified claim set.

Phase B: Validation + rendering. Claims are confirmed/refuted/undetermined via controlled binary
execution. Outputs include a validation report and rendered views (man page today; other views
later).

Minimum (binary-only) and augmented (binary + docs + annotations) modes are configuration choices
that feed the same pipeline.

## Parameter Surface Tiers

Option parameters are evaluated as a tiered surface:

- T0: Option existence.
- T1: Parameter binding (required vs optional value).
- T2: Parameter form (attachment style, repeatability).
- T3: Parameter domain/type (enum, numeric, path-like).
- T4: Behavioral semantics.

Only T0 and T1 are in scope today. Higher tiers may remain not evaluated indefinitely.

Large parameter spaces are accounted for via coverage reporting and explicit unknowns, not by
exhaustive enumeration.

## Environment Contract

Validation is tied to a controlled execution contract:

- LC_ALL=C
- TZ=UTC
- TERM=dumb
- temp fs fixtures (when required)

Results are valid only under this contract. If behavior appears environment-sensitive, it should
be classified as `undetermined`.

## BinaryIdentity

```json
{
  "path": "/abs/path/to/binary",
  "hash": { "algo": "blake3", "value": "..." },
  "platform": { "os": "linux", "arch": "x86_64" },
  "env": { "locale": "C", "tz": "UTC", "term": "dumb" }
}
```

## Claim

```json
{
  "id": "string",
  "text": "string",
  "kind": "option|behavior|env|io|error|exit_status",
  "source": { "type": "man|help|source", "path": "string", "line": 0 },
  "status": "unvalidated",
  "extractor": "parse:man:v0",
  "raw_excerpt": "string",
  "confidence": 0.42
}
```

M1 focuses on surface claims (`option`, `io`, `exit_status`). Behavior claims are tagged but validated later.

## ClaimsFile

```json
{
  "binary_identity": null,
  "invocation": "ls",
  "capture_error": null,
  "claims": ["...Claim..."]
}
```

`binary_identity` is optional when claims were derived from static files. It is present only when
help text was captured directly from the binary by this tool (or explicitly asserted in the future).
`invocation` is a human-facing label (argv0) and is not part of binary identity. `capture_error`
records a failure to capture help output; in that case, `claims` may be empty.

## Evidence

```json
{
  "args": ["--long"],
  "env": { "LC_ALL": "C", "TZ": "UTC" },
  "exit_code": 0,
  "stdout": "blake3:...",
  "stderr": "blake3:...",
  "notes": "string"
}
```

## ValidationResult

```json
{
  "claim_id": "string",
  "status": "confirmed|refuted|undetermined",
  "method": "acceptance_test|behavior_fixture|stderr_match|exit_code|output_diff|other",
  "determinism": "deterministic|env_sensitive|flaky",
  "attempts": ["...Evidence..."],
  "observed": "string",
  "reason": "string"
}
```

Confirmed means evidence directly implies the claim. Refuted means evidence directly contradicts it.
Undetermined means tests ran but were inconclusive.

## ValidationReport

```json
{
  "binary_identity": { "...BinaryIdentity..." },
  "results": ["...ValidationResult..."]
}
```

## RegenerationReport

```json
{
  "binary_identity": { "...BinaryIdentity..." },
  "claims_path": "./claims.json",
  "results_path": "./validation.json",
  "out_man": "./tool.1"
}
```
