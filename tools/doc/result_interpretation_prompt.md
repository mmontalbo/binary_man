You are writing behavioral documentation for the `{{BINARY}}` command-line tool based on systematic observation data.

The data below was collected by running `{{BINARY}}` across multiple input states and recording what happened. Each "run" line shows the command arguments, how many distinct behaviors were observed across input contexts, and which input perturbations caused behavior changes.

## How to read the results

- **`N/M distinct`**: N unique behaviors observed across M input contexts. `1/8 distinct` = identical behavior everywhere. `5/8 distinct` = behavior varies by input.
- **`sensitive to: X`**: perturbation X caused a different observation. `(-N lines)` means N fewer output lines. `(exit 0->1)` means exit code changed.
- **`exit {0,1}`**: different exit codes observed across contexts.
- **`stdout empty`** / **`stdout not empty`**: whether the command produced output.
- **`modifies filesystem`**: the command created, deleted, or changed files.
- **`from "ref"`**: this run was diffed against a reference run. Diff details show what's only in this run vs the reference.

## Observation data

```
{{RESULTS}}
```

## Your task

Write a concise behavioral reference for `{{BINARY}}` based ONLY on the observation data above. Structure it like a man page with these sections:

**NAME** — one-line description.

**SYNOPSIS** — usage pattern.

**FLAGS** — for each observed flag, one paragraph: what it does (based on observed behavior, not guessing), exit code behavior, sensitivity to input variations. Group related flags.

**EXIT STATUS** — observed exit codes and what triggers them.

**NOTES** — any interesting behavioral observations: flags that had no observable effect, surprising sensitivities, flags that always produce identical output regardless of input.

Rules:
- CRITICAL: Only describe behavior visible in the observation data. Do NOT use your prior knowledge of what flags do. If `-M` produces the same output order as the base invocation, say "produced identical ordering to base" — do NOT say "sorts by month names" just because you know that's what -M normally does.
- For each flag, compare its actual stdout lines against the base invocation's stdout. Quote 2-3 output lines that show the difference. If there is no difference, say so explicitly.
- If a flag produced identical results to the base invocation, say so — do not invent a difference.
- Keep it concise. One paragraph per flag or flag group, not per observation.

Output only the documentation. No preamble or meta-commentary.
