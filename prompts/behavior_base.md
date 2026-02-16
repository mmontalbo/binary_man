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

**CRITICAL: Use the EXACT option form in argv.** When verifying `--delete`, use `--delete` in argv (not `-d`). When verifying `-d`, use `-d` (not `--delete`). The argv must contain the exact surface_id being tested.

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

**Stdin input**: For filter commands that read from stdin (tr, cut, sort, uniq, sed, awk), use the `stdin` field:

```json
{
  "kind": "add_behavior_scenario",
  "argv": ["-d", "aeiou"],
  "stdin": "hello world"
}
```

The command receives this content on stdin. Use stdout assertions to verify the transformation:

```json
{
  "kind": "add_behavior_scenario",
  "argv": ["-d:", "-f2"],
  "stdin": "root:x:0:0\nnobody:x:65534:65534",
  "assertions": [
    {"kind": "stdout_contains", "run": "variant", "seed_path": null, "token": "x"}
  ]
}
```

**Guidelines for stdin:**
- Use stdin for filter commands, NOT file arguments
- Keep stdin content minimal - just enough to verify behavior
- Include multiple lines when the option's behavior depends on line structure
- Maximum stdin size: 64KB (UTF-8 only)

**2. add_value_examples**: Specify valid values.
`{"kind": "add_value_examples", "value_examples": ["val1", "val2"]}`

**3. add_requires_argv**: Specify prerequisite flags.
`{"kind": "add_requires_argv", "requires_argv": ["-l"]}`

**4. add_exclusion**: Mark as untestable.
`{"kind": "add_exclusion", "reason_code": "fixture_gap", "note": "Why untestable (max 200 chars)"}`

Valid reason codes:
- `fixture_gap` - needs complex fixtures we can't easily create
- `assertion_gap` - output varies in ways we can't assert on
- `nondeterministic` - output changes between runs
- `requires_interactive_tty` - needs TTY/terminal interaction
- `unsafe_side_effects` - modifies system state dangerously
- `blocks_indefinitely` - waits forever for input (e.g., `tail --follow`, `cat` without file)

Use `blocks_indefinitely` for options like `--follow`, `-f` that wait for file changes or stdin.

**File assertions are now available** for commands that create files/directories instead of producing stdout output. Use file assertions instead of `fixture_gap` for:
- `touch` - creates files → use `file_exists` assertion
- `mkdir` - creates directories → use `dir_exists` assertion
- Commands that write to files → use `file_contains` assertion

See "File assertions" section below for syntax.

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

## File Assertions

For commands that create files/directories instead of producing stdout output, use file assertions:

**Assertion kinds:**
- `file_exists` - verify a file was created (not a directory)
- `file_missing` - verify a file does NOT exist (use for rm)
- `dir_exists` - verify a directory was created
- `dir_missing` - verify a directory does NOT exist (use for rmdir)
- `file_contains` - verify a file contains a pattern (requires `pattern` field)

**Example: touch command**
```json
{
  "kind": "add_behavior_scenario",
  "argv": ["newfile.txt"],
  "assertions": [
    {"kind": "file_exists", "path": "newfile.txt"}
  ]
}
```

**Example: mkdir command**
```json
{
  "kind": "add_behavior_scenario",
  "argv": ["-p", "parent/child"],
  "assertions": [
    {"kind": "dir_exists", "path": "parent"},
    {"kind": "dir_exists", "path": "parent/child"}
  ]
}
```

**Example: file_contains**
```json
{
  "kind": "add_behavior_scenario",
  "argv": ["-o", "output.txt"],
  "assertions": [
    {"kind": "file_exists", "path": "output.txt"},
    {"kind": "file_contains", "path": "output.txt", "pattern": "expected content"}
  ]
}
```

**Example: rmdir command (verify deletion)**
```json
{
  "kind": "add_behavior_scenario",
  "argv": ["-p", "parent/child"],
  "seed": {"dirs": ["parent/child"]},
  "assertions": [
    {"kind": "dir_missing", "path": "parent/child"},
    {"kind": "dir_missing", "path": "parent"}
  ]
}
```

**Rules:**
- Paths must be relative (no `/tmp` or `/home/...`)
- Paths must not contain `..`
- File assertions are variant-only (no baseline comparison needed)
- Use `file_exists` when the command should create a regular file
- Use `dir_exists` when the command should create a directory
- Use `file_missing` to verify deletion or that a file was NOT created

Respond ONLY with JSON.
