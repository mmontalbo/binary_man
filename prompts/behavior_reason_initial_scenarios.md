Generate initial behavior scenarios for ALL options below in a single response.

Use the descriptions to determine what each option does and create scenarios that demonstrate different behavior from the baseline:

1. **Text filters** (tr, cut, sort, uniq, etc.): Use `stdin` with content that the option will transform.

2. **File operations** (touch, mkdir, rm, etc.): Use `seed` to create necessary fixtures and `assertions` (file_exists, dir_exists, file_missing, dir_missing) to verify results.

3. **Formatting options** (--verbose, --long, etc.): These often work with a basic scenario, possibly with seed files to show formatted output.

4. **Value options** (--width=N, --delimiter=X, etc.): Provide appropriate values based on the description/placeholder.

5. **Blocking options** (--follow, -f that wait for input): Add exclusion with `blocks_indefinitely`.

6. **Help/version options**: Add exclusion with `assertion_gap` (output varies by installation).

Each option needs EITHER a scenario OR an exclusion. Respond with ALL options covered.
