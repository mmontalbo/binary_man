These options produce identical output to the baseline. Analyze the option description to determine why:

1. **Option needs specific fixture files**: Add a `seed` with files/directories that demonstrate the behavioral difference.

2. **Option needs a specific value**: Add value_examples based on the option description.

3. **Option needs other flags**: Add requires_argv if the option only works with other options.

4. **Option effect not visible in text output**: Add exclusion with appropriate reason_code.

Prefer updating the scenario with seed fixtures before excluding.
