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

With seed: `{"kind": "add_behavior_scenario", "argv": ["--option", "input.txt"], "seed": {"files": {"input.txt": "line1\nline2"}}}`

**IMPORTANT**: If your seed creates input files, include them in argv! Many commands read from files, not stdin.
- `cat -n input.txt` (not just `cat -n`)
- `tac input.txt` (not just `tac`)
- `cut -f1 input.txt` (not just `cut -f1`)

Seed fields (all optional):
- `files`: `{"name": "content"}` - create files (include filename in argv if needed!)
- `dirs`: `["name"]` - create directories
- `symlinks`: `{"link": "target"}` - create symlinks
- `executables`: `{"script.sh": "content"}` - create executable files

**Seed paths must be RELATIVE** (e.g., `input.txt`, `work/data.txt`). Never use absolute paths like `/tmp` or `/home/...`. The sandbox already provides a working directory.

**2. add_value_examples**: Specify valid values.
`{"kind": "add_value_examples", "value_examples": ["val1", "val2"]}`

**3. add_requires_argv**: Specify prerequisite flags.
`{"kind": "add_requires_argv", "requires_argv": ["-l"]}`

**4. add_exclusion**: Mark as untestable.
`{"kind": "add_exclusion", "reason_code": "fixture_gap", "note": "Why untestable (max 200 chars)"}`

Valid reason codes:
- `fixture_gap` - needs complex fixtures we can't easily create, OR behavior is timing/side-effect based with no stdout to verify (e.g., `sleep NUMBER` affects timing, not output)
- `assertion_gap` - output varies in ways we can't assert on
- `nondeterministic` - output changes between runs
- `requires_interactive_tty` - needs TTY/terminal interaction
- `unsafe_side_effects` - modifies system state dangerously
- `blocks_indefinitely` - waits forever for input (e.g., `tail --follow`, `cat` without file)

Use `blocks_indefinitely` for options like `--follow`, `-f` that wait for file changes or stdin.

Use `fixture_gap` for behaviors that are timing-based (e.g., `sleep`, `timeout`) or produce side effects without stdout changes (e.g., file creation with `touch`, permission changes). Note these in the exclusion for future assertion support.

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
