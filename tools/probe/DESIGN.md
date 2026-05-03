# Probe — Design Principles

## Purpose

Observe CLI binary behavior by running invocations across varied input states.
One user-edited file (`.probe`) declares what to run. One tool-generated file
(`.results`) records what happened.

## Three layers

**Layer 1 — Execution context.** What the binary receives: filesystem state,
environment, arguments, stdin. Declared in `.probe` files. User-written.

**Layer 2 — Observations.** What the binary produced: stdout, stderr, exit
code, filesystem changes. Recorded in `.results` files. Tool-generated.

**Layer 2.5 — Computed summaries.** Derived from observations: diffs between
invocations, sensitivity to perturbations, universal properties. Auto-computed
by the tool, included in `.results`.

**Layer 3 — Analysis.** Interpretation of observations: documentation
generation, regression detection, behavioral clustering. Performed by external
consumers (humans, LMs, scripts) reading the data.

## Language

Five keywords build the probe file:

- **context** — declares input state (extends, remove for derivation)
- **vary** — generates perturbation variants of a context
- **invoke** — runs the binary during context setup (output discarded)
- **run** — declares an invocation to observe (output recorded)
- **from** — groups runs for diff comparison against a reference
- **in** — scopes runs to specific contexts (block or modifier)

`invoke` and `run` both execute the binary. `invoke` builds state (inside
context blocks). `run` observes behavior (outside context blocks). Different
keywords because different semantics.

## Design constraints

**Binary-agnostic.** The language knows nothing about any binary. No flag
lists, no command-specific predicates. The same language tests ls, grep, sort,
cp, git, ffmpeg.

**Two files.** The user edits `.probe`. The tool generates `.results`. No tool
output in the user's file. No user content in the tool's file. Clean separation.

**The grid is the model.** Input states × invocations = cells. Each cell
produces an observation. Contexts define the rows. Runs define the columns. The
tool fills in every cell.

**From blocks declare comparison relationships.** Diffs are computed between
runs that the user explicitly groups, not between arbitrary pairs. No assumed
baseline. No heuristic about which runs to compare.

**In blocks scope without repetition.** When multiple runs share a context
scope, `in` groups them — no per-run repetition. `in` and `from` compose by
nesting.

**Collapsing reveals sensitivity.** When multiple contexts produce identical
observations, they collapse into one group. The contexts that DON'T collapse
are the sensitive ones. Sensitivity is auto-computed from the collapsing
pattern.

## What the tool computes

For each run across all applicable contexts:

1. **Observations** — stdout, stderr, exit code, filesystem changes
2. **Collapsing** — group contexts with identical observations
3. **Sensitivity** — which vary perturbations changed the output
4. **Universals** — properties consistent across all contexts
5. **Diffs** �� for from-block members, line-level comparison vs reference

All of this is written to the `.results` file. The tool adds no other analysis.
Further interpretation is the reader's job.

## What the tool does NOT do

- No expectations or assertions (layer 3 is external)
- No binary-specific knowledge
- No query language
- No network support
- No subcommands — one invocation reads and runs everything
