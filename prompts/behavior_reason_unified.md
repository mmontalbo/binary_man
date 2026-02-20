## Current State: {reason_code}

{state_context}

## Guidance

**Co-dependent options**: Some options only work with specific actions or trailing arguments.

- **Modifier options**: include the action `["action", "--modifier", "arg"]` not `["--modifier"]`
- **Options requiring values**: include trailing arguments `["--option", "key", "value"]`
- **Format/output options**: pair with an action that produces output

**"unknown option" doesn't mean invalid** - it means the option needs additional context.
Check the description for clues like "With get..." or "Requires action...".
Don't exclude options just because bare usage fails - they may work with proper action/args.

**Common patterns by option type:**
- Filters (tr, cut, sort): use `stdin`
- File creation (touch, mkdir, cp): use `seed` + `file_exists`/`dir_exists` assertion
- File removal (rm, mv source): use `seed` + `file_removed` assertion
- Check/test (--check, grep): use `exit_code` assertion
- Blocking (--follow): exclude with `blocks_indefinitely`
- Interactive (--edit): exclude with `requires_interactive_tty`
- Repo tools (git, cargo, npm): use `seed.setup` to init: `[["git", "init"]]`

{hints_section}
