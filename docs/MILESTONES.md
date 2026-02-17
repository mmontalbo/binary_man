# Milestones

This document tracks the static-first roadmap for generating man pages from
`binary_lens` packs. Dynamic execution is optional and used for scenario-backed
validation, coverage tracking, and (eventually) a structured "enrichment loop"
that supports iterative static + dynamic passes from portable doc packs.

Current focus: M31 (git deep-dive for stateful verification patterns).

## M31 — Git as Stateful Verification Case Study

Goal: Use `git` to **discover gaps** in the verification system that don't surface
with stateless coreutils. Exploration milestone — deliverable is a findings section
(below), not code changes.

### Motivation

Coreutils are mostly stateless (input → output). Git is stateful:
- Surfaces depend on prior state (files, directories, config)
- Many surfaces mutate state rather than producing stdout
- Behavior depends on environment state, not just arguments

### Scope

- **In scope**: `git config` surfaces (simplest stateful case, ~34 surfaces)
- **Out of scope**: Implementing fixes (M32+), other git scopes (add, commit, branch)

### Key Questions

1. **Prereq complexity**: Can LM infer multi-step prereqs for stateful binaries?
2. **Observability**: Can existing assertions verify effects beyond stdout/stderr/exit_code?

### Approach

1. Attempt `git config` verification
2. Document mechanical blockers encountered
3. Categorize gaps: prereq inference, observability
4. Propose M32 scope based on findings

### Acceptance Criteria

| Criterion | Status |
|-----------|--------|
| Attempt verification, document blockers | done |
| Categorize gaps with examples | done |
| Propose M32 plan | done |

### Findings

#### Mechanical Blockers

**Scope context missing from LM scenarios**: For scoped packs (e.g., `git config`),
the LM-generated scenarios don't include the scope context:

| Scenario Type | Argv | Result |
|---------------|------|--------|
| baseline | `["git"]` | Shows git usage (exit 1) |
| verify_--list | `["git", "--list"]` | "unknown option" (exit 129) |
| auto_verify::--list | `["git", "config", "--list"]` | Correct (exit 0) |

The `auto_verify` system knows about scope context and includes it. LM scenarios don't.
All 31 "verified" surfaces are **false positives** - they show `delta_seen` because
both baseline and verify fail differently, not because the option was tested.

**Root cause**: The LM prompt doesn't communicate scope context. The LM returns
`["--list"]`, execution prepends binary → `["git", "--list"]`. Missing: `config`.

**Location**: Prompt building in `src/workflow/lm_client.rs` needs to include
scope context, OR scenario execution needs to insert it.

#### Prereq Gaps

**Usage pattern dependencies**: Some surfaces can't be tested in isolation:

| Surface | Issue | Required Pattern |
|---------|-------|------------------|
| `--all` | "unknown option" | Needs `git config get --all <key>` |
| `--append` | "unknown option" | Needs `git config --append <key> <value>` |
| `--system` | "no action specified" | Needs action: `--system --list` |

These aren't prereqs in the "need files/state" sense - they're **co-option requirements**.
The surface can't be exercised without other arguments. Current prereq system handles
file/directory setup, not argument dependencies.

**Note**: Due to scope context bug (above), we can't fully evaluate prereq inference.
The LM never got a chance to reason about git-specific state requirements.

#### Observability Gaps

**Assessment blocked**: Due to scope context bug, no scenarios actually ran `git config`.
All "verified" surfaces used `outputs_differ` comparing wrong commands.

**Observed pattern**: All LM-generated scenarios used `outputs_differ` assertion.
For `git config` read operations, this would be sufficient (stdout shows values).
For write operations, `file_contains` on `.git/config` would work.

**Hypothesized gaps for other git scopes** (not tested due to blocker):

| Scope | Operation | Current Assertions | Potential Gap |
|-------|-----------|-------------------|---------------|
| `git add` | Stage file | stdout (empty on success) | Need `file_staged` or git-status check |
| `git commit` | Create commit | stdout (commit message) | `outputs_differ` likely sufficient |
| `git branch` | Create branch | stdout | `ref_exists` might help |

**Conclusion**: Can't fully evaluate observability gaps without fixing scope context bug.
For `git config` specifically, existing assertions appear sufficient.

### M32 Proposal

Based on M31 findings, M32 should focus on **fixing the scope context bug**.

**Priority 1 - Scope context in LM scenarios**:
- Problem: LM scenarios don't include scope context (e.g., `config` in `git config`)
- Impact: ALL scoped pack verification is broken (false positives)
- Fix: Either inject context in prompt, or insert context during execution

**Priority 2 - Co-option requirements** (if time permits):
- Problem: Some surfaces need other arguments (`--all` needs `--get`)
- Impact: 3 of 34 surfaces can't be verified in isolation
- Fix: Add `requires_argv` inference or document as limitation

**Not priority for M32**:
- New assertion types (observability): Existing assertions appear sufficient for `git config`
- State-mutation verification: Blocked by P1, defer to M33+

**Success criteria**: Re-run `git config` verification with fixed scope context,
achieve actual (not false positive) verification of surfaces.

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
