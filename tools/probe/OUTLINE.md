# Probe — Design Outline

## 1. What We Built

A tool for behavioral testing of CLI binaries. Given a binary, the tool:
- Discovers its behavioral surface area by probing (no documentation required)
- Generates test stubs with mechanically suggested expectations
- Runs tests across multiple execution contexts
- Reports which expectations are properties (hold everywhere) vs context-dependent
- Annotates test files with observations and results

### Current Implementation

```
bman-probe discover <binary> <dir>    # probe flags, classify deltas, cluster
bman-probe init <binary> <dir>        # parse --help, generate stubs
bman-probe <binary> <test-file>       # run tests, append results
```

### Test Language (current)

```
# Named execution contexts
context "base"
  file "visible.txt" "hello"
  file ".hidden" "secret"
  dir "subdir"

context "empty"

context "with backup" extends "base"
  file "backup~" "old"

context "no hidden" extends "base"
  remove ".hidden"

# Test blocks — run in all contexts by default
test args "."
  expect exit 0

# Scoped to specific contexts
test args "." "-a"
  in "base"
  expect stdout superset vs "."
  expect stdout contains ".hidden"
  expect exit 0
```

### Setup Commands

| Command | Effect |
|---|---|
| `file "path" "content"` | Create file with content (lines joined with \n) |
| `file "path" size N` | Create file filled to N bytes |
| `file "path" empty` / `file "path"` | Create empty file |
| `dir "path"` | Create directory |
| `link "name" -> "target"` | Create symlink (target need not exist) |
| `props "path" executable` | chmod +x |
| `props "path" readonly` | chmod -w |
| `props "path" mtime old` | Set mtime to year 2000 |
| `props "path" mtime recent` | Touch file |
| `env VAR "value"` | Set environment variable |
| `remove "path"` | Remove path inherited from extends |
| `invoke "arg1" "arg2"` | Run binary under test during setup (planned) |

### Expect Predicates

**Stdout — structural (vs another invocation):**

| Syntax | Meaning |
|---|---|
| `superset vs "args"` | Contains all reference lines plus more |
| `subset vs "args"` | Contains only a subset of reference lines |
| `reordered vs "args"` | Same lines, different order |
| `preserved vs "args"` | Entry names present, format may differ |

**Stdout — content:**

| Syntax | Meaning |
|---|---|
| `empty` / `not-empty` | Stdout presence |
| `contains "text"` / `not-contains "text"` | Substring check |
| `every-line-matches "regex"` | Regex on all non-empty lines |
| `line N contains "text"` | Positional substring check |
| `"X" before "Y"` | Ordering check |
| `lines exactly N` | Line count |
| `lines same/more/fewer than "args"` | Relative line count |

**Stderr:** `empty`, `not-empty`, `contains "text"`, `unchanged vs "args"`

**Exit:** `N`, `unchanged vs "args"`, `changed vs "args"`

### Results Format

Tool appends `#>` results block to the test file on each run (stripped and regenerated):

```
#> --- results ---
#> test [".", "-a"] in base:
#>   stdout (7 lines):
#>     .
#>     ..
#>     .hidden
#>     visible.txt
#>   exit: 0
#>   passed: expected superset, got Superset
#> suggested (from base):
#>   expect stdout superset vs "." — all 2 contexts
#>   expect stdout contains ".hidden" — holds in: base (fails in: empty)
#>   expect exit 0 — all 2 contexts
```

### Directory Structure

```
surfaces/<binary>/
  _bootstrap.test     # discovery observations
  _clusters.md        # behavioral clustering from discover
  setup.test          # shared contexts (auto-loaded by sibling files)
  -a.test             # one file per behavioral surface
  -r.test
  ...
```

---

## 2. Quality Measurement

### Discrimination

Each check is evaluated against all other invocations in the script. A check discriminates if it fails for at least one other invocation — it says something specific to THIS invocation.

### Cross-Flag Confusion

For each test block, the tool runs substitute flags (from sibling test files) and checks if all expectations still pass. Flags that pass all checks are "confused with" the tested flag.

### What We Removed

- **Information-theoretic bits**: measured identification (which flag?) not verification (correct behavior?). Never drove a decision. Removed.
- **Preserved subtypes** (prefix-added, fields-expanded, wrapped): one use case each. Broader `preserved` covers all. Removed.
- **Fs predicates**: parsed but unimplemented. Dead code. Removed.
- **Identical, complement, collapsed predicates**: never successfully used in any test. Removed.
- **COMMON_FLAGS**: hardcoded ls flags. Replaced by sibling file scanning (planned) and discover.
- **Privileged control**: first test block was special. Replaced by symmetric peer comparison.

---

## 3. Conceptual Frameworks Explored

### Metamorphic Testing (most relevant)
Our relational predicates ARE metamorphic relations. `superset vs`, `reordered vs`, `preserved vs` describe relationships between outputs of different invocations — the core technique for testing without an oracle.

### Mutation Testing
Discrimination IS mutation testing. The "mutant" is the binary run without the flag. A check that passes on the mutant (control) = surviving mutant = non-discriminating check.

### Confusion Matrices
The cross-flag check IS a confusion matrix. Which flags can the test suite distinguish? Directly drove improvements across 4 rounds.

### Daikon / Specification Mining
Our predict-observe-revise loop is manual Daikon. The suggestion generator automates the "observe and hypothesize" step — run two invocations, classify the delta, emit candidate invariants.

### Property-Based Testing
Our relational predicates are properties — they hold across sandbox variations. Instance-level predicates (`contains ".hidden"`, `line 1 contains "gamma.txt"`) are examples, not properties. The distinction: properties generalize across contexts, examples don't.

### Equivalence Classes
A bet that "these inputs all produce the same behavior." The bet can be wrong. Our multi-context execution tests this — an expectation that holds in all contexts is a confirmed equivalence class.

### What Didn't Work
- **Information theory**: measured the wrong thing (identification vs verification). Per-check bits detected redundancy but the total "X/5.0 bits" was not actionable.
- **Version space learning**: theoretically elegant, never used mechanically.

---

## 4. Key Design Decisions

### Execution Context Replaces Sandbox
"Sandbox" is an implementation detail. "Execution context" is what the binary sees: filesystem state + environment + stdin + arguments. The language describes contexts, not sandboxes.

### No Privileged Control
All invocations are peers. Discrimination checks against all other invocations, not a special "first" block. Any invocation can reference any other via `vs`.

### Predictions Are Relationships, Not Values
`superset vs "."` holds regardless of filenames. `contains ".hidden"` only holds in contexts with a hidden file. The language supports both, but relational predicates are the higher-value construct.

### No Arbitrary Shell Commands
No `run` escape hatch. Context setup uses declarative primitives + `invoke` (runs the binary under test). Pre-built state via `from "path"`. The binary bootstraps its own context.

### The File Is Both Test and Report
One artifact. Predictions + observations + results. The tool annotates the file with `#>` lines on each run. Stripped and regenerated. Git tracks the revision history.

### Convention-Based Directory Organization
`setup.test` auto-loaded. One file per behavioral surface. `_status.md` generated. The tool suggests reorganization but never performs it.

---

## 5. The Discovery Pipeline

### Phase 1: Bootstrap (what `discover` does)

Given just a binary name:

1. Create a basic sandbox (files, dirs, hidden files, symlinks)
2. Run baseline: `binary "."` → observe stdout/stderr/exit
3. Sweep all single-char flags: `-a` through `-z`, `-A` through `-Z`
4. For each flag: classify delta vs baseline (Superset/Subset/Reordered/Preserved/Identical)
5. Cluster flags by delta type, with set-overlap refinement within clusters
6. Write stubs + clustering summary

Output: a directory of annotated stubs organized by behavioral cluster. Zero documentation read.

### Phase 2: Suggestions (what `run` does for observation-only blocks)

For each stub (test block with no expectations):

1. Find the baseline invocation (shortest args)
2. Compare option output against baseline using delta classifier
3. Generate suggested expectations: structural predicate, content additions/removals, line count, exit code
4. Validate each suggestion across all contexts
5. Annotate: "expect stdout superset vs '.' — all 5 contexts" or "holds in: base (fails in: empty)"

### Phase 3: Acceptance (human or LM)

Read the annotated file. Accept good suggestions, drop bad ones, add manual refinements (sort-order pins, format regexes, context scoping). Run again to verify.

---

## 6. Surface Area Discovery

### What Is a Surface?

A surface is a dimension of behavioral variation. Defined by behavioral independence: two input variations are the same surface if they interact (changing one modifies the other's effect). Different surfaces are independent (effects compose without interference).

### Mechanical Detection

The delta classifier detects surface type:
- Superset flags → filtering (additive) surface
- Subset flags → filtering (subtractive) surface
- Reordered flags → ordering surface
- Preserved flags → formatting surface
- Identical flags → no observable effect (may need different context)

Set-overlap refinement within clusters: flags that add different entries are independent sub-surfaces.

### Surface Boundaries

Tested by pairwise composition: run flag A alone, flag B alone, A+B together. If A+B = compose(A, B), they're independent (different surfaces). If not, they interact (same surface or cross-surface link).

---

## 7. Perturbation-Based Exploration (proposed)

### Core Insight

The flag sweep IS perturbation on the args dimension. Filesystem mutations are perturbation on the filesystem dimension. All discovery is perturbation: change one thing, observe what changes.

### Unified Framework

```
baseline = run(binary, default_args, default_fs, default_stdin, default_env)

for each input dimension:
  for each perturbation on that dimension:
    perturbed = run(binary, ..., perturbed_dimension, ...)
    delta(baseline, perturbed) → sensitivity on that dimension
```

Every input channel (args, filesystem, stdin, environment) is explored with the same pattern.

### The Perturbation Vocabulary

**Args:** single-char flag sweep, common long flags, value arguments

**Filesystem:**
- Add/remove files
- Toggle hidden prefix (`.`)
- Toggle backup suffix (`~`)
- Vary file sizes
- Vary timestamps
- Add/remove subdirectories
- Add/remove symlinks
- Change permissions

**Stdin:** empty, text content, binary content

**Environment:** LANG, TERM, LC_ALL, HOME, binary-specific vars

Each perturbation type is binary-agnostic — every CLI tool operates on these primitives.

### Hierarchical Screening

The space of all perturbations is large but most are irrelevant. Screen hierarchically:

```
Phase 1: Channel screening (~4 runs)
  Does the binary use args? stdin? filesystem? env?
  → Eliminate inactive channels entirely

Phase 2: Dimension screening (~10 runs)
  Within active channels, one perturbation per dimension
  → Eliminate insensitive dimensions

Phase 3: Flag sweep (~60 runs)
  Probe all flags in a sandbox informed by phases 1-2
  → Discover active flags and their delta types

Phase 4: Cross-reference, sampled (~30 runs)
  For each active flag × each sensitive dimension
  → Discover which flags care about which dimensions
```

Total: ~100 runs. Each phase eliminates most of the space below it.

### What This Produces

A sensitivity map: for each flag, which input dimensions affect its behavior.

```
flag -a: sensitive to hidden files. insensitive to sizes, timestamps.
flag -S: sensitive to file sizes. insensitive to hidden files.
flag -t: sensitive to timestamps. insensitive to sizes.
flag -B: sensitive to backup suffix. insensitive to most other dimensions.
```

This map tells you exactly which contexts to generate for each flag. No hand-crafting, no domain knowledge — the binary revealed its own structure through perturbation.

### From Sensitivity Map to Contexts

For each sensitive dimension, generate a context that varies it:
- Hidden-sensitive flags get contexts with 0, 1, 5 hidden files
- Size-sensitive flags get contexts with decorrelated sizes
- Time-sensitive flags get contexts with decorrelated timestamps
- Backup-sensitive flags get contexts with/without backup files

The contexts are targeted — each exercises the dimensions that actually matter for the flags it tests.

---

## 8. Remaining Gaps

### Not Yet Implemented
- `invoke` (binary self-bootstrapping for complex contexts)
- `stdin` per invocation
- `from "path"` (external file references)
- `expect file "path" exists/contains` (post-execution filesystem checks)
- Per-invocation `env`
- Directory mode (run all files, generate _status.md)
- Sibling-file confusion checking (replacing COMMON_FLAGS)
- The perturbation-based exploration phases 1-4

### Open Questions
- How to handle side-effect binaries (cp, mv) where stdout is empty and the effect is on the filesystem
- How to test terminal-mode behavior (column layout flags invisible in pipe mode)
- How to express property-level predicates (`sorted-by "name-reverse"`) vs instance-level (`line 1 contains "gamma.txt"`)
- How to measure meaningfulness (do the tests verify behavior that matters, not just behavior that changed?)
- How to handle binaries with subcommands (git) or complex argument structures (ffmpeg)

### What Works Well Now
- Discover → stubs → suggestions → accept → run cycle
- Multi-context execution with property/context-dependent categorization
- Delta classifier driving structural suggestions
- Cross-context validation annotating each suggestion with where it holds
- Append-style results persisting evidence in the test file
- Setup.test auto-loading for shared contexts
- Backward compatibility with flat (no context) test files
