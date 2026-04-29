# Experiment eN: [Title]

**Date**: YYYY-MM-DD
**Commit**: [hash]
**Config**: `tools/eval/configs/eN_name.toml`

## Hypothesis

What we expected to be true and why we ran this experiment.

## Setup

- LM(s): [which models]
- Binaries: [which binaries]
- Runs: N per cell
- Max cycles / timeout
- Any code changes from mainline (dirty working tree, branch, etc.)

## Results

Key metrics table. Always include:
- Verification rate per binary (range across runs)
- Comparison to baseline (corpus_v0/v1 or prior experiment)
- The metric the experiment was designed to measure

## Root Cause / Analysis

What the data revealed. Include specific surfaces, code paths, and line numbers
where relevant. Distinguish between expected and surprising results.

## Lessons

Numbered list. Each lesson should be:
1. A general principle (not just "we fixed X")
2. Actionable for future work
3. Backed by specific data from this experiment

## Decisions

- **Ship**: changes committed with rationale
- **Drop**: changes abandoned with rationale
- **Next**: follow-up experiments or fixes identified

## Open Questions

Unresolved issues that didn't block this experiment but need future attention.
