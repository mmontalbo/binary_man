# Probe — Work in Progress

## What exists

### Tool (Rust, 1272 lines)

```
tools/probe/src/
  parse.rs     ~590 lines  Language parser
  main.rs      ~350 lines  Grid orchestration, results formatting, diffs
  execute.rs   ~260 lines  Grid execution, fs snapshotting
  sandbox.rs   ~147 lines  Filesystem setup from commands
```

Binary: `bman-probe <binary> <file-or-directory>`

### Implemented features

- **context** with extends and remove
- **vary** with command-list filtering (each line = one variant)
- **invoke** for binary self-setup (git init, sqlite3 CREATE TABLE, etc.)
- **run** for observation invocations
- **from** blocks for diff comparison grouping
- **stdin** per invocation (lines or from file)
- **Filesystem observation** — snapshot before/after each invocation, detect created/deleted/modified files (content hash + permission mode)
- **Context collapsing** — group identical observations across contexts
- **Sensitivity summary** — which vary perturbations changed each run's output
- **Universals** — properties consistent across all contexts (exit code, stdout empty/not-empty)
- **Auto-computed diffs** — for from-block members, line-level comparison vs reference
- **Two-file model** — .probe (user) → .results (tool)
- **setup.probe/contexts.probe** auto-loading for shared contexts

### NOT yet implemented

- **`in` as block-level keyword** — currently only works as per-run modifier. Design doc says it should be block-level and composable with `from`. Haiku blind test confirmed this is the natural syntax.
- **`from` for files** — `file "path" from "fixture/path"` is parsed but untested with real binary fixtures.
- **`remove env VAR`** — parsed but untested.
- **Per-run `env`** — not implemented. env is context-level only.
- **JSON export** — designed but not built. Would enable jq queries over observation data.
- **Directory mode** — tool accepts a single file. Directory scanning (run all .probe files, aggregate results) not implemented.

## What was validated

### Binaries tested (8)

| Binary | Patterns exercised |
|---|---|
| ls | Filesystem perturbation, flag sensitivity, collapsing, 34 flags |
| grep | Content perturbation, stdin, multifile, recursive, 18 flags |
| git diff | invoke for repo setup, extends for state variants, staged vs unstaged |
| sort | Stdin transformation, field delimiters, validation mode, month sort |
| find | Complex predicate arguments, depth control, type filtering |
| cp | Filesystem side effects (created files observed) |
| mv | Created + deleted in same observation |
| sed | Stdout transformation AND in-place file modification (-i) |
| chmod | Permission changes detected via mode tracking |

### Blind test with Haiku

Gave Haiku the language spec and asked it to write comprehensive ls probe files from scratch. Results:

- **Correct:** context design, vary usage, from blocks, flag coverage, error cases, file organization
- **Wrong:** used `in` as a block (our syntax required it as a per-run modifier)
- **Lesson:** `in` should be block-level. Haiku's instinct was the better design. Updated the spec.

## Language design (current)

Five keywords: **context**, **vary**, **invoke**, **run**, **from**.
Plus **in** (block or modifier) and **extends**/**remove** for context derivation.

```
context "base"
  file "visible.txt" "hello"
  file ".hidden" "secret"

vary from "base"
  remove ".hidden"

context "empty"

run "."

from "."
  run "." "-a"
  run "." "-B"

in "sorts"
  from "."
    run "." "-r"
    run "." "-S"

run "nonexistent"
```

User writes `.probe`. Tool generates `.results` with:
- Observations (stdout, stderr, exit, fs changes)
- Context collapsing (identical outputs grouped)
- Sensitivity (which perturbations matter)
- Universals (always-true properties)
- Diffs (from-block members vs reference)

## Architecture decisions

### Three layers

1. **Layer 1 (user):** Execution contexts + invocations. The .probe file.
2. **Layer 2 (tool):** Raw observations. The .results file.
3. **Layer 2.5 (tool):** Computed summaries — diffs, sensitivity, universals. Also in .results.
4. **Layer 3 (external):** Analysis — documentation generation, regression detection, clustering. Done by humans, LMs, or scripts reading the results. NOT part of the tool.

### invoke vs run

Same operation (execute the binary) but different semantics. invoke builds state (output discarded, inside context blocks). run observes behavior (output recorded, outside context blocks). Attempted unification with single `run` keyword caused parsing ambiguity. Reverted to separate keywords.

### from blocks

Explicit comparison relationships. No assumed baseline. No heuristic. The user declares which runs to compare against which reference. Runs outside from blocks are standalone — observed but not diffed.

### in blocks

Block-level scoping. Groups runs and from blocks under a context scope. Composes with from by nesting. Also usable as per-run modifier for one-off scoping.

### No expectations/assertions

Layer 3 is external. The tool records and summarizes. It doesn't judge. Assertions, documentation, regression checks are built on top of the observation data by external consumers.

### No query language

The observation data in .results is structured text. For programmatic access, JSON export (not yet built) enables jq queries. The tool doesn't provide its own query language.

## What's next

### Implementation priorities

1. **`in` as block-level keyword** — update parser to handle in-blocks containing runs and from-blocks. Highest priority: validated by Haiku test.

2. **Implement `in` + `from` nesting** — `in "ctx" from "." run "." "-a"` should work.

3. **Directory mode** — `bman-probe ls surfaces/ls/` runs all .probe files, shared contexts loaded from contexts.probe.

4. **JSON export** — `bman-probe export <file>` outputs structured JSON for programmatic queries.

5. **Re-run Haiku blind test** with `in`-as-block support to validate the fix.

### Design questions (open)

- Should per-run `env` be supported (like `stdin`)?
- Should the tool detect and warn about stale .results files?
- How should cross-file analysis work (clustering, coverage)?
- What's the right format for JSON export?

### Use cases to validate

- **Documentation generation:** LM reads .results files for a binary, generates man page. Tested conceptually, not end-to-end.
- **Regression detection:** diff .results across binary versions. Not implemented.
- **Behavioral clustering:** group flags by delta type across a directory. Implemented in old discover command, removed in rewrite.

## Commit history (structured-delta branch)

Key commits in this session:

- `781bd1d` — initial bman-probe crate with 3 ls test surfaces
- `e57e5ac` — comprehensive ls surfaces with discrimination and specificity
- `a18d727` — remove superfluous predicates, add LANGUAGE.md
- `b8a3f6f` — remove control concept, symmetric peer discrimination
- `a442c86` — init subcommand, observation-only blocks, append-style results
- `201c136` — discover subcommand for documentation-free behavioral discovery
- `a3878ac` — fresh layer 1+2 rewrite (grid executor with observations)
- `22f8b3d` — filesystem observation (snapshot before/after)
- `4bc0817` — content and permission change detection
- `b625a33` — remove dead code (delta.rs, validate.rs)
- `f7c209d` — from blocks, two-file model, auto-diffs, universals
- `998b0ce` — update LANGUAGE.md and DESIGN.md (in-as-block from Haiku test)
