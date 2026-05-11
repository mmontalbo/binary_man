# Probe Language

## Model

A `.probe` file describes a grid of **input states × invocations**. The tool
executes every cell and writes observations to a `.results` file.

```
bgrid <binary>                        explore: discover flags + run grid + report
bgrid --skeleton <binary>             print probe skeleton for manual authoring
bgrid <binary> <file.probe>           run observation grid, write .results file
bgrid --dry-run <binary> <file.probe> show resolved grid without executing
```

`bgrid <binary>` runs the full exploration loop automatically. For manual
probe authoring, generate a skeleton and customize it:

```
bgrid --skeleton sort > sort.probe
# edit sort.probe — add vary blocks, organize runs
bgrid sort sort.probe
```

The tool is binary-agnostic — it knows nothing about any specific binary.
The same language tests ls, grep, sort, cp, git, ffmpeg, or any CLI tool.

The tool observes and summarizes. It does not assert, judge, or interpret.
Assertions, documentation generation, regression detection, and behavioral
clustering are external concerns — built on top of the observation data by
humans, LMs, or scripts reading the `.results` files.

## Concepts

Eight keywords: **context**, **vary**, **combine**, **invoke**, **run**, **from**, **in**, **stdin**.

### context

Declares a named input state — everything the binary will see.

```
context "base"
  file "visible.txt" "hello"
  file ".hidden" "secret"
  dir "subdir"
  env LANG "C"

context "empty"
```

**extends** inherits and modifies:

```
context "with backup" extends "base"
  file "backup.txt~" "old"

context "no hidden" extends "base"
  remove ".hidden"
```

### vary

Generates perturbation variants of a context. Each line produces one variant.

```
vary from "base"
  remove ".hidden"
  remove "subdir"
  file "visible.txt" size 1000
  props "visible.txt" mtime old
```

5 lines = 5 variants + the base = 6 states.

**vary compound** applies all perturbations together as one variant:

```
vary compound from "base"
  remove ".hidden"
  file "visible.txt" size 1000
```

1 variant = both perturbations applied simultaneously.

**vary stress** generates 8 adversarial mutations of a file:

```
vary stress from "base" "visible.txt"
```

Generates: null_inject, huge_line (1MB), truncated, repeated (1000x),
empty, invalid_utf8, line_explosion (100K lines), delimiter_flood.

### combine

Generates single + pairwise flag combinations from a list of flags.

```
combine "input.txt"
  "-r"
  "-n"
  "-u"
```

Produces 3 singles (`-r`, `-n`, `-u`) + 3 pairs (`-r -n`, `-r -u`, `-n -u`)
= 6 runs, all with `input.txt` as the trailing positional arg.

### invoke

Runs the binary during context setup. Output is discarded. Used to build
complex state (git repos, databases). Only valid inside context blocks.

```
context "repo"
  invoke "init"
  invoke "config" "user.email" "test@test.com"
  file "readme.md" "hello"
  invoke "add" "."
  invoke "commit" "-m" "initial"
```

If invoke exits non-zero, the context setup fails and is reported.

### run

Declares an invocation to observe. Only valid outside context blocks.

```
run "."
run "." "-a"
run "." "-l"
run "nonexistent"
```

Each run executes in every context. The tool records stdout, stderr, exit
code, and filesystem changes.

### from

Groups runs for diff comparison against a reference invocation. The
reference must also be declared as a run.

```
run "."

from "."
  run "." "-a"
  run "." "-B"
  run "." "-l"
```

Runs inside a `from` block get an additional diff section in the results
showing what's only in this run vs the reference, per context.

Runs outside any `from` block are standalone — observed but not diffed.

### in

Scopes runs to specific contexts. Can be used as a block (grouping
multiple runs) or as a modifier on a single run.

**Block-level** (scopes all contained runs and from blocks):

```
in "sort_sensitive"
  run "."

  from "."
    run "." "-r"
    run "." "-S"
    run "." "-t"
```

**Nesting** (in + from compose naturally):

```
in "with_symlinks"
  from "." "-l"
    run "." "-lL"
    run "." "-lH"
```

**Per-run modifier** (single run):

```
run "." "-v"
  in "versions"
```

Without `in`, a run executes in all contexts.

## Setup commands

Used inside `context`, `extends`, and `vary` blocks:

| Command | Effect |
|---|---|
| `file "path" "l1" "l2"` | Create file (content lines joined with \n) |
| `file "path" size N` | Create file of N bytes |
| `file "path" empty` | Create empty file |
| `file "path" from "rel/path"` | Copy from external path |
| `file "path" -> "target"` | Create symlink (target need not exist) |
| `dir "path"` | Create directory |
| `props "path" executable` | chmod +x |
| `props "path" readonly` | chmod -w |
| `props "path" mtime old` | Set mtime to 2000-01-01 |
| `props "path" mtime recent` | Set mtime to now |
| `env VAR "value"` | Set environment variable |
| `remove "path"` | Remove a file/dir/link |
| `remove env VAR` | Remove an environment variable |
| `invoke "args"` | Run the binary under test |

Content strings support escape sequences: `\n`, `\t`, `\\`, `\"`, `\xNN` (hex byte).
Parent directories are created automatically.
`from` paths are relative to the probe file's directory.
Run arguments are passed directly to the binary — no shell expansion.
`run "." "*.txt"` passes the literal string `*.txt`, not a glob.

## Run modifiers

| Modifier | Effect |
|---|---|
| `in "context"` | Scope to specific context(s) |
| `stdin "l1" "l2"` | Pipe content to stdin |
| `stdin from "file"` | Pipe file content to stdin |

## Results file

`bgrid <binary> <file.probe>` writes a `.results` file. Contains:

**Behavioral groups** — runs grouped by identical per-context observations.
Two runs are in the same group when they produce the same stdout, stderr,
exit code, and filesystem changes in every context. Singleton groups are
isolated (unique behavior). Multi-run groups are identical (equivalent or
underexplored).

**Sensitivity** — which context perturbations cause different behavior.
If "many_files / remove .hidden" is in a different group than "many_files",
the run is sensitive to that perturbation. Effect sizes are quantified.

**Universals** — properties consistent across all contexts (exit code,
stdout empty/not-empty, modifies filesystem).

**Diffs** — for runs inside `from` blocks, line-level comparison showing
what's only in this run vs the reference.

Example results:

```
# 60 runs in 37 behavioral groups

## group 1 (1 runs): "input.txt"
  exit 0 | stdout not empty | sensitive to: input.txt=size:1 (-4 lines)
  10 contexts (few_files, many_files, ...):
    stdout (5 lines):
      apple
      banana
      cherry
      date
      elderberry
    exit: 0

## group 5 (3 runs): "-s" "input.txt", "--stable" "input.txt", "-m" "input.txt"
  exit 0 | stdout not empty
  all contexts:
    stdout (5 lines):
      apple
      banana
      cherry
      date
      elderberry
    exit: 0
  vs "input.txt": identical
```

## Shared contexts

`contexts.probe` (or `setup.probe`) in the same directory as a probe file
is loaded automatically, providing shared contexts across sibling probes.
The probe file's own contexts are merged with the shared ones.

## Examples

Arguments to `run` are passed directly to the binary. Different binaries
have different argument conventions — there is no universal pattern.
Design your runs based on how the binary under test works.

### Contexts, vary, and collapsing

contexts.probe — defines input states and perturbation variants:
```
context "base"
  file "data.txt" "alpha" "beta" "gamma" "delta"
  file "extra.txt" "epsilon"
  dir "subdir"
  file "subdir/nested.txt" "deep"
  file "script.sh" "#!/bin/sh"
  props "script.sh" executable

# Each line generates one variant of "base".
# Collapsing reveals which perturbations affect each run.
vary from "base"
  remove "extra.txt"
  remove "subdir"
  file "data.txt" "single line"
  file "data.txt" empty
  file "data.txt" size 10000
  props "data.txt" mtime old
```

runs.probe — unscoped runs execute in all contexts (base + 6 variants):
```
# Arguments depend on the binary. These are examples only.
run "data.txt"

from "data.txt"
  run "data.txt" "-r"
  run "data.txt" "-n"
  run "data.txt" "-u"
```

### extends and invoke

For binaries that require complex setup (repos, databases), use `invoke`
during context construction and `extends` to derive variants:

```
context "initialized"
  invoke "init"
  invoke "config" "user.email" "test@test.com"
  invoke "config" "user.name" "Test"
  file "data.txt" "content"
  invoke "add" "."
  invoke "commit" "-m" "initial"

# extends inherits all setup including invokes.
# in "initialized" auto-includes all extends children + vary variants.
context "modified" extends "initialized"
  file "data.txt" "changed content"

context "staged" extends "initialized"
  file "data.txt" "changed content"
  invoke "add" "data.txt"

context "removed" extends "initialized"
  remove "data.txt"

vary from "initialized"
  file "data.txt" "alternate content"
  file "data.txt" empty
```

Unscoped runs test across the entire family:
```
# All contexts run every invocation. Collapsing groups contexts
# by behavior — initialized/vary-variants produce empty output,
# modified shows working tree changes, staged shows index changes.
run "status"

from "status"
  run "status" "--short"
```

### Scoping with `in`

When contexts have different file structures, scope runs to contexts
that have the right files:

```
context "base"
  file "data.txt" "alpha" "beta"

context "multi"
  file "a.txt" "alpha"
  file "b.txt" "beta"

# base and multi have different files — scope each group
in "base"
  run "data.txt"
  run "data.txt" "-n"

in "multi"
  run "a.txt" "b.txt"
```

## Patterns

### Designing for collapsing

Collapsing and sensitivity — where identical observations across contexts
are grouped and the perturbations that cause differences are identified —
are the tool's most powerful features. They activate when runs execute
across multiple varied contexts.

**Vary-centric design** enables collapsing: keep the same file names
across all contexts, vary the contents. Then all runs work across all
variants:

```
context "base"
  file "input.txt" "error: disk" "warning: low" "info: ok"

vary from "base"
  file "input.txt" "single error"
  file "input.txt" empty
  file "input.txt" size 10000

in "base"
  run "error" "input.txt"
```

`in "base"` automatically includes all vary variants of "base". The run
executes across 4 contexts. Collapsing reveals which content variations
affect grep's behavior.

**Shape-centric design** is for scenarios that need different file
structures (multi-file grep, recursive cp, stdin). Scope with `in`:

```
in "multifile"
  run "error" "a.log" "b.log"

in "stdin"
  run "error"
    stdin "error: piped" "no match"
```

The best probe files combine both: vary-centric for the main scenario
(enabling collapsing), shape-centric for edge cases.

### `in` includes the context family

`in "base"` matches context "base", all vary variants "base / ...",
and all contexts that extend "base". This means you can define a base
context, derive variants with `vary` and named contexts with `extends`,
and `in "base"` runs across the entire family:

```
context "base"
  file "source.txt" "hello"

vary from "base"
  file "source.txt" size 10000

context "special" extends "base"
  props "source.txt" executable

in "base"
  run "source.txt" "dest.txt"    # runs in base + variant + special
```

To scope to a specific context only, use its exact name.

### Use `from` for same-format comparisons only

`from` computes line-level diffs between runs. This is useful when both
outputs have the same shape — like comparing `ls "."` vs `ls "." "-a"`:

```
from "."
  run "." "-a"    # diff shows: 3 only in this: ., .., .hidden
```

Comparing across output formats (e.g., `git diff` vs `git diff --stat`)
produces noise like "16 only in ref, 3 only in this" — not meaningful.

### One probe file per behavioral question

The best probe files answer one question:
- `filtering.probe` — what does ls show and hide?
- `sorting.probe` — how does ls order output?
- `matching.probe` — how does grep match patterns?

Cramming multiple concerns into one file makes results harder to scan.

### `vary` and `invoke` contexts

`vary` perturbations are appended after all parent commands. For contexts
with `invoke` commands (git repos, databases), this means the file is
created, invokes see it, and then the perturbation overwrites it:

```
context "repo"
  file "readme.md" "hello"      # created
  invoke "add" "."              # sees readme.md
  invoke "commit" "-m" "init"   # commits it

vary from "repo"
  file "readme.md" "changed"    # appended: overwrites after commit
```

The variant has "hello" committed and "changed" in the working tree.
This is safe — invokes always see the original file.

### Fixture files for structured input

For binaries whose behavior depends on input file content (compilers,
parsers, linters, formatters), use `file from` to load real fixture
files instead of embedding escaped content inline:

```
context "base"
  file "input.json" from "fixtures/valid.json"

vary from "base"
  file "input.json" from "fixtures/malformed.json"
  file "input.json" from "fixtures/empty.json"
  file "input.json" from "fixtures/huge.json"
```

`from` paths are relative to the probe file's directory. The fixture
files are real, editable files — no escaping needed. Collapsing across
fixture variants reveals which input shapes affect the binary's behavior.

### Use `--dry-run` to inspect resolved state

`bgrid --dry-run <binary> <file>` prints resolved contexts (after
extends) and the planned run grid without executing. Useful for debugging
extends resolution and `in`-block scoping.
