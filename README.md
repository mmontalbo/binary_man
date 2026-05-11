# bgrid

Observation-driven behavioral specification for CLI binaries. Given a
binary, `bgrid` runs invocations across varied input states and records
what happens — stdout, stderr, exit code, and filesystem changes. It
tests every flag individually and all pairwise combinations in a single
phase, then reports which flags have unique observable behavior.

## Usage

```
bgrid <binary>                        explore: discover flags + run grid + report
bgrid --skeleton <binary>             print probe skeleton for manual authoring
bgrid <binary> <file.probe>           run observation grid from a probe file
bgrid --trace <binary> <file.probe>   include syscall traces
bgrid --dry-run <binary> <file.probe> show resolved grid without executing
```

### Exploring a binary

`bgrid sort` discovers flags from `--help`, generates orthogonal contexts
(varying file content, directory structure, permissions, timestamps),
runs every flag individually and all pairwise flag combinations across
every context in parallel, then analyzes behavioral groups. Output is a
report with exemplar observations showing what each flag does — base
output vs flag output, mechanically selected from the context where the
flag's behavior is most unique.

For subcommands: `bgrid git diff` explores `git diff`.

### Manual probe authoring

`bgrid --skeleton sort` prints a probe file to stdout. Edit it to add
custom contexts, vary blocks, and run combinations, then execute:

```
bgrid --skeleton sort > sort.probe
# edit sort.probe
bgrid sort sort.probe
```

See [LANGUAGE.md](LANGUAGE.md) for the probe language specification.

## How it works

1. **Contexts** declare input states — files, directories, symlinks,
   environment variables, and setup commands.
2. **Runs** declare invocations to observe. Each run executes in every
   applicable context inside a bwrap sandbox.
3. **Analysis** compares observations using structural tree diff:
   stdout/stderr are tokenized and aligned via two-level
   Needleman-Wunsch, producing edit scripts that describe the
   transformation each flag applies (e.g., "insert 8 tokens per
   line", "reverse line order"). Runs with identical edit scripts
   across all contexts are behaviorally equivalent. Singleton groups
   are isolated — that flag has unique behavior.
4. **Pairwise testing** runs all flag combinations to detect
   interaction effects — two flags that look identical alone may
   behave differently when combined with a third.
5. **Report** shows observed behavior rate: flags where the tool saw
   the flag work (exit 0, non-trivial output or filesystem changes).
   Each flag gets an exemplar showing base vs flag output in the context
   that best demonstrates its unique behavior.

## Requirements

- Linux (uses bubblewrap for sandbox isolation)
- bubblewrap (`bwrap`) — install via your package manager
- Rust toolchain

## Testing

```
cargo test                    # unit tests
./tests/coreutils.sh          # integration test (22 binaries, ~64s)
```

Reference reports with exemplar observations for each binary are
in `tests/results/`.

## Development

```
nix develop              # enter dev shell (or use direnv)
cargo build              # build bgrid
```
