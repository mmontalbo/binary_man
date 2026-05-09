# Experiment Design

## Problem

Given an arbitrary CLI binary, mechanically characterize what each flag does
by observing behavior differences across varied input conditions — without
source code, documentation, or knowledge of the binary's domain.

## Model

The binary is a black box: f(flags, positional_args, filesystem_state, env) -> (stdout, stderr, exit_code, fs_changes).

An **observation** is the recorded tuple (stdout, stderr, exit_code, fs_changes) from one invocation.

A **cell** is one (context, run) pair — a specific invocation in a specific input state.

The **grid** is contexts x runs. Every cell produces one observation.

Two observations are **identical** when stdout, stderr, exit_code, and fs_changes all match exactly.

A **behavioral group** is a set of runs that produce identical observations across ALL contexts. Runs in the same group are behaviorally indistinguishable given the experimental conditions.

A flag is **distinguished** when at least one run containing that flag (solo or in combination with other flags) produces a unique behavioral fingerprint under the tested conditions.

## Experimental variables

The tool's input space has four independent dimensions:

| Dimension | What varies | What it exercises |
|-----------|-------------|-------------------|
| File content | Lines inside input files | Text processors (sort, grep, awk, sed) |
| Directory structure | What files/dirs exist, nesting depth | Filesystem tools (ls, find, cp, du) |
| File properties | Permissions, timestamps, sizes | Metadata-aware tools (ls -l, stat, chmod) |
| Positional arguments | File, directory, multi-file, pattern variants | Which code path the binary takes |

The base contexts use a Latin square design: 5 content levels x 3 structure levels x 3 property levels. Property assignment cycles through a Latin square pattern. This ensures main effects are estimable without aliasing — when a flag behaves differently across structure levels, the difference is attributable to structure alone, not confounded with content or properties.

All structure levels include `input.txt` and `other.txt` so multi-file invocations work in every context. Content and structure definitions are in `data.rs`.

## Context design

5 content levels x 3 structure levels with cycling property assignment:

```
              minimal         standard              deep
alpha         default         varied-perms          varied-times
numeric       varied-times    default               varied-perms
fielded       varied-perms    varied-times          default
formatted     default         varied-times          varied-perms
tabular       varied-times    varied-perms          default
```

**Content levels** (what's in input.txt):
- **alpha**: 7 mixed-case words (cherry, Apple, banana, Date, ...)
- **numeric**: 16 integers (exercises truncation — longer than `head -n 10`)
- **fielded**: 3 colon-delimited records (bob:30:sales, ...)
- **formatted**: tabs, blank lines, trailing whitespace, control characters (\\x01, \\x07, \\x1b)
- **tabular**: tab-delimited fields, repeated rows, long line >80 chars

**Structure levels** (what files exist):
- **minimal**: input.txt + other.txt (2 files)
- **standard**: + hidden, subdir, symlink, exec, more files (10 entries)
- **deep**: + 3-level nesting, directory symlink (7 entries)

**Property levels** (file metadata):
- **default**: no special properties
- **varied-perms**: readonly file, flag-like filename (`-rf`)
- **varied-times**: old mtime, large file (10KB)

Plus: empty_dir (nothing — error path exerciser), 9 single-factor perturbations from numeric_standard (remove .hidden, remove subdir, remove link.txt, empty input.txt, readonly, old mtime, size=1, LC_ALL=en_US.UTF-8, COLUMNS=40), and a locale perturbation on alpha_minimal (mixed-case content + UTF-8 locale).

Total: ~27 contexts per grid.

For pattern-taking tools (grep, sed, awk), the pattern argument is also varied: literal match, case variant, regex metacharacter, non-matching. This exercises flags like -i (case sensitivity), -E/-F/-G (regex engine), -w (word boundary).

## Observation and collapsing

Each run executes in every context. Observations are compared across contexts to form **context groups** — subsets of contexts where the run produced identical output. The largest context group is the "majority" behavior; contexts outside it reveal **sensitivity** to specific perturbations.

Runs are then compared to each other by their full per-context observation vectors. For runs with a `from` reference (diff base), comparison uses **delta keys** — what changed relative to the base — rather than absolute observations. This groups flags by the transformation they apply (e.g., "reversed the lines", "added a header column") rather than what the output looks like. Different reorderings are encoded as permutation vectors so that reverse-sort and time-sort are distinguished even when they operate on the same set of lines.

Runs with identical delta keys in EVERY context are grouped into **behavioral groups**. This is the core equivalence relation: two flags are behaviorally indistinguishable if and only if no tested context separates their transformation of the base output.

## Sensitivity analysis

For each run, sensitivity labels identify which perturbations caused behavioral splits, with quantified effect sizes:

```
sensitive to: input.txt=size:1 (-4 lines), input.txt readonly (exit 0->2)
```

This tells us: reducing input.txt to 1 byte removed 4 output lines; making it readonly changed the exit code. These are mechanically derived from context group membership — no interpretation is applied.

## Iterative refinement

Round 0 runs all discovered flags across the base contexts. Subsequent rounds generate new experiments targeting flags that remain in identical groups.

Four refinement strategies:

**Within-group interaction.** For large identical groups (3+ unique flags), generate pairwise flag combinations. If two flags in the same group interact differently when combined, they're proven distinguishable.

**Stem-guided cross-group interaction.** The report-level flag-stem analysis identifies which specific flags remain indistinguishable. For each such flag, the refinement finds all its run variants (file target, directory target, multi-file target) and pairs it with the top isolated flags that have the most sensitivity signal. This closes the loop between report-level analysis and experiment generation — the refinement targets exactly the flags the report identifies as needing more evidence. Sensitivity-dimension overlap guides partner selection; a fallback to top-by-dimension-count prevents gaps when no dimensions overlap.

**Sensitivity refinement.** For dimensions that caused splits in previous rounds, generate graduated variants. If a flag is sensitive to file size, test sizes 1, 100, 1K, 10K, 100K to find the threshold.

**Untested flag pickup.** Flags discovered from --help but not yet included in any run. Alias-deduplicated (if -b was tested, --ignore-leading-blanks is marked tested).

**Convergence.** The loop stops when no new flags are distinguished in a round, or after a maximum number of rounds (default 3).

**Accumulation.** Flags distinguished in any round stay distinguished. Unproductive runs (in large identical groups with the same target) are pruned from subsequent rounds to avoid wasted cells.

## Distinguishability metric

The primary metric is: **distinguished flags / total flags**.

A flag is distinguished if any run containing it (solo or in combination) produces a unique behavioral fingerprint under the tested conditions. The report separates:

- **Solo**: the flag alone produces unique behavior across contexts
- **Via combination**: the flag is distinguishable only when paired with another flag (proven by pairwise evidence from cross-group interaction)
- **Indistinguishable**: the flag remains in an identical group — no tested condition separates it from other flags in the group. This is a statement about the tested conditions, not about the flag itself; more conditions might distinguish it.
- **Untested**: discovered from --help but not included in any run

Combination-based evidence is weaker than solo: it proves the flag *modifies behavior differently than another flag* but the specific independent effect is only visible in combination. Solo evidence means the flag's independent behavioral surface has been directly observed.

**Behavioral aliases** are detected when two flags with different names produce identical behavior across all contexts. These are reported separately — they may be genuine aliases (same flag, different name) or genuinely different flags that are indistinguishable under tested conditions.

**Exemplar observations** are shown for each solo flag: the context where the flag's behavior is most distinctive, with both the base invocation output and the flag invocation output. This mechanically demonstrates what the flag does without documentation or prior knowledge.

## Limitations

**Positional argument coverage.** Runs are generated with three target types: single file (`input.txt`), directory (`.`), and multi-file (`input.txt other.txt`). Pattern-taking tools also get four pattern variants. This covers most invocation patterns but not all — stdin piping, glob expansion, and recursive directory arguments are not yet generated.

**Modifier flags require combination testing.** Flags like -h, -G, -k that modify another flag's output (e.g., -l) cannot be distinguished solo. Cross-group interaction addresses this but the evidence is weaker — it proves the flag modifies behavior differently, not what its independent effect is.

**Context diversity is binary-agnostic, not binary-optimal.** The same ~27 contexts are used for every binary. A sort-specific probe with carefully chosen content would distinguish more flags than the generic contexts. The trade-off is automation vs coverage.

**Delta grouping is lossy for non-permutation reorderings.** When output lines are added or removed AND reordered simultaneously, the delta encodes the set difference but not the order of shared lines. Two flags that add the same lines but in different positions may be incorrectly grouped.

**No semantic interpretation.** The tool reports *that* flags differ, not *why*. Understanding what `-n` does (numeric sort) requires reading the output, not just knowing it's isolated. The behavioral groups are evidence for interpretation, not interpretation itself.

**Environment sensitivity is limited.** The sandbox defaults to LANG=C with piped stdout (no TTY). LC_ALL and COLUMNS perturbations are included but terminal-dependent flags (color output, cursor control) may be indistinguishable because stdout is not a TTY.

**Stateful binaries need manual setup.** Tools like git that require prerequisite state (repositories, commits) cannot be fully explored automatically. The `invoke` mechanism supports manual context setup, but automatic discovery of prerequisite invoke sequences is not yet implemented.

## Execution

Cells are grouped by context. For each context, all cell workspaces are created as subdirectories under a batch parent, a shell script runs all commands with per-command `timeout`, and bwrap is invoked once per context (not per cell). This reduces thousands of bwrap namespace creations to ~27, achieving ~5000 cells/s.

Each cell gets its own workspace directory within the batch. The bwrap sandbox provides network isolation and a controlled mount namespace (read-only system paths, writable workspace). A 2-second per-command timeout kills runaway processes. Contexts are processed in parallel across 8 threads.

The integration test verifies 17 coreutils binaries in ~40 seconds.
