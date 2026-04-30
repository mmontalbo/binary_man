# Probe Test Script Language

A `.test` file describes a sandbox, a set of invocations, and predictions about their output. The tool builds the sandbox, runs each invocation, checks every prediction, and reports quality metrics (discrimination, cross-flag specificity).

## Structure

```
# Comments start with #

<setup commands>

test args "arg1" "arg2" ...
  expect <dimension> <predicate>
  expect <dimension> <predicate>

test args "arg1" "arg2" "arg3" ...
  expect <dimension> <predicate>
```

Setup commands come first. Then one or more test blocks, each starting with `test args` followed by indented `expect` lines. The first test block is treated as the control for discrimination analysis.

## Setup Commands

### file

Create a file in the sandbox.

```
file "path" "line1" "line2"       # File with content (lines joined with \n)
file "path" size 1000             # File filled to N bytes
file "path" empty                 # Empty file (0 bytes)
file "path"                       # Also empty file
```

Parent directories are created automatically. Content strings support escape sequences: `\n`, `\t`, `\\`, `\"`.

### dir

Create a directory.

```
dir "subdir"
dir "path/to/nested"              # Parent dirs created automatically
```

### link

Create a symbolic link.

```
link "name" -> "target"
```

The target does not need to exist (creates a broken symlink).

### props

Set properties on an existing file or directory.

```
props "path" executable           # chmod +x
props "path" readonly             # chmod -w
props "path" mtime old            # Set mtime to year 2000
props "path" mtime recent         # Touch file (update mtime to now)
```

Multiple properties can be set in one line: `props "path" executable mtime old`.

### env

Set an environment variable for all invocations.

```
env LANG "en_US.UTF-8"
```

## Test Blocks

Each test block declares an invocation and its expected behavior.

```
test args "." "-a"
  expect stdout superset vs "."
  expect stdout contains ".hidden"
  expect exit 0
```

The `args` are passed directly to the binary. Multiple test blocks can reference each other via `vs` clauses — the args identify the invocation.

## Expect Predicates

### stdout

**Content checks:**

| Syntax | Meaning |
|---|---|
| `empty` | stdout is empty (whitespace-only) |
| `not-empty` | stdout has content |
| `contains "text"` | stdout contains the substring |
| `not-contains "text"` | stdout does not contain the substring |
| `every-line-matches "regex"` | every non-empty line matches the regex |

**Positional checks:**

| Syntax | Meaning |
|---|---|
| `line N contains "text"` | line N (1-indexed, non-empty lines) contains substring |
| `line N not-contains "text"` | line N does not contain substring |
| `"X" before "Y"` | line containing X appears before line containing Y |

**Quantitative checks (vs another invocation):**

| Syntax | Meaning |
|---|---|
| `lines exactly N` | exactly N non-empty lines |
| `lines same as "arg1" "arg2"` | same non-empty line count as reference |
| `lines more than "arg1" "arg2"` | more non-empty lines than reference |
| `lines fewer than "arg1" "arg2"` | fewer non-empty lines than reference |

**Structural checks (vs another invocation):**

These use the delta classifier to compare output structure against a reference invocation.

| Syntax | Meaning |
|---|---|
| `reordered vs "args"` | same lines, different order |
| `superset vs "args"` | contains all lines from reference plus more |
| `subset vs "args"` | contains only a subset of reference lines |
| `preserved vs "args"` | all entry names present, format may differ |

### stderr

| Syntax | Meaning |
|---|---|
| `empty` | stderr is empty |
| `not-empty` | stderr has content |
| `contains "text"` | stderr contains substring |
| `unchanged vs "arg1" "arg2"` | stderr identical to reference invocation |

### exit

| Syntax | Meaning |
|---|---|
| `N` | exit code equals N (e.g., `expect exit 0`) |
| `unchanged vs "arg1" "arg2"` | same exit code as reference |
| `changed vs "arg1" "arg2"` | different exit code from reference |

## Output

The tool reports per-test results with quality metrics:

```
Binary: ls
Setup: 5 commands
Tests: 2
  ✓ test args ["."]: 4/4 predictions
  ✓ test args [".", "-a"]: 6/6 predictions (1 non-discriminating)
    ~ expected exit 0, got 0
    [confused with: -f]
```

- `✓` — check passed
- `✗` — check failed (with context showing observed output)
- `~` — check passed but is non-discriminating (also passes on control)
- `[confused with: ...]` — other flags whose output also passes all checks
- `[only in control]` / `[only in option]` — diff shown for non-discriminating checks
- `[stdout identical]` — flag has no visible effect in pipe mode

## Example

```
# ls -r: reverse sort order

file "alpha.txt" "a"
file "beta.txt" "b"
file "gamma.txt" "c"
file "delta.txt" "d"

test args "."
  expect stdout not-empty
  expect exit 0

test args "." "-r"
  expect stdout reordered vs "."
  expect stdout lines same as "."
  expect stdout line 1 contains "gamma.txt"
  expect stdout "gamma.txt" before "alpha.txt"
  expect exit 0
```
