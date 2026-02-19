Output matches baseline - no observable difference. Fix by:
- Add `seed` files the option needs
- Add `stdin` for filter commands
- Use assertions (`file_exists`, `exit_code`) instead of stdout
- Include subcommand in argv
- Exclude if truly untestable
