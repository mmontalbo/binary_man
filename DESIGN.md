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

Stdin probing confirms `head` accepts piped input. Value probing confirms `-c` and `-n` need numeric arguments (default value `1` works).

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

Plus 10 single-factor perturbations from numeric_standard and a locale perturbation on alpha_minimal. Total: ~27 contexts per grid.

## Behavioral Discovery

Help text provides flag candidates. Behavioral probing determines which invocations work:

- **Arg patterns**: 7 candidates (no args, file, directory, two files, file+dir, pattern+file, pattern+dir) tested against the binary. Working patterns become run targets.
- **Stdin**: binary tested with piped content. If accepted, stdin runs generated for each pattern × flag.
- **Values**: flags with value hints (NUM, FILE, CHAR, etc.) get default `1`. If that fails, candidates (`auto`, `,`, `input.txt`, `.`, `0`) are tried. All candidates are tested; the one whose output differs most from the unflagged baseline is selected.
- **Subcommands**: common verbs probed as first positional arg. Classified as working, state-building, or needs-state.

## Delta Grouping

For runs with a `from` reference (base invocation), comparison uses **structural deltas** — what transformation the flag applied to the base output — rather than the raw output content. This groups flags by the structural change they produce ("prepended 8 tokens per line", "reversed line order", "inserted a header line") rather than by specific output values.

The structural delta is computed via two-level Needleman-Wunsch alignment:

1. **Tokenize**: split stdout into lines, split lines by whitespace. Shared tokens between ref and obs (filenames, keywords) are natural alignment anchors.
2. **Line-level alignment**: match ref lines to obs lines. Match cost = token edit distance within the line pair. Delete/insert cost = token count. This correctly matches `"a.txt"` with `"-rw-r--r-- 1 root root 0 Jan 1 2020 a.txt"` because the shared token `a.txt` makes matching cheaper than delete+insert.
3. **Token-level alignment**: within matched lines, classify each token position as Keep, Insert, Delete, or Replace.
4. **Reorder detection**: if ref and obs contain the same lines in different order, encode as a permutation vector.

The resulting edit script is a structural description independent of per-cell nondeterminism (each cell has its own inode numbers, but the edit "insert one token at position 0" is the same regardless of the specific inode value).

Runs with identical edit scripts in every context form a behavioral group. This is the equivalence relation: two flags are indistinguishable iff no tested context separates their structural transformation.

All runs — single-flag AND pairwise combinations — are tested in a single phase. No iterative refinement. The experimental design is fixed before execution, eliminating path-dependence (where intermediate results could influence which experiments are generated next).

## Execution

Cells are batched by context. Per context: create per-cell workspace directories, generate one shell script with all commands (each with 2-second timeout), invoke bwrap once. ~27 bwrap invocations instead of thousands. 8 threads across contexts. Integration tests run 22 binaries in parallel (JOBS=4) in ~67 seconds.

## Limitations

- **Error-only flags**: flags that need specific content types (month names for `sort -M`, version strings for `sort -V`) or specific argument values to produce non-error output remain unobserved. These are at the boundary of binary-agnostic exploration.
- **Modifier flags**: flags like `-h` that only modify another flag's output are distinguished via pairwise combination testing, which provides interaction evidence.
- **Timing-dependent flags**: `cp -u` (copy only if newer) is nondeterministic because source and destination files are created within the same second. Filesystem snapshot-based change detection also has mtime race conditions.
- **No semantic interpretation**: the tool reports *that* flags differ, not *why*. The structural edit script vocabulary (Insert/Delete/Keep/Replace) provides structural context but no domain semantics.
- **Stateful binaries**: tools requiring prerequisite state (git repositories) need manual setup.
