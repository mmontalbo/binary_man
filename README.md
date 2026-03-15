# bman

LM-driven CLI binary verifier. Given a binary and optional subcommand, `bman`
discovers every flag/option from `--help`, then uses a language model to
design test scenarios that exercise each option in a sandbox and verify
observable behavior.

## Usage

```
bman <binary> [entry-point...]
```

Examples:

```
bman ls                              # verify all ls options
bman git diff                        # verify git-diff options only
bman -v --max-cycles 10 git diff     # verbose, limited cycles
bman git diff --output json          # structured JSON output
```

## How it works

1. **Bootstrap** — runs `<binary> --help` (and `-h`), parses every option/flag
   into a surface inventory with descriptions and value hints.
2. **Verification loop** — asks an LM to design seed environments and test
   arguments for each surface. Runs the command with and without the option in
   a bubblewrap sandbox, comparing stdout/stderr/exit code/filesystem effects.
3. **Critique pass** — reviews verified surfaces for false positives (e.g.,
   outputs differ but not because of the option). Demoted surfaces are retried
   with feedback.
4. **Output** — summary of verified/excluded/pending surfaces, or JSON status.

All state lives in a single `state.json` inside the doc pack
(`~/.local/share/bman/packs/<binary>[-entry-point]` by default).

## Options

| Flag | Default | Description |
|------|---------|-------------|
| `--max-cycles N` | 80 | Max LM cycles before stopping |
| `--lm LM` | `claude:haiku` | LM plugin (`claude:haiku`, `claude:sonnet`) |
| `--session-size N` | 20 | Surfaces per LM session (0 = unlimited) |
| `--parallel BOOL` | true | Run sessions in parallel |
| `--context-mode MODE` | auto | `auto`, `full`, `reset`, `incremental` |
| `--with-pty` | off | Run commands in PTY for color/terminal output |
| `--doc-pack DIR` | auto | Override pack directory |
| `--output FORMAT` | man | `man`, `json`, or `path` |
| `-v, --verbose` | off | Detailed progress output |

## Requirements

- [Claude Code CLI](https://docs.anthropic.com/en/docs/claude-code) (`claude` binary)
- [bubblewrap](https://github.com/containers/bubblewrap) (`bwrap`) for sandboxing
- Linux (uses `bwrap` and `PR_SET_PDEATHSIG`)

## Development

```
nix develop              # enter dev shell (or use direnv)
cargo build              # build bman
cargo test               # run unit tests
cargo clippy             # lint (all warnings are denied)
```
