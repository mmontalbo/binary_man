# Experiment Design

## Problem

Given an arbitrary CLI binary, mechanically characterize what each flag does by observing behavior differences across varied input conditions — without source code, documentation, or knowledge of the binary's domain.

## Model

The binary is a black box:

```
f(flags, positional_args, filesystem_state, env) → (stdout, stderr, exit_code, fs_changes)
```

A **cell** is one (context, run) pair — a specific invocation in a specific input state. The **grid** is contexts × runs. Every cell produces one **observation**: the recorded (stdout, stderr, exit_code, fs_changes).

A **behavioral group** is a set of runs that produce identical observations across ALL contexts. Runs in the same group are indistinguishable given the tested conditions.

A flag has **observed behavior** when at least one run containing it exits 0 with non-trivial output or filesystem changes — the tool saw the flag work, not just fail distinctively.

## Worked Example: `head`

### Discovery

`head --help` reveals 7 flags: `-c`, `-n`, `-q`, `-v`, `-z`, plus long forms `--bytes`, `--lines`, `--quiet`, `--silent`, `--verbose`, `--zero-terminated`.

Behavioral probing finds three working invocation patterns:
- `head` (reads stdin)
- `head input.txt` (single file)
- `head input.txt other.txt` (multi-file, shows headers)

Stdin probing confirms `head` accepts piped input. Value probing finds working arguments for `-c` and `-n`: metavar-based candidates (`1`, `0`, `2`, `10`, `100`) are tried in order; the first success (`1`) becomes the combo value, and all successes generate independent solo runs.

### Grid Execution

The tool generates ~178 runs (7 short flags + 6 long flags × 3 patterns + stdin variants + boundary values + 21 pairwise combinations) across 26 contexts = ~4,628 cells. Executed in ~0.6s via batched bwrap sandboxing.

### Context and Observations

Consider `head -v input.txt` across three contexts:

**Context: alpha_minimal** (input.txt = `cherry\nApple\nbanana\n...`, 7 lines)
```
base (head input.txt):     cherry\nApple\nbanana\nDate\nelderberry\nBANANA\napple
flag (head -v input.txt):  ==> input.txt <==\ncherry\nApple\nbanana\nDate\nelderberry\nBANANA\napple
```
Delta from base: +1 line (`==> input.txt <==` header prepended)

**Context: numeric_minimal** (input.txt = `100\n2\n30\n...`, 16 lines)
```
base: 100\n2\n30\n1\n20\n3\n10\n50\n8\n200
flag: ==> input.txt <==\n100\n2\n30\n1\n20\n3\n10\n50\n8\n200
```
Delta from base: +1 line (same header prepended). Same transformation, different content.

**Context: input.txt=empty** (input.txt = empty)
```
base: (empty)
flag: ==> input.txt <==
```
Delta from base: +1 line. Sensitivity detected: output went from 10 lines to 1 line.

The delta key for `-v` across all contexts encodes: "prepends `==> input.txt <==` header line." This is the same transformation in every context, making `-v` isolated in its own behavioral group.

### Grouping

The grid produces ~37 behavioral groups from ~178 runs:
- **4 isolated groups** (solo-distinguished flags): `-v`, `-z`, `-c`, `-n`
- **~33 identical groups** containing runs that produced the same transformation

The 3 remaining flags (`-q`, `--quiet`, `--silent`) all suppress the multi-file header — they produce the same delta as the bare `head` base in multi-file contexts. They're in one identical group, but pairwise combinations (e.g., `head --lines --silent`) provide interaction evidence that distinguishes them.

### Pairwise Combinations

All flag pairs are tested in the same grid: `head --lines --silent input.txt other.txt` vs `head --lines input.txt other.txt`, etc. The `--silent` flag removes the header that `--lines` preserves → different output → `-q`/`--quiet`/`--silent` are distinguished via combination evidence.

Result: **7/7 flags observed** (4 solo + 3 via combination). The `-q`/`--silent` pair is correctly identified as behavioral aliases (same structural transformation in every context). Aliases detected: `-c = --bytes`, `-n = --lines`, `-v = --verbose`, `-z = --zero-terminated`.

## Context Design

5 content levels × 3 structure levels with cycling property modifiers:

```
              minimal         standard              deep
alpha         default         varied-perms          varied-times
numeric       varied-times    default               varied-perms
fielded       varied-perms    varied-times          default
formatted     default         varied-times          varied-perms
tabular       varied-times    varied-perms          default
```

**Content levels** (what's in input.txt):
- **alpha**: mixed-case words (cherry, Apple, banana, Date, ...)
- **numeric**: 16 integers, longer than `head -n 10`
- **fielded**: colon-delimited records (bob:30:sales, ...)
- **formatted**: tabs, blank lines, trailing whitespace, control characters
- **tabular**: tab-delimited fields, duplicates, long line >80 chars

**Structure levels** (what files exist):
- **minimal**: input.txt + other.txt
- **standard**: + hidden file, subdir, symlink, executable, readonly
- **deep**: + 3-level nesting, directory symlink

**Property levels** (file metadata):
- **default**: no special properties
- **varied-perms**: readonly file, flag-like filename (`-rf`)
- **varied-times**: old mtime, large file (10KB)

Plus 10 single-factor perturbations from numeric_standard, a locale perturbation on alpha_minimal, and 3 stdin contexts (words, numbers, passwd with varied delimiters). Total: ~35 contexts per grid.

## Factor Identification and Level Determination

The experiment has three factors:
- **Flag** (treatment): which flag is applied. Levels = all flags from `--help`.
- **Value** (nested within flag): what argument value the flag gets. Levels determined by pilot study.
- **Context** (blocking): input content, filesystem structure, environment. Levels = fixed fixture corpus.

### Pilot study (factor level determination)

Before the main experiment, a sequential pilot study determines working factor levels:

- **Invocation patterns**: 7 positional arg candidates + structural patterns from Usage line (`COMMAND → echo`, `[expression] → -name/-type`). Working patterns become run templates.
- **Stdin**: piped content tested. If accepted, stdin contexts provide input alongside file contexts.
- **Flag values**: multi-source candidate discovery per flag:
  1. *Help text mining* — quoted values, brace enumerations, bracket character sets, continuation lines.
  2. *Metavar candidates* — per-type curated lists (NUM → `1,0,2,10,100`; FILE → `input.txt`; etc.).
  3. *Error mining* — fires when no candidate exits 0; parses "Valid arguments are:" from stderr.
  4. *Companion probing* — failing flags tried with each working flag as companion.
  5. *Mutual compound probing* — pairs of both-failing flags tried together (discovers co-dependencies).
  First working value = combo value (stable). All working values generate independent solo runs (additive).
- **Alias propagation**: short flags inherit metavar from long alias for proper value probing.

The pilot is adaptive (later probes depend on earlier results). The main experiment is fixed.

## Delta Grouping

For runs with a `from` reference (base invocation), comparison uses **structural deltas** — what transformation the flag applied to the base output — rather than the raw output content. This groups flags by the structural change they produce ("prepended 8 tokens per line", "reversed line order", "inserted a header line") rather than by specific output values.

The structural delta is computed via hash-anchored alignment:

1. **Tokenize**: split stdout into lines, split lines by whitespace.
2. **Hash-anchor matching**: hash each line, find exact-match anchors between ref and obs in O(n). Shared lines (filenames, keywords) are natural anchors.
3. **Gap alignment**: between anchors, run Needleman-Wunsch on the small unmatched segments. Match cost = token edit distance within line pairs. Gap cap at 100 lines for unanchored segments.
4. **Token-level alignment**: within matched line pairs, classify each token as Keep, Insert, Delete, or Replace.

For outputs with shared lines (90%+ for most tools), alignment is O(n). For completely different outputs (e.g., diff normal vs unified format), the disjoint hash sets trigger an early exit — no anchor search needed.

Runs with identical edit scripts in every context form a behavioral group. This is the equivalence relation: two flags are indistinguishable iff no tested context separates their structural transformation.

All runs — single-flag AND pairwise combinations — are tested in a single phase. No iterative refinement. The experimental design is fixed before execution, eliminating path-dependence (where intermediate results could influence which experiments are generated next).

## Execution

Cells are batched by context. Per context: create per-cell workspace directories, generate one shell script with all commands (each with 2-second timeout), invoke bwrap once. ~35 bwrap invocations instead of thousands. 16 threads across contexts.

## Quality metrics

- **Robustness**: leave-one-out context removal (sampled 10 contexts). Flags that survive all removals are robust; flags dependent on a single context are fragile.
- **Reproducibility**: opt-in cross-run verification (REPRO=1). Re-runs all binaries and compares observed counts. Nondeterministic binaries reported but don't fail the test.
- **Surface stability**: exact flag count checked against expected total. Changes in flag discovery surface (regex shifts) are caught.

## Limitations

- **Error-only flags**: flags that need specific content types (month names for `sort -M`, version strings for `sort -V`) or specific argument values to produce non-error output remain unobserved. These are at the boundary of binary-agnostic exploration.
- **Modifier flags**: flags like `-h` that only modify another flag's output are distinguished via pairwise combination testing, which provides interaction evidence.
- **Timing-dependent flags**: `cp -u` (copy only if newer) is nondeterministic because source and destination files are created within the same second. Filesystem snapshot-based change detection also has mtime race conditions.
- **No semantic interpretation**: the tool reports *that* flags differ, not *why*. The structural edit script vocabulary (Insert/Delete/Keep/Replace) provides structural context but no domain semantics.
- **Stateful binaries**: tools requiring prerequisite state (git repositories) need manual setup.
