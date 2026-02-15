These options still produce identical output after {retry_count} retry attempts.

Analyze the option description to determine what fixtures are needed. Common fixes:

1. **Update scenario with seed fixtures** - create files/dirs/symlinks that demonstrate the behavior
2. **Add exclusion** if the option cannot be tested (requires interactive TTY, system changes, etc.)

Use seed fixtures when the option's behavior depends on specific file types or contents.
