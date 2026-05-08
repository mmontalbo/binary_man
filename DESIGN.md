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

A flag is **characterized** when at least one run containing that flag is in a singleton group — its behavioral fingerprint is unique across the tested conditions.

## Experimental variables

The tool's input space has four independent dimensions:

| Dimension | What varies | What it exercises |
|-----------|-------------|-------------------|
| File content | Lines inside input files | Text processors (sort, grep, awk, sed) |
| Directory structure | What files/dirs exist, nesting depth | Filesystem tools (ls, find, cp, du) |
| File properties | Permissions, timestamps, sizes | Metadata-aware tools (ls -l, stat, chmod) |
| Positional arguments | File path vs directory vs pattern | Which code path the binary takes |

The current design varies all four simultaneously across 6 base contexts plus 7 single-factor perturbations = 13 contexts per grid.

## Context design

Each base context presents a distinct world along all four dimensions:

| Context | Structure | Content | Properties | Target arg |
|---------|-----------|---------|------------|------------|
| few_files | 2 files | alpha-sorted words | default | input.txt |
| many_files | 9 files, hidden, subdir | numeric | default | . |
| deep_tree | 3-level nesting, dir symlink | colon-delimited | default | input.txt |
| mixed_types | symlink, broken link, exec, readonly, flag-like name | mixed case | varied perms | input.txt |
| timestamped | varied sizes (0-10KB), old/recent mtime | duplicated lines | varied times | input.txt |
| empty_dir | nothing | - | - | . |

Single-factor perturbations from many_files: remove .hidden, remove .config, remove subdir, empty input.txt, readonly, old mtime, size=1. These enable attribution — when a run behaves differently in "many_files / remove .hidden" vs "many_files", the difference is attributable to that specific perturbation.

For pattern-taking tools (grep, sed, awk), the pattern argument is also varied: literal match, case variant, regex metacharacter, non-matching. This exercises flags like -i (case sensitivity), -E/-F/-G (regex engine), -w (word boundary).

## Observation and collapsing

Each run executes in every context. Observations are compared across contexts to form **context groups** — subsets of contexts where the run produced identical output. The largest context group is the "majority" behavior; contexts outside it reveal **sensitivity** to specific perturbations.

Runs are then compared to each other by their full per-context observation vectors. Runs with identical observations in EVERY context are grouped into **behavioral groups**. This is the core equivalence relation: two flags are behaviorally indistinguishable if and only if no tested context separates them.

## Sensitivity analysis

For each run, sensitivity labels identify which perturbations caused behavioral splits, with quantified effect sizes:

```
sensitive to: input.txt=size:1 (-4 lines), input.txt readonly (exit 0->2)
```

This tells us: reducing input.txt to 1 byte removed 4 output lines; making it readonly changed the exit code. These are mechanically derived from context group membership — no interpretation is applied.

## Iterative refinement

Round 0 runs all discovered flags across the base contexts. Subsequent rounds generate new experiments targeting flags that remain in identical groups.

Three refinement strategies:

**Cross-group interaction.** Pair each uncharacterized flag with the top 3 characterized flags (those with the most sensitivity signal). If flags A and B are in the same identical group but (X+A) and (X+B) produce different observations, A and B are proven distinguishable — they modify X's behavior differently. This targets the modifier-flag problem: flags like ls -h (human-readable sizes) that only show effect when combined with a mode flag like -l.

**Sensitivity refinement.** For dimensions that caused splits in previous rounds, generate graduated variants. If a flag is sensitive to file size, test sizes 1, 100, 1K, 10K, 100K to find the threshold. This sharpens the characterization of already-isolated flags.

**Untested flag pickup.** Flags discovered from --help but not yet included in any run. Alias-deduplicated (if -b was tested, --ignore-leading-blanks is marked tested).

**Convergence.** The loop stops when no new flags are characterized in a round, or after a maximum number of rounds (default 3).

**Accumulation.** Flags characterized in any round stay characterized. Unproductive runs (all errors, or in large identical groups with the same target) are pruned from subsequent rounds to avoid wasted cells.

## Characterization metric

The primary metric is: **characterized flags / total flags**.

A flag is characterized if any run containing it (solo or in combination) produces a unique behavioral fingerprint. The report separates:

- **Solo**: the flag alone produces unique behavior across contexts
- **Via combination**: the flag is distinguishable only when paired with another flag (proven by pairwise evidence from cross-group interaction)
- **Uncharacterized**: the flag remains in an identical group — no tested condition distinguishes it from other flags in the group
- **Untested**: discovered from --help but not included in any run

Combination-based characterization is weaker than solo: it proves the flag *does something different* but the specific effect is only visible in combination. Solo characterization means the flag's independent behavioral surface has been observed.

## Limitations

**The positional argument determines the ceiling.** If all runs use `ls input.txt`, flags that affect directory listing (most ls flags) are invisible. The dual-arg approach (file + directory) and pattern archetypes mitigate this but don't eliminate it.

**Modifier flags require combination testing.** Flags like -h, -G, -k that modify another flag's output (e.g., -l) cannot be characterized solo. Cross-group interaction addresses this but adds cell cost.

**Context diversity is binary-agnostic, not binary-optimal.** The same 13 contexts are used for every binary. A sort-specific probe with carefully chosen content would characterize more flags than the generic contexts. The trade-off is automation vs coverage.

**Observation equality is exact.** Two outputs differing by a single whitespace character are in different groups. This can cause false splits (timestamp jitter, random ordering) or false equivalence (outputs that differ only in unobservable ways like trailing whitespace).

**No semantic interpretation.** The tool reports *that* flags differ, not *why*. Understanding what `-n` does (numeric sort) requires reading the output, not just knowing it's isolated. The behavioral groups are evidence for interpretation, not interpretation itself.

## Execution

Each cell gets its own bwrap sandbox (network-isolated, separate mount namespace). Context setup is replayed per cell. All cells run in parallel bounded by a thread pool (8 threads). A 5-second per-cell timeout kills runaway processes via process group signal. Wall time is recorded per cell; CPU time and memory are not measured in parallel mode.
