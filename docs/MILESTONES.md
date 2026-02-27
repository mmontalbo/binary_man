# Milestones

This document tracks the static-first roadmap for generating man pages from
`binary_lens` packs. Dynamic execution is optional and used for scenario-backed
validation, coverage tracking, and (eventually) a structured "enrichment loop"
that supports iterative static + dynamic passes from portable doc packs.

Current focus: M36.

## M36 — Targeting State Machine Hardening (done)

Goal: Fix targeting bugs that prevented complex binaries (git log) from reaching
full coverage.

### Problem

Fresh e2e test for `git log` achieved only 17% coverage (17/102 verified) despite
previous runs achieving 97%. Investigation revealed three targeting bugs:

1. **Initial phase exit too early**: `is_initial_behavior_cycle()` returned false
   after ANY behavior scenarios existed, not after all surfaces were covered
2. **Duplicate targeting**: Same surfaces targeted repeatedly because no
   deduplication of already-covered surfaces
3. **MVE blocking**: `missing_value_examples` surfaces blocked by priority
   ordering and single-target batching

### Results

| Metric | Before | After R1 | After R2 | After R3 |
|--------|--------|----------|----------|----------|
| Verified | 17 | 28 | 80 | **99** |
| no_scenario | 80 | 41 | 1 | 0 |
| MVE | 4 | 4 | 19 | **0** |
| Rate | 17% | 27% | 78% | **97%** |

### Fixes Applied

| Round | Fix | Impact |
|-------|-----|--------|
| R1 | Check all surfaces covered, not just "any scenarios exist" | +11 verified |
| R2 | Filter already-covered surfaces from targeting | +52 verified |
| R3 | Batch MVE surfaces + reorder priority | +19 verified |

### Key Commits

- `3b625fb` targeting: fix behavior verification coverage for complex binaries
- `046ba92` refactor: extract helper functions from behavior targeting state machine
- `6269fa4` fix: improve missing_value_examples detection and verification priority
- `f85ef3e` apply: reset no-progress counter when reason kind changes

### Acceptance Criteria

| Criterion | Status |
|-----------|--------|
| git log reaches 90%+ verification | done (97%) |
| no_scenario < 5 after 30 cycles | done (0) |
| MVE surfaces get scenarios | done (0 remaining) |
| No regression on other binaries | done |

---

## M35 — Verification Stall Diagnostics (done)

Goal: Surface why each unverified option is stuck and what action to take.

### Problem

`bman status` currently shows aggregated counts without actionable detail:

```
verification: 78/101 behavior verified
```

This tells us nothing about:
- Which 23 are unverified?
- Why is each stuck?
- What should we do about each one?

During git log verification (M34 follow-up), we hit a plateau at 48% that required
manual investigation to diagnose:
- 16 options had **no scenario** (LM never attempted them)
- 3 options had **setup failures** (silent `git checkout main` errors)
- 2 options were **stuck** (3+ attempts, same `outputs_equal` outcome)

The data existed in scenario files but wasn't surfaced.

### Solution

`bman status` shows diagnostics automatically when pack is incomplete:

```
verification: 78/101 behavior verified

Unverified (23):

  NO_SCENARIO (16) - never attempted:
    --basic-regexp, --boundary, --first-parent, ...

  SETUP_FAILED (3) - setup command errored:
    --cherry: "git checkout main" → pathspec 'main' did not match
    --date-order: "git checkout main" → pathspec 'main' did not match

  OUTPUTS_EQUAL (2) - no observable difference:
    --abbrev, --no-abbrev

  STUCK (2) - 3+ attempts, same outcome:
    --show-signature: outputs_equal × 3
    --remotes: scenario_error × 3
```

JSON output includes `unverified_breakdown` array for automation:
```json
{
  "unverified_breakdown": [
    {"surface_id": "--cherry", "reason": "setup_failed", "attempts": 1,
     "context": {"cmd": "git checkout main", "stderr": "pathspec 'main'..."}}
  ]
}
```

### Design Decisions

1. **No new state for attempt tracking** - Count scenario files per surface
   (`inventory/scenarios/verify_--option-*.json`). If files are deleted, count
   resets - that's intentional (user wants fresh start).

2. **Show diagnostics automatically** - No `--verbose` flag. Complete pack gets
   terse output. Incomplete pack shows breakdown. Keeps common case clean.

3. **Setup tracking is the only new data** - Classification reads existing
   `delta_outcome` from verification cache. Only new capture is setup command
   results.

4. **JSON includes diagnostics** - `unverified_breakdown` array with
   `{surface_id, reason, attempts, context}`. Text output is formatted view.

### Changes

**1. Track setup command results** (`src/scenarios/types.rs`, `src/scenarios/run/exec.rs`)

```rust
pub struct SetupResult {
    pub command: Vec<String>,
    pub exit_code: i32,
    pub stderr: String,
}

// In ScenarioOutput:
pub setup_results: Vec<SetupResult>,
pub setup_failed: bool,
```

On any setup command failure: set `setup_failed = true`, skip main command,
store which command failed and why.

**2. Add `setup_failed` outcome** (`queries/.../10_behavior_assertion_eval.sql`)

```sql
WHEN scenario.setup_failed THEN 'setup_failed'
```

Distinct from `scenario_error` (schema/config issues).

**3. Build unverified breakdown** (`src/status/evaluate/verification_requirement/reasoning.rs`)

```rust
pub struct UnverifiedEntry {
    pub surface_id: String,
    pub reason: UnverifiedReason,
    pub attempts: u32,
    pub context: Option<UnverifiedContext>,
}

pub enum UnverifiedReason {
    NoScenario,
    SetupFailed,
    OutputsEqual,
    AssertionFailed,
    ScenarioError,
    Stuck,
}
```

Derive from:
- Scenario file count → `attempts`, `NoScenario` if 0
- `setup_failed` flag → `SetupFailed`
- `delta_outcome` from cache → `OutputsEqual`, `AssertionFailed`, `ScenarioError`
- 3+ attempts with same outcome → `Stuck`

**4. Surface in status output** (`src/status.rs`)

- Add `unverified_breakdown` to JSON output
- Add grouped text output when incomplete
- No change to output when complete

### Files Changed

| File | Change |
|------|--------|
| `src/scenarios/types.rs` | Add `SetupResult`, `setup_results`, `setup_failed` |
| `src/scenarios/run/exec.rs` | Capture setup command results |
| `queries/.../10_behavior_assertion_eval.sql` | Add `setup_failed` outcome |
| `src/status/evaluate/verification_requirement/reasoning.rs` | Add `UnverifiedEntry` builder |
| `src/status.rs` | Add `unverified_breakdown` to JSON, grouped text when incomplete |

### Acceptance Criteria

| Criterion | Status |
|-----------|--------|
| Setup failures captured with failing command + stderr | done |
| `setup_failed` distinct from `scenario_error` in query | done |
| `--json` includes `unverified_breakdown` array | done (`behavior_unverified_preview`) |
| Text output shows grouped breakdown when incomplete | done |
| Text output unchanged when complete | done |
| Attempt count derived from scenario file count (no new state) | done |

### Out of Scope

- Auto-exclusion of STUCK surfaces (future M36)
- Priority formatting in LM prompts (future)
- Cross-subcommand hint sharing (future)

---

## M34 — Post-Execution Behavior Judgment (done)

Goal: Meaningfully verify scenarios by adding a judgment step that checks whether
outputs actually demonstrate the documented behavior, not just that they differ.

### Problem

Current verification marks scenarios as "verified" when `outputs_differ` passes,
but doesn't check if the documented behavior was actually exercised:

```
--show-stash scenario: git init → run → outputs differ slightly → ✓ verified
Reality: No stash existed, no stash info shown, option behavior not triggered
```

The assertion `outputs_differ` is necessary but not sufficient.

### Approach

Add a post-execution judgment step where an LM answers a factual question:

> "Does this output demonstrate the behavior described for this option?"

The LM judges after seeing actual outputs, grounded in the option description.
Failed judgments feed back into retry prompts with concrete improvement suggestions.

### Workflow

```
1. LM proposes scenario (existing)
2. bman runs scenario (existing)
3. outputs_differ? NO → retry (no judgment needed)
4. outputs_differ? YES → call judge LM
5. Judge: demonstrates_behavior?
   - YES → VERIFIED
   - NO  → store feedback, retry (max 3)
6. Max retries exhausted → UNVERIFIABLE
```

### Judge Prompt

```markdown
## Option

Name: {{option_id}}
Description: {{description}}

## Output

{{variant_stdout}}

## Question

This option should: {{description}}

Does the output above demonstrate this? Answer YES or NO, then explain briefly.

If NO, what setup commands would trigger this behavior?

## Response Format

demonstrates_behavior: yes/no
reason: one sentence
suggested_setup: [list of commands] or null
```

### Retry Integration

When judgment fails, next behavior prompt includes:

```markdown
## Previous Attempt for {{option_id}}

Your scenario ran but did not demonstrate the expected behavior.

Judgment: "{{reason}}"
Suggested setup: {{suggested_setup}}

Please propose an improved scenario that addresses this feedback.
```

### Implementation

| File | Change |
|------|--------|
| `src/workflow/apply/judge.rs` | NEW: judgment LM call, progress tracking |
| `prompts/judge_behavior.md` | NEW: judge prompt template |
| `src/workflow/apply/mod.rs` | Insert judgment after `run_apply_single()` |
| `src/workflow/lm_client.rs` | Add judgment feedback to targets section |
| `src/enrich/types.rs` | Add `TargetJudgmentFeedback` struct |
| `src/status/evaluate/verification_requirement/next_action.rs` | Load judgment progress for retry prompts |

### Results

- **Judgment progress persisted**: `enrich/judgment_progress.json` tracks passed, pending_retry, unverifiable
- **Feedback integrated**: Failed judgments include reason and suggested_setup in retry prompts
- **Max 3 retries**: After 3 failures, surface marked unverifiable with accumulated reasons

### Success Criteria

| Criterion | Status |
|-----------|--------|
| Judge prompt implemented | done |
| Judgment call after outputs_differ | done |
| Failed judgments feed into retry | done |
| --show-stash requires "stash" in output | done (fails judgment correctly when no stash) |
| --ignored requires "Ignored" in output | done (passes judgment when "Ignored files:" present) |
| E2E: git status with meaningful verification | done (20/21 verified, 1 invalid option) |

---

## M33 — LM Semantic Enrichment (done)

Goal: Improve scenario generation by including option descriptions in prompts,
enabling LM to infer prerequisites from documentation.

### Results

- Added option descriptions to all behavior prompts
- Added prereq inference guidance to `behavior_base.md`
- Removed ~400 LOC of unused family infrastructure

**Note**: M33 achieved its goal (descriptions in prompts), but verification was
shallow (`outputs_differ` only). M34 addresses meaningful verification.

### Acceptance Criteria

| Criterion | Status |
|-----------|--------|
| Include descriptions in prompts | done |
| Add prereq guidance to behavior prompts | done |
| Remove unused family infrastructure | done |

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
