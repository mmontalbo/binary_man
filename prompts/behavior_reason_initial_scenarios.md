Generate scenarios for ALL options. Each needs a scenario OR exclusion.

Tips by option type:
- **Filters** (tr, cut, sort): use `stdin`
- **File ops** (touch, rm, mkdir): use `seed` + assertions
- **Check/test** (--check, grep): use `exit_code` assertion
- **Blocking** (--follow): exclude with `blocks_indefinitely`
- **Interactive** (--edit): exclude with `requires_interactive_tty`
- **Subcommands**: include in argv: `["get", "--all", "key"]`
- **Repo tools** (git, cargo, npm): use `seed.setup` to init: `[["git", "init"]]`
