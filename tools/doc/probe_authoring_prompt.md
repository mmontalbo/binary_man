You are writing a behavioral probe file for the `{{BINARY}}` command-line tool.

A probe file describes input states and invocations. A tool called bgrid executes every combination and records what happens (stdout, stderr, exit code, filesystem changes). Your job is to write a probe file that thoroughly explores the binary's behavior.

## Probe language reference

{{LANGUAGE_MD}}

## Discovery output

The following probe skeleton was auto-generated from `{{BINARY}} --help`. It gives you the flag names but makes poor design choices (no flag combinations, placeholder values). Use it as a starting point for flag names only.

```
{{DISCOVERY}}
```

## Your task

Write a single `.probe` file that comprehensively explores `{{BINARY}}`'s behavior.

**Size constraint**: Aim for 80-150 lines. Cover the important flags and combinations, not every possible permutation. Quality over quantity.

Follow these principles:

1. **Vary-centric design**: Create a base context with realistic input, then use `vary` blocks to perturb it. Keep file names stable across contexts so all runs can execute everywhere. Collapsing (grouping identical observations) is the tool's most powerful feature -- it only works when runs execute across multiple contexts.

2. **Realistic input content**: Choose file contents that exercise the binary's actual functionality. For a text tool, include numbers, mixed case, duplicates, blank lines, special characters. For a tool that operates on fields, include delimited data.

3. **Fix placeholder values**: The discovery skeleton has placeholders like `--key=keydef` or `--sort=word`. Replace these with actual valid values based on the binary's documentation.

4. **Flag combinations**: Test important flags in combination, not just individually.

5. **Use `from` blocks for same-format diffs**: Group related flags and diff them against a base invocation. A `from` block sets a sticky reference -- ALL subsequent runs get that reference until the next `context`, `vary`, `in`, or `from` keyword. To end a from block, start a new section.

6. **Use `in` blocks to scope runs**: When some runs only make sense in certain contexts (e.g., multi-file runs need a context with multiple files, or check-mode runs need sorted input), use `in "context_name"` to scope them. `in` also clears any active `from` reference.

7. **Error cases**: Include runs that should fail (nonexistent file, invalid input).

8. **Stdin**: If the binary reads stdin, test it with `stdin` lines.

## Syntax rules

- `run` arguments are passed directly to the binary. Each quoted string is one argument: `run "-k" "2,2"` not `run "-k 2,2"`.
- Only `stdin` and `in` are valid modifiers after a `run` line. Do NOT put `file`, `props`, or other setup commands after `run`.
- Do NOT use shell syntax (pipes, redirects like `>`, `|`, `&&`) in run arguments.
- `from` is a TOP-LEVEL block, not a run modifier. Do NOT nest `from` inside `run`. Structure like this:

```
# CORRECT: from is a top-level block with runs inside it
run "data.txt"

from "data.txt"
  run "-r" "data.txt"
  run "-n" "data.txt"

# This standalone run is NOT in any from block because `in` clears from scope
in "base"
  run "data.txt" "other.txt"

# Another from block for a different reference
from "data.txt"
  run "-u" "data.txt"
  run "-s" "data.txt"
```

```
# WRONG: do not nest from inside run
run "data.txt"
  from "data.txt"      # WRONG — from is not a run modifier
    run "-r" "data.txt" # WRONG — nesting
```

Output only the probe file contents. No explanation or commentary.
