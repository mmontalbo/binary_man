# Schemas

These schemas define the minimal, audit-friendly data exchanged during the fast-pass pipeline.

## Notes

- Self-report is non-authoritative and used only to plan probes.
- Unknowns are first-class: use `undetermined` when evidence is insufficient.
- Higher tiers are explicitly marked as not evaluated.
- The goal is accounting and honesty, not completeness.

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

## SelfReport

```json
{
  "help": {
    "args": ["--help"],
    "exit_code": 0,
    "stdout": "...",
    "stderr": "..."
  },
  "version": {
    "args": ["--version"],
    "exit_code": 0,
    "stdout": "...",
    "stderr": "..."
  },
  "usage_error": {
    "args": ["--__bvm_unknown__"],
    "exit_code": 2,
    "stdout": "...",
    "stderr": "..."
  }
}
```

## ProbePlan (LM Output)

```json
{
  "planner_version": "v1",
  "options": [
    { "option": "--all", "probes": ["existence", "invalid_value"] },
    { "option": "--block-size", "probes": ["existence", "invalid_value", "option_at_end"] }
  ],
  "budget": { "max_total": 300, "max_per_option": 3 },
  "stop_rules": { "stop_on_unrecognized": true, "stop_on_binding_confirmed": true }
}
```

Probe types:

- `existence`: run `<opt> --help` and detect unrecognized/ambiguous responses.
- `invalid_value`: run `<opt>` with a dummy value and `--help` to detect binding.
- `option_at_end`: run `<opt>` at end (no `--help`) to detect missing-arg responses.

## SurfaceReport

```json
{
  "invoked_path": "/usr/bin/ls",
  "binary_identity": { "path": "/abs/path/to/binary", "hash": { "algo": "blake3", "value": "..." } },
  "planner": { "version": "v1", "plan_hash": "..." },
  "probe_library_version": "v1",
  "timings_ms": { "planner_ms": 12, "probes_ms": 148, "total_ms": 185 },
  "self_report": { "help": { "args": ["--help"], "exit_code": 0, "stdout": "...", "stderr": "..." } },
  "options": [
    {
      "option": "--all",
      "existence": { "status": "confirmed", "reason": null, "evidence": ["...Evidence..."] },
      "binding": {
        "status": "confirmed",
        "kind": "no_value",
        "reason": "argument not allowed response observed",
        "evidence": ["...Evidence..."]
      }
    }
  ],
  "higher_tiers": { "t2": "not_evaluated", "t3": "not_evaluated", "t4": "not_evaluated" }
}
```

## Evidence

```json
{
  "args": ["--long"],
  "env": { "LC_ALL": "C", "TZ": "UTC", "TERM": "dumb" },
  "exit_code": 0,
  "stdout": "blake3:...",
  "stderr": "blake3:...",
  "notes": "probe=existence"
}
```
