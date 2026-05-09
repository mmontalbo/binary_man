# bgrid

Observation-driven behavioral specification for CLI binaries. Given a
binary, `bgrid` runs invocations across varied input states and records
what happens — stdout, stderr, exit code, and filesystem changes. It
iteratively refines experiments until each flag's behavioral surface
is isolated or progress stalls.

## Usage

```
bgrid <binary>                        iterative exploration (discover + run + refine)
bgrid --skeleton <binary>             print probe skeleton for manual authoring
bgrid <binary> <file.probe>           run observation grid from a probe file
bgrid --trace <binary> <file.probe>   include syscall traces
bgrid --dry-run <binary> <file.probe> show resolved grid without executing
```

### Exploring a binary

`bgrid sort` discovers flags from `--help`, generates orthogonal contexts
(varying file content, directory structure, permissions, timestamps),
runs every flag across every context in parallel, analyzes behavioral
groups, refines with cross-group interactions, and converges when no
new flags are observed. Output is a report with exemplar
observations showing what each flag does — base output vs flag output,
mechanically selected from the context where the flag's behavior is
most unique.

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
3. **Analysis** groups runs by identical per-context observations.
   Runs in the same group are behaviorally equivalent. Singleton
   groups are isolated — that flag has unique behavior.
4. **Refinement** generates new experiments targeting specific
   indistinguishable flag stems: cross-group flag pairing (modifier +
   mode flag), sensitivity-graduated contexts, untested flag pickup.
   Converges when no new flags are observed.
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
./tests/coreutils.sh          # integration test (22 binaries, ~105s)
```

Reference reports with exemplar observations for each binary are
in `tests/results/`.

## Development

```
nix develop              # enter dev shell (or use direnv)
cargo build              # build bgrid
```
