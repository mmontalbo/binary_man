# Probe — Design Principles

## Purpose

Describe an execution context, invoke a binary, predict what happens, record what actually happens. One file is both the test and the report. A directory of files is a corpus of behavioral knowledge about one binary.

## Core concepts

**Execution context.** The complete input environment a binary sees: filesystem state, environment variables, stdin content. Described declaratively — what should exist, not how to create it. Multiple named contexts per file enable testing the same behavior across different configurations.

**Invocations.** Arguments passed to the binary in a given context. Every invocation is a peer — no privileged "control." Multiple invocations enable bilateral comparison via `vs` references.

**Expectations.** Predictions about output: stdout, stderr, exit code, filesystem post-state. Either standalone (`contains "text"`, `exit 0`) or relational (`superset vs "."` — comparing this invocation's output to another's).

**Observations.** Tool-generated annotations recording what the binary actually produced. Interleaved with expectations as `#>` lines. Stripped and regenerated on each run. The annotated file is the complete record.

**Surfaces.** A behavioral surface is the unit of testing — one file per surface, grouped in a directory per binary. The tool operates on the whole directory, annotating files and generating a status summary.

## What the language does

- Declares execution contexts (filesystem, environment, stdin)
- Declares invocations (arguments, context scope)
- Expresses predictions as structural properties of output
- Records observations alongside predictions
- Supports context composition (`extends`, `remove`)
- Supports context bootstrapping via `invoke` (using the binary under test to set up its own state)

## What the language does not do

**No binary-specific knowledge.** No hardcoded flag lists, no command-specific predicates. The tool derives the flag list from sibling test files in the surface directory.

**No arbitrary shell commands.** No `run` escape hatch. Context setup uses declarative primitives (`file`, `dir`, `link`, `props`, `env`) and `invoke` (which runs the binary under test, not arbitrary commands). Pre-built state can be referenced with `from`.

**No control flow.** No conditionals, loops, or branching. A test file is a flat structure: contexts, then test blocks with expectations.

**No expected output literals.** Predictions describe properties (`superset`, `reordered`, `contains`), not exact output strings. The tool captures actual output as observations.

**No implementation access.** The binary is a black box. Only observable inputs and outputs.

## Design constraints

**Readable without documentation.** `expect stdout contains ".hidden"` and `context "base" extends "repo"` should be understandable to anyone.

**Binary-agnostic.** The same language tests ls, grep, sort, cp, git, sqlite3. Input primitives cover the common channels (filesystem, stdin, environment). Output predicates cover the common channels (stdout, stderr, exit code, filesystem post-state). The binary determines which channels matter.

**Predictions are relationships, not values.** `superset vs "."` holds regardless of filenames or file count. Instance-level predicates (`contains ".hidden"`, `line 1 contains "gamma.txt"`) are supported but are context-specific by nature. The tool's cross-context reporting distinguishes properties (hold everywhere) from context-specific observations (hold in some contexts).

**One file = one behavioral surface.** A surface groups invocations that share contexts and compare against each other. A directory of surface files covers one binary.

**The tool owns the directory.** It reads all files, runs all tests, annotates every file, and generates a `_status.md` summarizing the corpus state — what's tested, what's stubbed, what's untested, what's confused, and what to work on next.

## Execution model

**Contexts reset per invocation.** Each test block gets a fresh instance of its context. Earlier invocations don't affect later ones. This is essential for filesystem-modifying binaries — `cp` creating a file in one test block doesn't affect the next.

**All test blocks run in all contexts by default.** An expectation that passes across all contexts is a property. One that fails in some contexts reveals a precondition. The `in` clause scopes a test block to specific contexts when it only makes sense there.

**Cross-file confusion checking.** The tool scans sibling test files to discover what other flags/invocations exist. For each test block, it checks whether substitute invocations (from other surface files) also pass all expectations. The confusion list reveals which surfaces the test can't distinguish.

## Directory structure

```
surfaces/<binary>/
  _status.md              # generated: corpus summary, gaps, suggestions
  _bootstrap.test         # help text, default output, error cases
  setup.test              # shared contexts (loaded automatically)
  <surface>.test           # one file per behavioral surface
```

The tool operates on the directory: `bman-probe <binary> surfaces/<binary>/`

`_status.md` is regenerated each run. `_bootstrap.test` captures discovery observations. `setup.test` provides shared contexts that all surface files inherit. Surface files contain invocations and expectations for one behavioral unit.

## Bootstrapping

1. Run `--help` and default invocation. Record observations in `_bootstrap.test`.
2. Parse help text to discover flags/subcommands. List untested ones in `_status.md`.
3. For each surface, write a test file — either manually, by LM, or as a stub with observations only.
4. Run the directory. Observations are annotated, status is updated.
5. Revise expectations based on observations. Add contexts to test generality.
6. Repeat until properties stabilize and confusion is resolved.

For binaries that need complex state (git repos, databases), use `invoke` to bootstrap context from the binary's own verified operations.

## Input channels

| Channel | Syntax | Scope |
|---|---|---|
| Filesystem | `file`, `dir`, `link`, `props`, `from` | context |
| Environment | `env VAR "value"` | context or per-invocation |
| Stdin | `stdin "line1" "line2"` or `stdin from "file"` | per-invocation |
| Arguments | `test args "arg1" "arg2"` | per-invocation |
| Self-setup | `invoke "arg1" "arg2"` | context (runs binary under test) |

## Output channels

| Channel | Predicate examples |
|---|---|
| Stdout | `contains`, `superset vs`, `reordered vs`, `line N contains`, `before` |
| Stderr | `empty`, `not-empty`, `contains`, `unchanged vs` |
| Exit code | `N`, `unchanged vs`, `changed vs` |
| Filesystem | `file "path" exists`, `file "path" contains "text"` |
