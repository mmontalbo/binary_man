# Milestones

This document tracks the static-first roadmap for generating man pages from
`binary_lens` packs. Dynamic execution is optional and used for scenario-backed
validation, coverage tracking, and (eventually) a structured "enrichment loop"
that supports iterative static + dynamic passes from portable doc packs.

Current focus: M31 (git deep-dive for stateful verification patterns).

## M31 — Git Deep-Dive: Stateful Verification Patterns

Goal: Use `git` as a test case for **stateful command verification**, exposing
gaps in prereq inference, assertion types, and subcommand handling that don't
surface with simple coreutils.

### Motivation

Coreutils are mostly stateless (input → output). Git is stateful:
- Commands depend on repository state (staged files, commits, branches)
- Many commands mutate state rather than producing output
- Subcommand hierarchy (`git config`, `git branch`, etc.) is complex
- Behavior depends on `.git/` state, not just arguments

### Key Questions to Answer

1. **Prereq Inference**: Can the LM reason "to verify `git commit -m`, I need staged changes first"?
2. **Assertion Types**: Is `stdout_contains` sufficient, or do we need `ref_exists`, `config_value_equals`?
3. **Subcommand Coverage**: Does surface lens handle `git config --get` vs `git config --list` as separate items?
4. **State Mutation Verification**: How do we verify `git add` staged a file vs just checking stdout?

### Expected Gaps

| Gap | Coreutils | Git |
|-----|-----------|-----|
| Prereq complexity | Seed files | Repo state (init, add, commit) |
| Assertion types | stdout/stderr/exit_code | file_staged, branch_exists, config_set |
| Dependency chains | Mostly independent | commit→add→files |
| Side effects | File creation | .git/ mutations |

### Approach

1. Start fresh with `git config` (simplest subcommand, stateless reads)
2. Progress to `git init`, `git add`, `git commit` (stateful)
3. Document mechanical gaps as they surface
4. Propose fixes for M32+

### Acceptance Criteria

| Criterion | Status |
|-----------|--------|
| `git config` subcommand fully verified | |
| Document prereq inference gaps | |
| Document assertion type gaps | |
| Propose M32 plan based on findings | |

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
