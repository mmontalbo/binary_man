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

## Pipeline

Six stages. Each transforms data and passes it downstream.

```
                          binary name
                              │
                              ▼
┌──────────────────────────────────────────────────────┐
│                    DISCOVER                          │
│                                                      │
│  What factors exist? What levels work?               │
│  What's the experimental design?                     │
│                                                      │
│  IN:  binary path                                    │
│  OUT: Script (contexts × runs) + FlagInfo            │
└──────────────────────┬───────────────────────────────┘
                       │
            Script: 25-35 contexts
                    100-4000+ runs
                       │
                       ▼
┌──────────────────────────────────────────────────────┐
│                    EXECUTE                            │
│                                                      │
│  Run every (context, run) cell in sandbox            │
│                                                      │
│  IN:  Script + binary                                │
│  OUT: GridResult: (context, run) → Observation       │
└──────────────────────┬───────────────────────────────┘
                       │
            GridResult: 200-7000 cells
            each with stdout/stderr/exit/fs
                       │
                       ▼
┌──────────────────────────────────────────────────────┐
│                    ANALYZE                            │
│                                                      │
│  Which flags are behaviorally distinct?              │
│  How robust is the evidence?                         │
│                                                      │
│  IN:  Script + GridResult + FlagInfo                 │
│  OUT: AnalysisMetrics (groups, robustness)           │
└──────────────────────┬───────────────────────────────┘
                       │
            BehaviorGroups + robustness scores
                       │
                       ▼
┌──────────────────────────────────────────────────────┐
│                    REPORT                             │
│                                                      │
│  Classify and present findings                       │
│                                                      │
│  IN:  AnalysisMetrics + FlagInfo                     │
│  OUT: Markdown report                                │
└──────────────────────────────────────────────────────┘
```

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

**Context: words_minimal** (input.txt = `cherry\nApple\nbanana\n...`, 7 lines)
```
base (head input.txt):     cherry\nApple\nbanana\nDate\nelderberry\nBANANA\napple
flag (head -v input.txt):  ==> input.txt <==\ncherry\nApple\nbanana\nDate\nelderberry\nBANANA\napple
```
Delta from base: +1 line (`==> input.txt <==` header prepended)

**Context: numbers_minimal** (input.txt = `100\n2\n30\n...`, 16 lines)
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

## Discovery

Three sub-phases. The first two are adaptive (probing); the third is a deterministic cross-product with no further observation.

```
binary
  │
  ▼
┌─────────────────────────────────────────────────────────────────┐
│ 1. FACTOR IDENTIFICATION                                        │
│                                                                 │
│    try_help(binary)  ─────►  help text                          │
│         │                       │                               │
│         │              extract_flag_info()                       │
│         │                       │                               │
│         │                       ▼                               │
│         │                   FlagInfo                             │
│         │                   ├─ flags: [("-n", Some("NUM")),      │
│         │                   │          ("--sort", Some("WORD")), │
│         │                   │          ("-r", None), ...]        │
│         │                   ├─ aliases: {"-n" ↔ "--numeric"}    │
│         │                   └─ extracted_values: {"--sort" →     │
│         │                       ["general", "human", "month"]}  │
│         │                                                       │
│         ▼                                                       │
│    probe_arg_patterns(help_text)                                │
│         │                                                       │
│         │  ┌──────────── 1 bwrap ────────────┐                  │
│         │  │ try [], [file], [dir],           │                  │
│         │  │     [file,file], [file,dir],     │                  │
│         │  │     [pattern,file], [pattern,dir]│                  │
│         │  │     stdin, stdin+"-",            │                  │
│         │  │     structural (echo, -print...) │                  │
│         │  └─────────────────────────────────┘                  │
│         │                                                       │
│         ▼                                                       │
│    working_patterns: [[file], [file,file], ...]                 │
│    stdin_works: bool                                            │
│    probe_pattern: Option<"cherry">                              │
└─────────────────────────────────┬───────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────┐
│ 2. LEVEL DETERMINATION (pilot study)                            │
│                                                                 │
│    For each value-taking flag: which candidate values work?     │
│                                                                 │
│    Phase 1 ─── 1 bwrap ──────────────────────────────────────── │
│    │  solo: every (flag, candidate) pair                        │
│    │  + error mine: every flag with __bgrid_invalid__           │
│    │                                                            │
│    Phase 2 ─── 1 bwrap ──────────────────────────────────────── │
│    │  probes for values parsed from error mine stderr           │
│    │  + stdin retries for flags still without exit 0            │
│    │                                                            │
│    Phase 3 ─── 1 bwrap ──────────────────────────────────────── │
│    │  companion: failing_flag + working_flag as enabler         │
│    │  (e.g., cut -d needs -f to succeed)                        │
│    │                                                            │
│    Phase 4 ─── 1 bwrap ──────────────────────────────────────── │
│    │  mutual compound: pairs of both-failing flags together     │
│    │                                                            │
│    Result per flag:                                             │
│      working value (first exit-0 candidate) ─or─ None          │
│      extra_solo_values (additional working candidates)          │
│      prerequisites (companion dependencies: -d requires -f)    │
└─────────────────────────────────┬───────────────────────────────┘
                                  │
                                  ▼
┌─────────────────────────────────────────────────────────────────┐
│ 3. DESIGN CONSTRUCTION (deterministic cross-product)            │
│                                                                 │
│    Contexts from fixture corpus (see Context Design below)      │
│                                                                 │
│    Runs:                                                        │
│    ┌─────────────────────────────────────────────────────┐      │
│    │ For each working_pattern:                           │      │
│    │   base run (no flags)                ──┐            │      │
│    │   per-flag solo run (diff from base) ──┤ all ctxs   │      │
│    │   extra value runs                   ──┘            │      │
│    │                                                     │      │
│    │ If stdin_works:                                     │      │
│    │   bare base + per-flag solo runs        all ctxs    │      │
│    │                                                     │      │
│    │ Boundary runs: numeric flags ×{0,-1,MAX} 1st pattern│      │
│    │                                                     │      │
│    │ Pairwise combos: all flag pairs ×       6 ctxs only │      │
│    │   both orderings (A,B) and (B,A)                    │      │
│    │                                                     │      │
│    │ Error provocation: nonexistent-file.txt all ctxs    │      │
│    └─────────────────────────────────────────────────────┘      │
│                                                                 │
│    Each non-base run carries diff_from → baseline args          │
│    Combo runs scoped to 6 diverse contexts (not all 35)         │
│                                                                 │
│    OUT: Script { contexts: ~30, runs: ~100-4000 }               │
└─────────────────────────────────────────────────────────────────┘
```

### Value discovery pipeline

Multi-source candidate discovery per flag:

1. **Help text mining** — quoted values (`'auto'`), brace enumerations (`{a,b,c}`), bracket character sets (`[xyz]`), continuation lines.
2. **Metavar candidates** — per-type curated lists (NUM → `1,0,2,10,100`; FILE → `input.txt`; CHAR → `,`, `:`, etc.).
3. **Error mining** — fires when no candidate exits 0; sends `__bgrid_invalid__` and parses "Valid arguments are:" from stderr.
4. **Companion probing** — failing flags tried with each working flag as companion.
5. **Mutual compound probing** — pairs of both-failing flags tried together (discovers co-dependencies like `cut -d` + `cut -f`).

First working value = combo value (stable). All working values generate independent solo runs (additive).

**Alias propagation**: short flags inherit metavar from long alias for proper value probing.

The pilot is adaptive (later probes depend on earlier results). The main experiment is fixed.

## Context Design

5 content levels × 3 structure levels with cycling property modifiers:

```
              minimal         standard              deep
words         default         varied-perms          varied-times
numbers       varied-times    default               varied-perms
passwd        varied-perms    varied-times          default
formatted     default         varied-times          varied-perms
csv           varied-times    varied-perms          default
```

**Content levels** (what's in input.txt):
- **words**: 1500 sorted English words with mixed case, hyphens, accents
- **numbers**: integers, floats, hex, scientific notation, NaN, Infinity
- **passwd**: /etc/passwd format, colon-delimited, 7 fields, UIDs, shells
- **formatted**: tabs, blank lines, trailing whitespace, control characters
- **csv**: RFC 4180 CSV, header row, quoted fields, accented names, duplicates

**Breadth-only** (minimal structure only): access_log, syslog, dates, config, paths, naughty (unicode/emoji/RTL)

**Structure levels** (what files exist):
- **minimal**: input.txt + other.txt
- **standard**: + hidden file, a.txt, b.txt, subdir, symlink, executable
- **deep**: + 3-level nesting, directory symlink

**Property levels** (file metadata):
- **default**: no special properties
- **varied-perms**: readonly file, flag-like filename (`-rf`)
- **varied-times**: old mtime, large file (10KB)

Plus 9 single-factor perturbations from numbers_standard (remove hidden/subdir/symlink, empty/readonly/old-mtime/tiny input.txt, COLUMNS=40, LC_ALL=en_US.UTF-8), a locale perturbation on words_minimal, and 3 stdin contexts (words, numbers, passwd with varied delimiters). Total: ~35 contexts per grid.

## Execution

```
Script { contexts: [C₁..Cₙ], runs: [R₁..Rₘ] }
                    │
                    ▼
         ┌─── enumerate cells ───┐
         │  filter: which runs   │
         │  match each context   │
         │  → cells_by_context   │
         └───────────┬───────────┘
                     │
    ┌────────────────┼─────────────────┐
    ▼                ▼                 ▼
 Thread 1         Thread 2    ...   Thread T    (T ≤ 32)
    │                │                 │
    │  ┌── work-stealing queue ──┐     │
    │  │  dequeue next context   │     │
    │  └─────────┬───────────────┘     │
    │            │                     │
    ▼            ▼                     ▼
 ┌─────────────────────────────────────────┐
 │ PER CONTEXT (one bwrap invocation):     │
 │                                         │
 │  Host side:                             │
 │  ├─ create batch_dir/                   │
 │  ├─ apply_setup(context) → env vars     │
 │  ├─ for each cell:                      │
 │  │   ├─ create c{i}/ workspace          │
 │  │   ├─ snapshot filesystem (before)    │
 │  │   └─ emit shell script line          │
 │  └─ write run.sh                        │
 │                                         │
 │  bwrap sandbox:                         │
 │  ├─ --unshare-net --die-with-parent     │
 │  ├─ ro-bind /usr /bin /lib /etc         │
 │  ├─ bind batch_dir → /batch             │
 │  └─ sh /batch/run.sh                    │
 │       each cell: (cd /batch/c{i} &&     │
 │         [stdin|] timeout 2 binary args   │
 │         >/batch/out/{i}.out             │
 │         2>/batch/out/{i}.err;           │
 │         echo $? >/batch/out/{i}.rc) &   │
 │       wait every 32 cells               │
 │                                         │
 │  Host side (after):                     │
 │  ├─ read {i}.out, {i}.err, {i}.rc      │
 │  ├─ snapshot filesystem (after)         │
 │  └─ diff snapshots → FsChanges         │
 └─────────────────────────────────────────┘
                     │
                     ▼
         GridResult {
           cells: { (context_name, run_idx) → Observation }
           setup_failures: { context → error }
         }
```

Discovery probing uses the same batched model — each phase of the pilot study (solo candidates, error mining, companion probing, compound probing) generates a shell script and runs all probes in a single bwrap invocation, with per-probe cell directories for isolation.

## Analysis

### Delta grouping

For runs with a `from` reference (base invocation), comparison uses **structural deltas** — what transformation the flag applied to the base output — rather than the raw output content. This groups flags by the structural change they produce ("prepended 8 tokens per line", "reversed line order", "inserted a header line") rather than by specific output values.

The structural delta is computed via hash-anchored alignment:

1. **Tokenize**: split stdout into lines, split lines by whitespace.
2. **Hash-anchor matching**: hash each line, find exact-match anchors between ref and obs in O(n). Shared lines (filenames, keywords) are natural anchors.
3. **Gap alignment**: between anchors, run Needleman-Wunsch on the small unmatched segments. Match cost = token edit distance within line pairs. Gap cap at 100 lines for unanchored segments.
4. **Token-level alignment**: within matched line pairs, classify each token as Keep, Insert, Delete, or Replace.

For outputs with shared lines (90%+ for most tools), alignment is O(n). For completely different outputs (e.g., diff normal vs unified format), the disjoint hash sets trigger an early exit — no anchor search needed.

### Grouping and evidence pipeline

```
GridResult + Script
        │
        ▼
┌───────────────────────────────────────────────────────────────┐
│ PER-RUN ANALYSIS                                              │
│                                                               │
│ For each run Rᵢ across all contexts:                          │
│                                                               │
│   ┌─── Has diff_from? ───┐                                   │
│   │ YES                  │ NO                                 │
│   ▼                      ▼                                    │
│  ObsKey = structural    ObsKey = tokenized                    │
│  delta vs baseline      raw observation                       │
│                                                               │
│  Structural delta (two-level NW):                             │
│  ┌──────────────────────────────────────┐                     │
│  │ ref: "Alice"    obs: "-rw 1 Alice"   │                     │
│  │      "Bob"           "-rw 1 Bob"     │                     │
│  │                                      │                     │
│  │ Line alignment (hash anchors + NW):  │                     │
│  │   Modified([Insert("-rw"),           │                     │
│  │            Insert("1"),              │                     │
│  │            Keep("Alice")])           │                     │
│  │   Modified([Insert("-rw"),           │                     │
│  │            Insert("1"),              │                     │
│  │            Keep("Bob")])             │                     │
│  │                                      │                     │
│  │ Same structural edit = same behavior │                     │
│  │ regardless of content nondeterminism │                     │
│  └──────────────────────────────────────┘                     │
│                                                               │
│  Per-run output:                                              │
│    context_groups: [{majority ctxs, obs}, {minority, obs}]    │
│    sensitivity: ["stdin (+3 lines)", "COLUMNS (reordered)"]   │
│    universals: ["exit 0", "stdout not empty"]                 │
└───────────────────────────┬───────────────────────────────────┘
                            │
                            ▼
┌───────────────────────────────────────────────────────────────┐
│ BEHAVIORAL GROUPING                                           │
│                                                               │
│  group_key(Rᵢ) = hash(diff_from, [ObsKey per context])       │
│                                                               │
│  Runs with identical structural deltas across all contexts    │
│  collapse into the same BehaviorGroup.                        │
│                                                               │
│  Example groups for `ls`:                                     │
│  ┌──────────────────────────────────────────────────┐         │
│  │ Group A (isolated): ["-l"]                       │         │
│  │   → inserts 8 tokens per line                    │         │
│  │                                                  │         │
│  │ Group B (isolated): ["-i"]                       │         │
│  │   → inserts 1 token (inode) per line             │         │
│  │                                                  │         │
│  │ Group C (2 runs):   ["-a", "--all"]              │         │
│  │   → same delta (add hidden files) = alias        │         │
│  │                                                  │         │
│  │ Group D (3 runs):   ["--color=auto",             │         │
│  │                      "--color=always", "-G"]     │         │
│  │   → indistinguishable under test conditions      │         │
│  └──────────────────────────────────────────────────┘         │
│                                                               │
│  isolated group (1 run)  → solo-distinguished                 │
│  multi-run group         → need pairwise evidence to separate │
└───────────────────────────┬───────────────────────────────────┘
                            │
                            ▼
┌───────────────────────────────────────────────────────────────┐
│ PAIRWISE INTERACTION EVIDENCE                                 │
│                                                               │
│  For combination runs (2+ flags):                             │
│  If runs sharing the same companion flags land in different   │
│  groups, the differing flag is proven distinguishable.        │
│                                                               │
│  "-a -l" in Group X,  "-r -l" in Group Y                     │
│   └─ same companion (-l), different groups                    │
│   └─ proves: -a ≠ -r (both distinguished)                    │
│                                                               │
│  This catches flags that ARE different but happen to          │
│  produce identical solo output.                               │
└───────────────────────────┬───────────────────────────────────┘
                            │
                            ▼
┌───────────────────────────────────────────────────────────────┐
│ LEAVE-ONE-OUT ROBUSTNESS                                      │
│                                                               │
│  For each distinguished flag:                                 │
│    For each of ≤10 sampled contexts:                          │
│      mask that context, re-group, re-check distinguished      │
│                                                               │
│  flag → (survived / total)                                    │
│  15/15 = robust:  flag works regardless of any single context │
│   3/15 = fragile: flag only distinguished in specific setups  │
│                                                               │
│  Uses hash-only grouping (no equality verify) for speed       │
│  O(contexts × runs) per iteration                             │
└───────────────────────────────────────────────────────────────┘
```

All runs — single-flag AND pairwise combinations — are tested in a single phase. No iterative refinement. The experimental design is fixed before execution, eliminating path-dependence (where intermediate results could influence which experiments are generated next).

## Report

```
AnalysisMetrics
       │
       ├─── solo_distinguished: flags in isolated groups
       │      exemplar: most distinctive (context, base, flag output)
       │
       ├─── combo_distinguished: proved different via pairwise evidence
       │      but not isolated (identical solo behavior)
       │
       ├─── behavioral_aliases: 2-run groups with different flag names
       │      (e.g., -A = --show-all)
       │
       ├─── error_differentiated: unique error messages per flag
       │      (flag errors with all tried values)
       │
       ├─── indistinguishable: in multi-run groups, no pairwise evidence
       │      (might be truly identical, or contexts don't expose difference)
       │
       └─── untested: known flags never included in any run
```

## Quality metrics

- **Robustness**: leave-one-out context removal (sampled 10 contexts). Flags that survive all removals are robust; flags dependent on a single context are fragile.
- **Reproducibility**: opt-in cross-run verification (REPRO=1). Re-runs all binaries and compares observed counts. Nondeterministic binaries reported but don't fail the test.
- **Surface stability**: exact flag count checked against expected total. Changes in flag discovery surface (regex shifts) are caught.

## Design Invariants

```
1. FIXED GRID        The entire experiment (contexts × runs) is
                     determined before any behavioral observation.
                     Discovery probes determine levels; they do
                     not observe behavior.

2. ADDITIVE-ONLY     Adding contexts or values can only ADD
                     evidence, never remove it.

3. STRUCTURAL DIFF   Grouping uses edit scripts, not raw output.
                     Two flags producing the same transformation
                     group together regardless of input content.

4. BATCHED SANDBOX   Both discovery probes and grid cells use the
                     same model: shell script → bwrap → read files.
                     One bwrap per context (grid) or per phase
                     (discovery). No individual process spawning.

5. DETERMINISTIC     Candidate lists are ordered and stable.
   VALUE SELECTION   First working = combo value. Probing order
                     is fixed, not adaptive within a phase.
```

## Limitations

- **Error-only flags**: flags that need specific content types (month names for `sort -M`, version strings for `sort -V`) or specific argument values to produce non-error output remain unobserved. These are at the boundary of binary-agnostic exploration.
- **Modifier flags**: flags like `-h` that only modify another flag's output are distinguished via pairwise combination testing, which provides interaction evidence.
- **Timing-dependent flags**: `cp -u` (copy only if newer) is nondeterministic because source and destination files are created within the same second. Filesystem snapshot-based change detection also has mtime race conditions.
- **No semantic interpretation**: the tool reports *that* flags differ, not *why*. The structural edit script vocabulary (Insert/Delete/Keep/Replace) provides structural context but no domain semantics.
- **Stateful binaries**: tools requiring prerequisite state (git repositories) need manual setup.
