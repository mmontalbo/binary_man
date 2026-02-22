# Milestones

This document tracks the static-first roadmap for generating man pages from
`binary_lens` packs. Dynamic execution is optional and used for scenario-backed
validation, coverage tracking, and (eventually) a structured "enrichment loop"
that supports iterative static + dynamic passes from portable doc packs.

Current focus: M33 (LM Semantic Enrichment).

## M33 — LM Semantic Enrichment (planning)

Goal: Improve verification of complex binaries (git, docker, kubectl) by having the
LM generate prerequisite-aware scenarios **from documentation**, not from runtime
failures.

### Core Insight

Instead of **error-driven discovery** (run, fail, learn), use **documentation-driven
generation** (read option description, generate appropriate setup). The LM already
has access to option descriptions and can infer prerequisites semantically.

### Approach

1. **Richer prompt context**: Include full option descriptions (not just IDs)
2. **Semantic prereq hints**: LM prompt guidance for common prereq patterns
3. **Working argv examples**: Show verified argvs from `learned_hints.json`
4. **Better initial scenarios**: Generate prereq-aware scenarios on first pass

### Example

**Before (error-driven):**
```
1. Generate bare scenario for `git rm`
2. Run, fail: "not a git repository"
3. LM proposes setup
4. Retry with setup
5. Fail: "pathspec did not match"
6. LM proposes more setup
7. Retry again
8. Success (3 cycles)
```

**After (documentation-driven):**
```
1. LM sees option description: "Remove files from the working tree and index"
2. LM infers: "this operates on tracked files, needs repo with files"
3. Generates scenario with setup: git init, create file, git add
4. Run, success (1 cycle)
```

### Benefits

- **Fewer cycles**: Generate correct scenario on first pass
- **No runtime learning structures**: LM understands semantics directly
- **Better generalization**: Works across binaries without family management
- **Simpler code**: Removed ~400 LOC of unused infrastructure

### Implementation Direction

| Priority | Change |
|----------|--------|
| High | Include option descriptions in all prompts (currently ID-only) |
| High | Add prereq guidance section to `behavior_base.md` |
| Medium | Pass `working_argvs` as examples (already implemented) |
| Low | Consider context-aware seed templates |

### Acceptance Criteria

| Criterion | Status |
|-----------|--------|
| Remove unused family infrastructure | done |
| Keep working_argvs in learned_hints | done |
| Document semantic enrichment direction | done |
| Include descriptions in prompts | todo |
| Add prereq guidance to behavior prompts | todo |
| E2E test: git config with enriched prompts | todo |

---

## M32 — Co-Dependent Surface Verification (done)

Goal: Complete `git config` verification by improving LM prompts for co-dependent
options. **Hypothesis confirmed**: better prompts alone solved the remaining gaps.

### Results

| Metric | M31 (Before) | M32 (After) |
|--------|--------------|-------------|
| Total surfaces | 34 | 34 |
| Verified (delta_seen) | 24 | 24 |
| Unverified | 2 | 0 |
| Excluded | 0 | 2 (`--edit`, `-e`) |
| Pack status | incomplete | **complete** |

Note: Verified count stayed at 24, but pack became complete by properly excluding
interactive options and resolving remaining unverified surfaces.

### Git Toplevel Exploration

Tested M32 prompts on git toplevel (all subcommands) to validate generalization:

| Metric | Count |
|--------|-------|
| Total surfaces | 488 |
| Verified (after 5 cycles) | 8 |
| Unverified | 457 |

LM correctly generates co-dependent argv patterns:
- `--dry-run` → `["git", "add", "--dry-run", "newfile.txt"]`
- `--force` → `["git", "add", "--force", "ignored.txt"]`
- `-A` → `["git", "add", "-A"]`

Prompts generalize well - LM understands git options need subcommand context.
Progress is slow due to surface count (488) but prompt guidance is effective.

### Changes Made

**Phase 1: Co-dependent guidance in all reason prompts**

Added to `behavior_reason_no_scenario.md`, `behavior_reason_initial_scenarios.md`,
`behavior_reason_outputs_equal.md`, `behavior_reason_outputs_equal_retry.md`:

```markdown
## Co-dependent options

Some options only work with specific actions or trailing arguments:

- **Modifier options** that modify another action: include the action
  `["action", "--modifier", "arg"]` not `["--modifier"]`

- **Options requiring values**: include realistic trailing arguments
  `["--option", "key", "value"]` not `["--option"]`

**"unknown option" doesn't mean the option is invalid** - it means the option
needs additional context. Check the option's description for clues like
"With get..." or "Requires action...".
```

**Phase 2: Sandbox limitations in behavior_base.md**

```markdown
**Sandbox limitations**: Setup runs in isolated sandbox with read-only home directory. Avoid:
- `--global` config operations (use `--local` or `--add` instead)
- Writing to `~` or user config files
- Network operations
```

### Key Insights

1. **Co-dependent guidance worked**: LM now generates `["get", "--all", "color.status"]`
   instead of bare `["--all"]`

2. **Sandbox awareness critical**: First attempt used `git config --global` which fails
   in sandbox. After adding sandbox limitations, LM generated sandbox-safe scenarios.

3. **Reason file scope matters**: Co-dependent guidance must be in ALL reason prompt
   files (`no_scenario`, `initial_scenarios`, `outputs_equal`, etc.) since different
   cycles use different reasons.

### Acceptance Criteria

| Criterion | Status |
|-----------|--------|
| Behavior prompt includes co-dependent guidance | done |
| Sandbox limitations documented | done |
| LM generates scenarios with correct argv | done |
| git config: unverified → 0 or excluded with reason | done |

### Regression Benchmarks

Prompt changes can silently break previously-working binaries. These benchmarks
establish coverage baselines that must not regress.

**When to run:**
- After modifying any `prompts/*.md` file
- After changing LM response parsing in `src/workflow/lm_*.rs`
- NOT needed for unrelated code changes

#### Benchmark Binaries

| Binary | Coverage Type | Why |
|--------|---------------|-----|
| `echo` | argument-only | Canary for basic argv parsing |
| `sort` | stdin transform | Validates stdin input + output assertions |
| `touch` | file assertions | Validates file_exists/file_contains |
| `wc` | stdin + args | Combined stdin and argument scenarios |
| `git config` | co-dependent | Validates M32 subcommand context prompts |
| `cp` | known-incomplete | Exercises outputs_equal/assertion_failed paths |

Note: `cp` intentionally tests failure modes (SELinux, reflink options) to ensure
exclusion logic doesn't regress.

#### Metrics Tracked

| Metric | Regression Condition |
|--------|---------------------|
| `verified` | Current < baseline |
| `complete` | Was true, now false |
| `excluded` | Unexpected increase (review manually) |

#### Scripts

Scripts in `tests/regression/`:

| Script | Purpose | Runtime |
|--------|---------|---------|
| `capture-baselines.sh` | One-time baseline capture | ~20min |
| `check-sanity.sh` | Re-evaluate existing packs (no LM) | ~10s |
| `check-full.sh [binary]` | Regenerate + compare (live LM) | ~3min/binary |

**Usage:**
```bash
# Capture baselines (one-time, commit results)
./tests/regression/capture-baselines.sh

# Quick sanity after code changes
./tests/regression/check-sanity.sh

# Full check after prompt changes (all binaries)
./tests/regression/check-full.sh

# Single binary for iteration
./tests/regression/check-full.sh echo
```

#### Baselines Captured

| Binary | Verified | Complete | Notes |
|--------|----------|----------|-------|
| echo | 3 | yes | Argument-only canary |
| sort | 33 | no | Many options, some auto-excluded |
| touch | 13 | yes | File assertions |
| wc | 12 | yes | Stdin + args |
| cp | 10 | no | SELinux/reflink gaps expected |
| git-config | 34 | yes | Co-dependent validation |

#### Acceptance Criteria (benchmarks)

| Criterion | Status |
|-----------|--------|
| Create `tests/regression/` scripts | done |
| Capture initial baselines | done |
| Sanity check passes | done |

---

## M31 — Git Verification: Scope Context Fix (done)

Goal: Fix the scope context bug discovered during git exploration, then verify
`git config` surfaces properly. Expose any remaining gaps for stateful binaries.

### Results

| Metric | Before Fix | After Fix |
|--------|------------|-----------|
| Total surfaces | 34 | 34 |
| Verified | 31 (false positives) | **24 (real)** |
| Unverified | 3 | 10 |
| Pack status | incomplete | incomplete |

Note: Verified count dropped because the fix exposed false positives from incorrect
scope context handling. The 10 unverified surfaces became the target for M32.

### Acceptance Criteria

| Criterion | Status |
|-----------|--------|
| Fix scope context bug | done |
| `git config` surfaces verified (non-interactive) | done (27/34 = 79%) |
| Document remaining gaps | done |

### Fix Applied

**Location**: `src/workflow/lm_response.rs` in `validate_responses()`

**Change**: Added `context_argv_map` parameter and prepended context_argv to LM-returned
argv when building scenarios. Call sites in `src/workflow/apply/lm_apply.rs` updated
to build the map from surface inventory.

**Before**: LM returns `["--list"]` → execution runs `["git", "--list"]` (wrong)
**After**: LM returns `["--list"]` → context prepended → `["git", "config", "--list"]` (correct)

### Remaining Unverified (7 surfaces)

These surfaces can't be tested in isolation - they're **co-option requirements**:

| Surface | Error | Required Pattern |
|---------|-------|------------------|
| `--all` | "unknown option" | Needs `git config get --all <key>` |
| `--append` | "unknown option" | Needs `git config --append <key> <value>` |
| `--bool-or-int` | "no action specified" | Needs action context |
| `--comment` | "requires a value" | Needs value argument |
| `--edit` | "not in a git directory" | Needs repo prereq |
| `--null` | "no action specified" | Needs action context |
| `--regexp` | "unknown option" | Needs `git config get --regexp` |

These aren't file/state prereqs - they're **argument composition requirements**.
The surface can't be exercised without specific co-arguments. The prereq system
handles setup (files, directories), not argument dependencies.

### Observability Assessment

With the fix in place, LM-generated scenarios correctly run `git config` commands.
Observations:

- **Read operations** (`--list`, `--get`, `--show-*`): `outputs_differ` sufficient
- **Write operations** (`--local`, `--global`): Would benefit from `file_contains` on
  `.git/config`, but LM correctly used argument-based verification
- **Format modifiers** (`-z`, `--null`, `--name-only`): Verified via output differences

For `git config`, existing assertions are sufficient. The remaining gaps are
argument composition, not observability.

### Future Work (M32+)

- **Co-option handling**: Extend LM prompt to understand surfaces that require
  specific co-arguments (e.g., `--all` only valid with `get` action)
- **Repo prereq**: Add git repo initialization as a prereq for repo-dependent options
- **Other git scopes**: Explore `git init`, `git add` for stateful patterns

---

## M30 — Unlock Remaining Coreutils (done)

Goal: Unblock 3 blocked binaries + complete 2 easy wins = **99+ coreutils complete**
(from 94) through targeted fixes to validation recovery, help-output detection, and
outputs_equal exhaustion handling.

### Results

| Status | Count | Binaries |
|--------|-------|----------|
| **Newly Complete** | 6 | du, ls, chown, ptx, rev, split |
| **Still Incomplete** | 4 | shred, chgrp, cp, test |
| **Total Complete** | 100 | (94 + 6 from M30) |

**Unblocked**: du, ls (were blocked by seed_path validation errors)
**Completed via help-output detection**: rev (all 4 help options)
**Completed via auto-exclude**: chown, ptx, split (outputs_equal exhaustion)

### Changes (all done)

1. **Validation → Runtime**: Deferred seed_path validation to SQL (`scenario_error`)
2. **Help-Output Detection**: Skip behavior verification for `--help`/`--version` options
3. **Auto-Exclude**: `AutoExclude` action after `OUTPUTS_EQUAL_WORKAROUND_CAP` reached

### Additional Improvements

- **Rich exclusion context**: `exclusion_note.rs` with contextual notes and evidence
- **Query fix**: `behavior_exit_code` captured even when stderr is empty
- **AutoExclude action**: Bypasses LM when surfaces stuck after max retries

### Remaining Incomplete

| Binary | Reason |
|--------|--------|
| shred | Requires elevated permissions |
| chgrp | Requires valid group IDs |
| cp | SELinux context, reflink filesystem |
| test | No parseable --help (shell builtin) |

---

## M29 — Exit Code Assertions (done)

Goal: Add `exit_code` assertion type for commands that signal via exit code
(e.g., `sort --check`, `test`, `grep`).

### Key Changes

- Added `{"kind": "exit_code", "expected": N}` assertion
- `requires_baseline()` returns false for exit_code assertions
- Query evaluates `exit_code = expected`

### Results

Comprehensive validation: **96/104 coreutils complete** (92.3%).
Remaining 8 incomplete due to outputs_equal, assertion_failed, no_scenario.

See M30 for follow-up work addressing remaining blockers.

---

## Completed Milestones (M0-M28)

Full details available via `git show <commit>:docs/MILESTONES.md`

| Milestone | Summary | Commit |
|-----------|---------|--------|
| M28 | LM-first scenario generation (skip wasteful bare auto-verify) | `7f0ce3a` |
| M27 | Stdin input support for text filters (tr, cut, sort) | `c9a72fd` |
| M26 | File-based assertions (file_exists, dir_exists, file_contains) | `c4ef716` |
| M25 | Coreutils validation (10 binaries, 9 verified) | `7416e8e` |
| M24 | DuckDB performance (12x query speedup, caching) | `5990cd5` |
| M23 | Prompt consolidation (extract to markdown files) | `677480a` |
| M22 | Testing infrastructure (mock LM, fixtures, E2E tests) | `5927900` |
| M21 | TUI redesign with LM transparency (Work/Log/Browse tabs) | `5725ea1` |
| M20 | LM-driven prereq and fixture generation | `067af4a` |
| M19 | Pack-owned verification semantics | `930d25b` |
| M18 | End-to-end LM agent validation | `9c1d5df` |
| M17 | Behavior authoring ergonomics simplification | `b734b4b` |
| M16 | Surface definition v2 + behavior verification suite | `c304136` |
| M15 | Batched auto-verification (subcommand existence) | `72985d0` |
| M14 | Batched auto-verification (options existence) | `f874ab6` |
| M13 | Verification triage + verification by default | `a2358f4` |
| M12 | Pack-owned semantics v1 | `f0802b2` |
| M11.1 | Scenario loop rough-edge smoothing | `14b76e2` |
| M11 | Execution-backed verification v1 | `14b76e2` |
| M10 | Scenario-only evidence + coverage v1 | `e3a22d1` |
| M9 | Enrich v1 (JSON-only, validate/lock, evidence-first) | `bdf8861` |
| M8 | Broad dynamic validation (deferred, folded into M9) | `a6760ab` |
| M7 | Portable doc packs | `76dba64` |
| M6 | Scenario-backed EXAMPLES | `b42c57f` |
| M5 | Comprehensive ls(1) man page | `0f729f1` |
| M4 | Provenance bundle | `12bafca` |
| M3 | LM man page pipeline | `05d758c` |
| M2 | Evidence extraction | `9aa8b7b` |
| M1 | Pack ingest | `171c7d7` |
| M0 | Static reset | `32b08b7` |
