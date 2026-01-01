# Schemas

These schemas define the minimal, audit-friendly data exchanged between pipeline stages.

## Notes

- Claim extraction is heuristic. Every claim must record its `extractor` and a `raw_excerpt` for auditability.
- Claims are not truth. They are unvalidated assertions until tested against the binary.
- Unknowns are first-class: use `undetermined` when evidence is insufficient.

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
  "stdout": "hash:blake3:...",
  "stderr": "hash:blake3:...",
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
