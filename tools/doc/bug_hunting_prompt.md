You are writing a behavioral probe file to find bugs in the `{{BINARY}}` command-line tool.

A probe file describes input states and invocations. A tool called bgrid executes every combination and records what happens (stdout, stderr, exit code, filesystem changes). Your job is to write a probe that is likely to trigger bugs, inconsistencies, or unexpected behavior.

## Quick language reference

A `.probe` file has contexts (input states) and runs (invocations). `vary` creates perturbation variants. `from` groups runs for diff comparison. `in` scopes runs to specific contexts. `invoke` runs the binary during setup. See examples below.

## Discovery output

The following flags were discovered from `{{BINARY}} --help`. Use this as a reference for flag names.

```
{{DISCOVERY}}
```

## Bug-hunting strategy

Focus on inputs that historically find bugs in CLI tools:

1. **Negative values for numeric flags**: `-1` for any flag that takes a number (context lines, depth, count, width, jobs). Many tools accept these silently and produce corrupt output or wrap to huge values.

2. **Overflow boundaries**: `2147483647` (INT32_MAX) and `2147483648` (INT32_MAX+1) for numeric flags. Tests integer overflow in C code.

3. **Zero values**: `0` for numeric flags that expect positive values. Tests off-by-one and division-by-zero.

4. **Contradictory flag combinations**: Flags that logically conflict (e.g., `--verbose --quiet`, `--merge --no-merge`). Many tools silently pick one or produce undefined behavior.

5. **Empty string values**: `""` for flags that take string arguments. Tests null/empty handling.

6. **Flag-like filenames**: Files named `-rf` or `--help`. Tests argument parsing robustness.

7. **Structural edge cases**: Broken symlinks, empty files, unreadable files, very large files, files with no trailing newline.

8. **Repeated flags**: The same flag multiple times (e.g., `-v -v -v`). Some tools accumulate, some override, some crash.

## Your task

Write a single `.probe` file designed to find bugs in `{{BINARY}}`. Prioritize boundary values and flag interactions over comprehensive coverage.

**Size constraint**: Aim for 80-150 lines. Focus on the highest-risk inputs.

## Syntax rules

- `run` arguments are passed directly to the binary. Each quoted string is one argument: `run "-k" "2,2"` not `run "-k 2,2"`.
- Only `stdin` and `in` are valid modifiers after a `run` line. Do NOT put `file`, `props`, or other setup commands after `run`.
- Do NOT use shell syntax (pipes, redirects like `>`, `|`, `&&`) in run arguments.
- `from` is a TOP-LEVEL block, not a run modifier. Do NOT nest `from` inside `run`. Structure like this:

```
# CORRECT
run "data.txt"

from "data.txt"
  run "-r" "data.txt"
  run "-n" "data.txt"
```

Output only the probe file contents. No explanation or commentary.
