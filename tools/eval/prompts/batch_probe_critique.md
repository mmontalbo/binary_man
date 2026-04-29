# Task: Add semantic critique to batch probe verifications

## Problem

bman's batch probe (`src/verify/bootstrap.rs:1453-1591`) verifies surfaces by running the binary with and without each flag against a pre-built "rich fixture." If outputs differ, the surface is marked Verified at cycle 0 with no further validation.

The LM-driven verification path has a **critique step** (`src/verify/critique.rs`) that asks an LM to review each newly-verified surface and determine whether the output difference actually demonstrates the documented behavior. Surfaces that fail critique are demoted back to Pending with feedback. This catches false positives like:
- Flag causes an error (changes stderr) but doesn't exercise its documented function
- Flag produces empty output (matched nothing) vs control producing output
- Output differs due to non-determinism (timestamps, PIDs) rather than the flag's behavior

Batch probe skips this critique entirely (`apply_batch_probe_hits` in `bootstrap.rs:1594-1640` writes `Status::Verified` directly). This means batch-probe-verified surfaces have lower confidence than LM-verified surfaces.

## Evidence gap

Batch probe currently stores `evidence_path: "batch_probe"` (a literal string, not a file path). No evidence JSON files are written to the pack directory. The critique module's `read_evidence_outputs` (critique.rs:239) reads from `pack_path.join(evidence_path)`, so calling critique on batch-probe surfaces today would produce **empty diffs and empty previews** — the critique LM would have nothing to judge.

The Attempt struct does preserve `stdout_preview` and `control_stdout_preview`, but these are truncated to 200 chars (bootstrap.rs:1564-1573), far less than the 800-char previews and 1500-char diffs the critique prompt normally uses.

**Resolution**: Batch probe must write evidence files before critique can run. `run_in_sandbox` (evidence.rs:555) already returns `Result<Evidence>` — the same struct used by `write_evidence`. The Evidence objects are already in hand at bootstrap.rs:1503 (control) and bootstrap.rs:1522 (option); they're just discarded after extracting 200-char previews. The fix is to retain and write them, not "construct" new ones.

## Implementation: two-phase approach

Rather than three separate experiments, this is structured as two phases — the first produces the data, the second analyzes it.

### Phase 1: Pipeline integration

**Goal**: Make batch probe write evidence files and pass verified surfaces through critique.

**Changes** (in order):

1. **Add `pack_path` parameter to `batch_probe_surfaces`** (bootstrap.rs:1453):
   - Change signature: `fn batch_probe_surfaces(state: &State, pack_path: &Path, verbose: bool) -> Vec<BatchProbeHit>`
   - Update call site in run.rs:634: `let hits = batch_probe_surfaces(&state, pack_path, verbose);`

2. **Add `evidence_path` field to `BatchProbeHit`** (bootstrap.rs:1441):
   - Add `pub evidence_path: String` to the struct

3. **Write evidence files in the probe loop** (bootstrap.rs:1516-1582):
   - The control `Evidence` is already captured at line 1503 via `run_in_sandbox`. Retain it for the loop.
   - Each option `Evidence` is captured at line 1522 via `run_in_sandbox`. Currently discarded after preview extraction.
   - **Insertion point**: After all mechanical filters pass (after line 1555), before `BatchProbeHit` construction (line 1575):
     ```
     let sanitized = sanitize_id(surface_id);
     let evidence_path = format!("evidence/batch_probe_{}.json", sanitized);
     let control_evidence_path = format!("evidence/batch_probe_{}_control.json", sanitized);
     write_evidence(pack_path, &evidence_path, &evidence)?;  // option
     write_evidence(pack_path, &control_evidence_path, &control)?;  // control (cloned per hit)
     ```
   - Control evidence is duplicated per-hit (~10KB × N hits). For typical batch probe (50-100 hits), this is ~1MB — negligible. This is simpler and more correct than symlinks or shared files, because it matches the convention `read_evidence_outputs` expects (`evidence_path.replace(".json", "_control.json")` at critique.rs:241).
   - Note: `batch_probe_surfaces` currently returns `Vec<BatchProbeHit>` and is not fallible. Adding `write_evidence` (which returns `Result`) means either making the function fallible or using `let _ = write_evidence(...)` to silently skip failures. Since evidence writing is best-effort (batch probe still works without it), the silent-skip approach is appropriate: `if let Err(e) = write_evidence(...) { if verbose { eprintln!(...) } }`.

4. **Use real evidence path in `apply_batch_probe_hits`** (bootstrap.rs:1594-1640):
   - Change `evidence_path: "batch_probe".to_string()` (line 1607) to `evidence_path: hit.evidence_path.clone()`
   - Change return type to `Vec<String>` (the list of verified surface IDs)
   - Return the collected IDs for use by the critique call

5. **Add critique gate in run.rs** (between lines 654 and 657):
   - **Scoping**: The batch probe block is inside `if state.seed_bank.is_empty() && state.cycle == 0 { ... }` (line 629). The critique call must be outside this block (after characterization). Declare `batch_verified_ids` before the if block:
     ```rust
     let mut batch_verified_ids = Vec::new();
     if state.seed_bank.is_empty() && state.cycle == 0 {
         eprintln!("PROGRESS: phase=batch_probe surfaces={}", state.entries.len());
         let hits = batch_probe_surfaces(&state, pack_path, verbose);
         batch_verified_ids = apply_batch_probe_hits(&mut state, hits, verbose);
     }
     ```
   - After characterization completes (line 654), insert:
     ```rust
     if !batch_verified_ids.is_empty() {
         eprintln!("PROGRESS: phase=batch_critique surfaces={}", batch_verified_ids.len());
         critique_surfaces(&mut state, pack_path, lm_config, verbose, &batch_verified_ids)?;
         eprintln!("PROGRESS: phase=batch_critique_done");
     }
     ```
   - LM is already available at this point (characterization uses it). No initialization changes needed.

### Smoke test (before full eval)

Before launching the full eval matrix, verify the integration with a single quick run:
```bash
cargo build --release
bman ls -v  # single binary, verbose
```
Check:
- `evidence/batch_probe_*.json` files appear in the pack directory
- Stderr shows `PROGRESS: phase=batch_critique surfaces=N` and `PROGRESS: phase=batch_critique_done`
- Surfaces with `critique_feedback` appear in state.json (if any were demoted)
- No panics or errors in the critique phase

### Phase 2: Analysis

**Goal**: Measure the false positive rate and critique quality.

**Eval config**:

```toml
# tools/eval/configs/e9_batch_critique.toml
name = "exp_e9_batch_critique"
parallel_groups = true

[defaults]
runs = 3
max_cycles = 80

[[groups]]
lms = ["claude:haiku"]
binaries = [["ls"], ["grep"], ["git", "diff"]]
timeout = 600
```

Note: `runs = 3` (not 1) to account for known run variance from batch starvation. Batch probe hits are deterministic across runs (same fixture, same binary, same filters → same hits). But critique calls an LM, which is non-deterministic — demotion sets may vary across runs. Report the false positive rate as a range (min/max across runs), not a single number.

**Measurements**:
1. **False positive rate**: For each cell, count batch-probe surfaces demoted by critique vs total batch-probe verified. Report per-binary as a range across runs: `{binary}: {N} batch probe verified, {M_min}-{M_max} demoted ({pct_min}-{pct_max}%)`
2. **Critique precision**: Manually inspect all DEMOTE decisions across all cells. For each, read the evidence files in the inspector and judge: was the demotion correct? Target: >80%
3. **Recovery rate**: Of surfaces demoted by critique at cycle 0, how many were re-verified by the LM during the main loop? High recovery = critique was right to demote (the LM found a better seed). Low recovery = either critique was wrong, or the surface is genuinely unverifiable with this approach
4. **Regression detection**: Compare against baseline (corpus_v0 or a no-critique run). Identify surfaces that were Verified in the baseline but Excluded in the critique run. These are regressions — critique demoted a valid verification and the LM failed to recover it. Any regressions need manual review to determine if critique or the LM was at fault.
5. **Time impact**: Compare total elapsed time against a baseline run without critique (use existing corpus_v0 data if available)

**Extracting batch-probe metrics**: The eval harness doesn't distinguish batch-probe surfaces from LM-verified ones. Use `jq` on `state.json` to filter:
```bash
# Count batch-probe surfaces demoted by critique
jq '[.entries[] | select(.attempts[0].cycle == 0 and .critique_feedback != null)] | length' state.json

# List demoted surface IDs and reasons
jq '.entries[] | select(.attempts[0].cycle == 0 and .critique_feedback != null) | {id, critique_feedback}' state.json

# Count batch-probe surfaces that survived critique
jq '[.entries[] | select(.attempts[0].cycle == 0 and .status == "Verified")] | length' state.json
```
Use the inspector TUI for detailed spot-checking of individual surfaces.

**Decision thresholds**:
- If batch probe false positive rate < 5%: critique adds cost without meaningful quality gain — consider keeping it off or making it optional
- If false positive rate 5-20%: critique justified, ship it
- If false positive rate > 20%: critique essential, also investigate whether mechanical filters in batch probe need strengthening
- If critique precision < 70%: prompt needs tuning before shipping
- If critique precision 70-80%: acceptable, but note as area for improvement
- If critique precision > 80%: ship as-is

**Comparison methodology**: Run the same matrix WITHOUT the critique change (or use existing corpus_v0 data) as the control. Compare verified counts, cycle distributions, and final verification rates. The key metric is: does critique improve end-state quality (fewer false positives in final verified set) without regressing genuine verification rate?

## Known confounders to monitor

1. **Prediction=None bias**: Batch probe Attempts have `prediction: None` and `prediction_matched: None` (bootstrap.rs:1618-1620). The critique prompt will show "Prediction: None (no prediction made)" for all batch probe surfaces. The critique LM may treat absent predictions as a negative signal. During spot-checking, compare critique DEMOTE rates between batch-probe surfaces (no prediction) and LM-verified surfaces (has prediction) to see if this biases decisions.

2. **Non-stdout diff types**: Batch probe produces `DiffKind::Stderr` and `DiffKind::ExitCode` hits. The critique prompt's primary evidence is a unified stdout diff (critique.rs:162-169), which will be empty for these. The prompt does include exit codes (line 155-159) and stderr (line 185-189), but its preamble emphasizes "output difference." During spot-checking, specifically examine critique decisions on non-stdout hits to verify the prompt handles them correctly.

3. **Exclusion threshold aggressiveness**: `CRITIQUE_EXCLUSION_THRESHOLD=2`. A batch-probe surface starts Verified (cycle 0), gets demoted (1 demotion), the LM re-verifies, critique demotes again (2 demotions → Excluded). That's only 2 total verification attempts before permanent exclusion. LM-verified surfaces typically accumulate more attempts before first verification. If many batch-probe surfaces are excluded with only 2 attempts, this threshold may be too aggressive for batch-probe-originating surfaces. Monitor exclusion counts and consider whether batch-probe demotions should count separately.

## Relevant code paths

- **Batch probe**: `src/verify/bootstrap.rs` — `batch_probe_surfaces()` (line 1453), `apply_batch_probe_hits()` (line 1594), `BatchProbeHit` struct (line 1441)
- **Evidence**: `src/verify/evidence.rs` — `run_in_sandbox()` (line 555, returns `Evidence`), `write_evidence()` (line 845), `Evidence` struct (line 258), `sanitize_id()` (line 885)
- **Critique module**: `src/verify/critique.rs` — `critique_surfaces()` (line 26), `build_prompt()` (line 133), `read_evidence_outputs()` (line 239)
- **Critique config**: `src/verify/config.rs` — `CRITIQUE_EXCLUSION_THRESHOLD` (2), `CRITIQUE_BATCH_SIZE` (10), `CRITIQUE_OUTPUT_MAX_LEN` (1500)
- **Critique types**: `src/verify/types.rs` — `SurfaceEntry.critique_feedback` (line 153), `critique_demotions` (line 158)
- **Pipeline orchestration**: `src/verify/run.rs` — batch probe at line 629-636, characterization at line 638-654, experiment_params at line 657, pipeline start at line 693; inline critique at line 1813

## Inspector support

The inspect TUI (`tools/inspect/`) can be used to examine surfaces post-experiment. Look for:
- Surfaces at cycle 0 with `critique_feedback` set (these were batch-probe-verified then demoted)
- The critique feedback text explains why the demotion happened
- Compare the control/option outputs in the attempt JSON to understand if the demotion was justified
- With evidence files now written, the full stdout/stderr/exit_code data is available for each batch probe surface

## Critique LM selection

The critique step uses the same `lm_config` as the verification pipeline. In the eval config above, this is `claude:haiku`. This means haiku is both the verifier and the critic for LM-driven surfaces. For batch-probe surfaces, haiku is only the critic (batch probe is mechanical).

If critique precision is low, consider testing with a stronger critique LM. This would require either:
- A separate `critique_lm` config field (not currently supported)
- Running the experiment with a stronger LM in the `lms` field (which also changes the verification LM)

For the initial experiment, using the same LM for both is fine — it matches how critique already works for LM-verified surfaces.

## Success criteria

1. **Phase 1**: Pipeline integration compiles, batch probe writes evidence files, critique runs on batch-probe surfaces without errors
2. **Phase 2**: Concrete false positive rate measured per-binary, critique precision > 80%, clear go/no-go decision on shipping critique for batch probe

## Notes

- The batch probe rich fixture is built by `build_rich_fixture()` in `bootstrap.rs` — it includes diverse files (hidden files, symlinks, binary files, nested dirs, etc.) designed to trigger many flags
- Some batch probe hits are already filtered: option-errored, empty-stdout-with-nonempty-control (see lines 1540-1555 in bootstrap.rs)
- Characterization logging was recently added (`characterize.rs`) — batch probe verifications log to `lm_log/` as cycle 0 attempts in state.json
- Use `sanitize_id` (evidence.rs:885) for file paths: strips leading dashes, replaces non-alphanumeric chars (e.g., `--color=auto` → `color_auto`)
