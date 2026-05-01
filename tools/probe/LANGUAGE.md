# Probe Test Script Language

A `.test` file describes execution contexts, invocations of a binary, and predictions about its output. The tool runs the tests, annotates the file with observations and outcomes, and reports quality metrics.

## Structure

```
# Comments start with #
# Tool annotations start with #> (stripped and regenerated each run)

context "name"
  <setup commands>

context "other" extends "name"
  <additional setup>

test args "arg1" "arg2"
  expect <dimension> <predicate>
  expect <dimension> <predicate>

test args "arg1" "arg2" "arg3" in "name" "other"
  expect <dimension> <predicate>
```

Contexts come first, then test blocks. Test blocks run in all contexts by default, or in specific contexts with the `in` clause. Each invocation gets a fresh context instance.

## Contexts

A named execution context declares the input state the binary will see.

```
context "base"
  file "visible.txt" "hello"
  file ".hidden" "secret"
  dir "subdir"
  env LANG "C"

context "with backups" extends "base"
  file "backup.txt~" "old"

context "minimal"
  file "only.txt" "alone"
```

`extends` inherits all setup from another context, then applies additional commands. Contexts in `setup.test` are shared across all surface files in the same directory.

### Setup commands

**file** — create a file:
```
file "path" "line1" "line2"       # content lines joined with \n
file "path" size 1000             # filled to N bytes
file "path" empty                 # empty file
file "path"                       # also empty
file "path" from "fixtures/data"  # copy from external path
```

**dir** — create a directory:
```
dir "subdir"
dir "path/to/nested"
```

**link** — create a symbolic link:
```
link "name" -> "target"           # target need not exist
```

**props** — set properties on an existing path:
```
props "path" executable           # chmod +x
props "path" readonly             # chmod -w
props "path" mtime old            # set mtime to year 2000
props "path" mtime recent         # touch (update mtime to now)
```

**env** — set an environment variable:
```
env LANG "C"
env HOME "."
```

**remove** — remove a path inherited from `extends`:
```
context "no hidden" extends "base"
  remove ".hidden"
```

**invoke** — run the binary under test during context setup:
```
context "repo"
  invoke "init"
  file "readme.md" "hello"
  invoke "add" "."
  invoke "commit" "-m" "initial"
```

`invoke` runs the same binary being probed — not arbitrary commands. Used to bootstrap complex state (git repos, databases) from the binary's own verified operations.

## Test blocks

Each test block declares an invocation and predictions about its behavior.

```
test args "." "-a"
  expect stdout superset vs "."
  expect stdout contains ".hidden"
  expect exit 0
```

Arguments are passed directly to the binary. Multiple test blocks can reference each other via `vs` clauses — the args identify the invocation.

**Context scoping** — limit a test block to specific contexts:
```
test args "." "-B" in "with backups"
  expect stdout not-contains "backup.txt~"
```

Without `in`, the test runs in all contexts.

**Stdin** — provide input on stdin:
```
test args "-i" "pattern"
  stdin "Hello World" "foo bar" "hello again"
  expect stdout contains "Hello"

test args
  stdin from "data.txt"
  expect stdout not-empty
```

**Per-invocation environment:**
```
test args "."
  env LANG "en_US.UTF-8"
  expect stdout not-empty
```

## Expect predicates

### stdout

**Content:**

| Syntax | Meaning |
|---|---|
| `empty` | stdout is empty (whitespace-only) |
| `not-empty` | stdout has content |
| `contains "text"` | stdout contains the substring |
| `not-contains "text"` | stdout does not contain the substring |
| `every-line-matches "regex"` | every non-empty line matches the regex |

**Positional:**

| Syntax | Meaning |
|---|---|
| `line N contains "text"` | line N (1-indexed, non-empty lines) contains substring |
| `line N not-contains "text"` | line N does not contain substring |
| `"X" before "Y"` | line containing X appears before line containing Y |

**Quantitative (vs another invocation):**

| Syntax | Meaning |
|---|---|
| `lines exactly N` | exactly N non-empty lines |
| `lines same as "args"` | same non-empty line count as reference |
| `lines more than "args"` | more non-empty lines than reference |
| `lines fewer than "args"` | fewer non-empty lines than reference |

**Structural (vs another invocation):**

| Syntax | Meaning |
|---|---|
| `reordered vs "args"` | same lines, different order |
| `superset vs "args"` | all reference lines present, plus more |
| `subset vs "args"` | only a subset of reference lines |
| `preserved vs "args"` | all entry names present, format may differ |

### stderr

| Syntax | Meaning |
|---|---|
| `empty` | stderr is empty |
| `not-empty` | stderr has content |
| `contains "text"` | stderr contains substring |
| `unchanged vs "args"` | stderr identical to reference invocation |

### exit

| Syntax | Meaning |
|---|---|
| `N` | exit code equals N |
| `unchanged vs "args"` | same exit code as reference |
| `changed vs "args"` | different exit code from reference |

### file (post-execution)

| Syntax | Meaning |
|---|---|
| `file "path" exists` | file exists after invocation |
| `file "path" not-exists` | file does not exist after invocation |
| `file "path" contains "text"` | file content contains substring |

## Annotations

Tool-generated lines use the `#>` prefix. They are stripped and regenerated on each run.

**Observations** — what the binary produced:
```
test args "." "-a"
  #> stdout (base):
  #>   .
  #>   ..
  #>   .hidden
  #>   visible.txt
  #> stdout (no hidden): 4 lines
  #> stdout (empty): 2 lines
  #> exit: 0 in all contexts
```

**Check results** — whether predictions matched:
```
  expect stdout superset vs "."
  #> passed in all 3 contexts
  expect stdout contains ".hidden"
  #> passed in: base (failed in: no hidden, empty)
```

**Per-file summary:**
```
#> 8/10 passed
#> properties: superset vs ".", contains ".", contains "..", exit 0
#> context-dependent: contains ".hidden" (needs hidden files in context)
#> failed: lines exactly 7 (context-specific count)
#> confused with: -f
```

**Categories:**
- **Properties** — passed in all contexts tested. General truths about the behavior.
- **Context-dependent** — passed in some contexts. Reveals preconditions.
- **Failed** — wrong predictions. The revision history.

## Directory structure

```
surfaces/<binary>/
  _status.md              # generated: corpus summary
  _bootstrap.test         # help text, default output, errors
  setup.test              # shared contexts
  <surface>.test           # one file per behavioral surface
```

The tool operates on the whole directory:
```
bman-probe <binary> surfaces/<binary>/
```

`setup.test` contexts are available to all surface files. `_status.md` summarizes the corpus — what's tested, what's stubbed, what's untested, what needs work. It is regenerated each run.

## Example

```
# ls -r: reverse sort order

context "base"
  file "alpha.txt" "a"
  file "beta.txt" "b"
  file "gamma.txt" "c"

context "more files" extends "base"
  file "delta.txt" "d"
  file "epsilon.txt" "e"

test args "."
  expect stdout not-empty
  expect exit 0

test args "." "-r"
  expect stdout reordered vs "."
  expect stdout lines same as "."
  expect stdout line 1 contains "gamma.txt" in "base"
  expect exit 0
```
