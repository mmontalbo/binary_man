These options have no test scenario. For each option, either:
1. Create a scenario that demonstrates the option's behavior, OR
2. Add an exclusion if the option cannot be tested (e.g., requires interactive TTY, unsafe side effects)

When creating scenarios:
- Use realistic argument values based on the option's description
- Include assertions that verify the option changes the output
- The scenario should have a `covers` array including the option id
