# bgrid

Observation-driven behavioral specification for CLI binaries. Given a
binary, `bgrid` runs invocations across varied input states and records
what happens — stdout, stderr, exit code, and filesystem changes. It
tests every flag individually and all pairwise combinations in a single
phase, then reports which flags have unique observable behavior.

## Usage

```
bgrid <binary>                        explore: discover flags + run grid + report
bgrid <binary> <file.probe>           run observation grid from a probe file
bgrid --dry-run <binary> <file.probe> show resolved grid without executing
```

### Exploring a binary

`bgrid sort` discovers flags from `--help`, probes working argument
values (via help text mining, error mining, and compound probing),
generates orthogonal contexts (varying file content, structure,
permissions, timestamps, stdin), runs every flag individually and all
pairwise flag combinations across every context in parallel, then
analyzes behavioral groups. Output is a report with exemplar
observations showing what each flag does.

For subcommands: `bgrid git diff` explores `git diff`.

### Manual probe authoring

Write a `.probe` file to add custom contexts and runs that exercise
flags the mechanical discovery can't reach:

```
bgrid sort sort.probe
```

See [LANGUAGE.md](LANGUAGE.md) for the probe language specification.

## How it works

1. **Factor identification** — parse `--help` for flags, metavars,
   aliases, and value enumerations. Probe invocation patterns
   (positional args, stdin, Usage-line structural patterns like
   `COMMAND` or `[expression]`).
2. **Level determination** — pilot study probes each flag with
   candidate values: help-mined values → metavar candidates → error
   mining → companion probing → mutual compound probing. Each phase
   runs all probes in a single batched bwrap invocation.
3. **Design construction** — cross all flags × invocation patterns ×
   contexts into a fixed grid. No adaptation after this point.
4. **Execution** — batched bwrap sandboxing, one invocation per
   context, up to 32 threads. Each cell has a 2-second timeout.
5. **Analysis** — hash-anchored structural diff (O(n) for shared
   lines, NW only on gap segments), hash-based behavioral grouping,
   pairwise interaction evidence, leave-one-out robustness scoring.
6. **Report** — flags classified as solo-distinguished,
   combo-distinguished, error-differentiated, or behavioral aliases.
   Each flag gets an exemplar observation. Robustness tiers reported.

See [DESIGN.md](DESIGN.md) for the full experiment design.

## Requirements

- Linux (uses bubblewrap for sandbox isolation)
- bubblewrap (`bwrap`) — install via your package manager
- Rust toolchain

## Testing

```
cargo test                          # unit tests
./tests/coreutils.sh                # integration test (22 binaries, ~2 min)
REPRO=1 ./tests/coreutils.sh        # + cross-run reproducibility check
```

The test harness checks four properties per tool:
- **observed ≥ threshold** — behavioral evidence doesn't regress
- **total flags = expected** — flag discovery surface is stable
- **fragile = 0** — no noise-dependent distinctions
- **reproducible** (opt-in) — observed count matches across runs

Reference reports are in `tests/results/`.

## Development

```
nix develop              # enter dev shell (or use direnv)
cargo build              # build bgrid
```
