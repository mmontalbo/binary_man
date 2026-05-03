# Probe Language

## Model

A `.probe` file describes a grid of **input states × invocations**. The tool
executes every cell and writes observations to a `.results` file.

The user writes `.probe` files. The tool generates `.results` files. One
command runs everything:

```
bman-probe <binary> <file-or-directory>
```

## Concepts

Five keywords: **context**, **vary**, **invoke**, **run**, **from**, **in**.

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
| `dir "path"` | Create directory |
| `link "name" -> "target"` | Create symlink (target need not exist) |
| `props "path" executable` | chmod +x |
| `props "path" readonly` | chmod -w |
| `props "path" mtime old` | Set mtime to year 2000 |
| `props "path" mtime recent` | Touch file |
| `env VAR "value"` | Set environment variable |
| `remove "path"` | Remove a file/dir/link |
| `remove env VAR` | Remove an environment variable |
| `invoke "args"` | Run the binary under test |

Content strings support escape sequences: `\n`, `\t`, `\\`, `\"`.
Parent directories are created automatically.
`from` paths are relative to the probe file's directory.

## Run modifiers

| Modifier | Effect |
|---|---|
| `in "context"` | Scope to specific context(s) |
| `stdin "l1" "l2"` | Pipe content to stdin |
| `stdin from "file"` | Pipe file content to stdin |

## Results file

The tool writes a `.results` file for each `.probe` file. Contains:

**Observations** — what the binary produced per (context × run) cell.
Identical observations across contexts are collapsed.

**Sensitivity** — which vary perturbations changed the output.

**Universals** — properties consistent across all contexts (exit code,
stdout empty/not-empty).

**Diffs** — for runs inside `from` blocks, line-level comparison showing
what's only in this run vs the reference.

Example results:

```
run "." "-a":
  3 contexts (base, base / remove backup.txt~, base / size=1000):
    stdout (7 lines):
      .
      ..
      .hidden
      backup.txt~
      subdir
      visible.txt
    exit: 0
  differs in base / remove .hidden:
    stdout (6 lines):
      .
      ..
      backup.txt~
      subdir
      visible.txt
    exit: 0
  differs in empty:
    stdout (2 lines):
      .
      ..
    exit: 0
  sensitive to: remove .hidden
  always: exit 0, stdout not empty
  from ".":
    3 only in this: . .. .hidden
    0 only in ref
    3 shared
```

## Directory structure

```
surfaces/<binary>/
  contexts.probe          # shared contexts + vary (loaded by sibling files)
  filtering.probe         # filter flag runs
  sorting.probe           # sort flag runs
  formatting.probe        # format flag runs
  errors.probe            # error cases
  filtering.results       # generated
  sorting.results         # generated
  ...
```

`contexts.probe` (or `setup.probe`) is loaded automatically by all sibling
`.probe` files in the same directory.

## Examples

### ls

contexts.probe:
```
context "base"
  file "visible.txt" "hello"
  file ".hidden" "secret"
  dir "subdir"
  file "subdir/nested.txt" "deep"
  link "mylink" -> "visible.txt"
  file "script.sh" "#!/bin/sh"
  props "script.sh" executable
  file "backup.txt~" "old"

vary from "base"
  remove ".hidden"
  remove "backup.txt~"
  remove "mylink"
  file "visible.txt" size 1000
  props "visible.txt" mtime old

context "empty"

context "sorts"
  file "nnn.txt" "ab"
  props "nnn.txt" mtime old
  file "bbb.txt" size 1000
  file "hhh.txt" "abcdefghij"
  props "hhh.txt" mtime recent
```

filtering.probe:
```
run "."

from "."
  run "." "-a"
  run "." "-A"
  run "." "-B"
  run "." "-d"
  run "." "-R"

run "nonexistent"
```

sorting.probe:
```
in "sorts"
  run "."

  from "."
    run "." "-r"
    run "." "-S"
    run "." "-t"
```

### grep

```
context "base"
  file "log.txt" "error: disk full" "warning: low" "ERROR: timeout" "info: ok"

run "error" "log.txt"

from "error" "log.txt"
  run "-i" "error" "log.txt"
  run "-v" "error" "log.txt"
  run "-c" "error" "log.txt"
  run "-n" "error" "log.txt"

run "error"
  stdin "Error: piped" "no match" "error: found"

run "nomatch" "log.txt"
```

### git

```
context "clean"
  invoke "init"
  invoke "config" "user.email" "test@test.com"
  invoke "config" "user.name" "Test"
  file "readme.md" "hello"
  invoke "add" "."
  invoke "commit" "-m" "initial"

context "dirty" extends "clean"
  file "untracked.txt" "new"

run "status"

from "status"
  run "status" "--short"

run "diff"

from "diff"
  run "diff" "--stat"
  run "diff" "--cached"
```

### cp

```
context "base"
  file "source.txt" "hello world"
  dir "destdir"

run "source.txt" "copy.txt"
run "source.txt" "destdir/"
run "missing.txt" "copy.txt"
```
