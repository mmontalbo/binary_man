These options produce identical output to the baseline. Analyze the option description to determine why:

1. **Option needs specific fixture files**: Add a `seed` with files/directories that demonstrate the behavioral difference.

2. **Option needs stdin input**: Use `stdin` field for filter commands (tr, cut, sort, uniq).

3. **Option needs a specific value**: Add value_examples based on the option description.

4. **Option needs other flags**: Add requires_argv if the option only works with other options.

5. **Option effect not visible in text output**: Add exclusion with appropriate reason_code.

6. **Option signals via exit code**: Use `exit_code` assertion for commands that signal success/failure via return code instead of stdout. Examples:
   - `sort --check` exits 0 (sorted) or 1 (unsorted) with no stdout change
   - `test -f file` exits 0 (exists) or 1 (missing)
   - `grep pattern` exits 0 (match) or 1 (no match)

   Example scenario with exit_code assertion:
   ```json
   {"argv": ["--check"], "stdin": "a\nb\nc", "assertions": [{"kind": "exit_code", "expected": 0}]}
   ```

Prefer updating the scenario with seed fixtures or exit_code assertions before excluding.
