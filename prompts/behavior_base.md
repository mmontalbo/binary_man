# Behavior Verification for `{binary_name}`

Generate test scenarios or exclusions for unverified options.

## Reason: {reason_code}

{reason_section}

{context_section}

## Target Options

{targets}

## Response Format

Respond with JSON containing a `responses` array. Each response has:
- `surface_id`: The option id (e.g., "--verbose")
- `action`: One of the action types below

### Action Types

**1. add_behavior_scenario** (preferred): Create a test scenario.

Basic: `{"kind": "add_behavior_scenario", "argv": ["--option"]}`

With seed: `{"kind": "add_behavior_scenario", "argv": ["--option"], "seed": {"files": {"f.txt": "x"}, "dirs": ["d"]}}`

Seed fields (all optional):
- `files`: `{"name": "content"}` - create files
- `dirs`: `["name"]` - create directories
- `symlinks`: `{"link": "target"}` - create symlinks
- `executables`: `{"script.sh": "content"}` - create executable files

**2. add_value_examples**: Specify valid values.
`{"kind": "add_value_examples", "value_examples": ["val1", "val2"]}`

**3. add_requires_argv**: Specify prerequisite flags.
`{"kind": "add_requires_argv", "requires_argv": ["-l"]}`

**4. add_exclusion**: Mark as untestable.
`{"kind": "add_exclusion", "reason_code": "fixture_gap", "note": "Why untestable (max 200 chars)"}`

Valid reason codes: `fixture_gap`, `assertion_gap`, `nondeterministic`, `requires_interactive_tty`, `unsafe_side_effects`

**5. skip**: Skip for now.
`{"kind": "skip", "reason": "Need more context"}`

## Example Response

```json
{
  "schema_version": 1,
  "responses": [
    {"surface_id": "--mode", "action": {"kind": "add_value_examples", "value_examples": ["fast", "slow"]}},
    {"surface_id": "--interactive", "action": {"kind": "add_exclusion", "reason_code": "requires_interactive_tty", "note": "Requires TTY"}}
  ]
}
```

Respond ONLY with JSON.
