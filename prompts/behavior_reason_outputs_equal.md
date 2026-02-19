Output matches baseline - no observable difference. Fix by:
- Add `seed` files the option needs
- Add `stdin` for filter commands
- Use assertions (`file_exists`, `exit_code`) instead of stdout
- Include action/subcommand in argv (see co-dependent options below)
- Exclude if truly untestable

## Co-dependent options

Some options only work with specific actions or trailing arguments:

- **Modifier options** that modify another action: include the action
  `["action", "--modifier", "arg"]` not `["--modifier"]`

- **Options requiring values**: include realistic trailing arguments
  `["--option", "key", "value"]` not `["--option"]`

- **Format/output options**: pair with an action that produces output
  `["action", "--format-option", "arg"]`

**"unknown option" doesn't mean the option is invalid** - it means the option
needs additional context. Check the option's description for clues like
"With get..." or "Requires action...". Example: if `--all` errors with
"unknown option", and docs say "With get, return all values", try
`["get", "--all", "key"]` not `["--all"]`.

Include the minimal args needed to exercise the option. Don't exclude options
just because bare usage fails - they may work with proper action/args.
