# Task: Add semantic critique to batch probe verifications

## Problem

bman's batch probe (`src/verify/bootstrap.rs:1453-1591`) verifies surfaces by running the binary with and without each flag against a pre-built "rich fixture." If outputs differ, the surface is marked Verified at cycle 0 with no further validation.

The LM-driven verification path has a **critique step** (`src/verify/critique.rs`) that asks an LM to review each newly-verified surface and determine whether the output difference actually demonstrates the documented behavior. Surfaces that fail critique are demoted back to Pending with feedback. This catches false positives like:
- Flag causes an error (changes stderr) but doesn't exercise its documented function
- Flag produces empty output (matched nothing) vs control producing output
- Output differs due to non-determinism (timestamps, PIDs) rather than the flag's behavior

Batch probe skips this critique entirely (`apply_batch_probe_hits` in `bootstrap.rs:1594-1640` writes `Status::Verified` directly). This means batch-probe-verified surfaces have lower confidence than LM-verified surfaces.

## Proposed experiments

### Experiment 1: Audit existing batch probe verifications

**Goal**: Quantify how many batch-probe-verified surfaces would fail critique.

**Method**:
1. Take the existing `corpus_v1` experiment data (or run a fresh one)
2. For each cell, identify surfaces verified at cycle 0 (batch probe)
3. Run the critique prompt (`critique.rs:build_critique_prompt`) on each batch-probe-verified surface, passing the control/option stdout previews
4. Count how many the critique LM would DEMOTE vs ACCEPT
5. Manually spot-check 10 demoted surfaces to verify the critique was correct

**Output**: A report showing `{binary}: {N} batch probe verified, {M} would be demoted ({pct}%)` per cell. This tells us the false positive rate.

**Implementation**: This can be a standalone script that reads `state.json` + evidence files and calls the critique LM, without modifying the pipeline. Use the existing `build_critique_prompt` function or replicate its logic.

### Experiment 2: Add critique to batch probe pipeline

**Goal**: Integrate critique into batch probe and measure impact on verification rates and quality.

**Method**:
1. Modify `apply_batch_probe_hits` in `bootstrap.rs` to collect batch probe hits as "newly verified" and pass them through `critique_surfaces()` (same as LM-driven verifications)
2. Run the same corpus_v0 matrix (3 LMs × 3 binaries) with critique enabled for batch probes
3. Compare:
   - Total verified count (before vs after)
   - Surfaces demoted by critique (which ones, why)
   - Whether demoted surfaces get re-verified by the LM later (recovery rate)
   - Overall cycle count / time impact

**Key implementation detail**: The critique step requires an LM call. Batch probe currently runs without any LM (it's mechanical). Adding critique means batch probe now depends on an LM being available. Options:
- Run critique as part of the first verification cycle (after characterization, before cycle 1)
- Run critique inline during `apply_batch_probe_hits` (requires LM plugin to be initialized earlier)
- Run critique as a post-processing step before the main loop starts

The cleanest approach is probably: after `apply_batch_probe_hits`, collect the batch-verified surface IDs and call `critique_surfaces` before the main verification loop starts. This keeps batch probe mechanical but adds a critique gate afterward.

### Experiment 3: Validate critique quality

**Goal**: Ensure the critique LM isn't being too aggressive (demoting valid verifications) or too lenient (accepting false positives).

**Method**:
1. From Experiment 2's results, take all DEMOTE decisions
2. For each demotion, manually verify: was the original batch probe result actually a false positive?
3. Also take a sample of ACCEPT decisions and verify they're genuine
4. Calculate precision (demotions that were correct) and recall (false positives that were caught)

## Relevant code paths

- **Batch probe**: `src/verify/bootstrap.rs` — `batch_probe_surfaces()` (line 1453) and `apply_batch_probe_hits()` (line 1594)
- **Critique module**: `src/verify/critique.rs` — `critique_surfaces()` (line 14), `build_critique_prompt()` (line 109)
- **Critique config**: `src/verify/config.rs` — `CRITIQUE_EXCLUSION_THRESHOLD` (2), `CRITIQUE_BATCH_SIZE` (10), `CRITIQUE_OUTPUT_MAX_LEN` (1500)
- **Critique types**: `src/verify/types.rs` — `SurfaceEntry.critique_feedback` (line 153), `critique_demotions` (line 158)
- **Main loop integration**: `src/verify/run.rs` — critique called at line 1813 after newly verified surfaces
- **Prompt feedback**: `src/verify/prompt.rs` — critique feedback included at line 209

## Eval config for experiments

Use the existing eval harness (`tools/eval/`). Create configs like:

```toml
# tools/eval/configs/e9_batch_critique_audit.toml
name = "exp_e9_batch_critique_audit"
parallel_groups = true

[defaults]
runs = 1
max_cycles = 80

[[groups]]
lms = ["claude:haiku"]
binaries = [["ls"], ["grep"], ["git", "diff"]]
timeout = 600
```

## Inspector support

The inspect TUI (`tools/inspect/`) can be used to examine surfaces post-experiment. Look for:
- Surfaces at cycle 0 with `critique_feedback` set (these were batch-probe-verified then demoted)
- The critique feedback text explains why the demotion happened
- Compare the control/option outputs in the attempt JSON to understand if the demotion was justified

## Success criteria

1. **Experiment 1**: We have a concrete false positive rate for batch probe across ls/grep/git-diff
2. **Experiment 2**: Critique integrated, verification rates measured, no regressions in genuine verifications
3. **Experiment 3**: Critique precision > 80% (most demotions are correct), recall reasonable (catches obvious false positives)

## Notes

- The batch probe rich fixture is built by `build_rich_fixture()` in `bootstrap.rs` — it includes diverse files (hidden files, symlinks, binary files, nested dirs, etc.) designed to trigger many flags
- Some batch probe hits are already filtered: option-errored, empty-stdout-with-nonempty-control (see lines 1540-1555 in bootstrap.rs)
- Characterization logging was recently added (`characterize.rs`) — batch probe verifications log to `lm_log/` as cycle 0 attempts in state.json
