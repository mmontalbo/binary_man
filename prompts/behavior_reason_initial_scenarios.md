Generate scenarios for ALL options. Each needs a scenario OR exclusion.

Tips by option type:
- **Filters** (tr, cut, sort): use `stdin`
- **File ops** (touch, rm, mkdir): use `seed` + assertions
- **Check/test** (--check, grep): use `exit_code` assertion
- **Blocking** (--follow): exclude with `blocks_indefinitely`
- **Interactive** (--edit): exclude with `requires_interactive_tty`
- **Repo tools** (git, cargo, npm): use `seed.setup` to init: `[["git", "init"]]`

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
