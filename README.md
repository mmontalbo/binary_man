# bgrid

Observation-driven behavioral specification for CLI binaries. Given a
binary, `bgrid` runs invocations across varied input states and records
what happens — stdout, stderr, exit code, and filesystem changes.

## Usage

```
bgrid <binary>                        discover flags from --help
bgrid <binary> <probe-file>           run observation grid
bgrid --compact <binary> <file.probe> collapsed summary output
bgrid --trace <binary> <file.probe>   include file access traces
bgrid --dry-run <binary> <file>       show resolved grid without executing
```

### Discovering a binary

`bgrid sort` runs `sort --help`, extracts flags, and prints a probe
skeleton to stdout. Pipe to a file, customize contexts and vary blocks,
then run:

```
bgrid sort > sort.probe       # discover flags, generate skeleton
# edit sort.probe — add vary blocks, organize runs
bgrid sort sort.probe          # run the observation grid
```

For subcommands: `bgrid git diff` discovers flags for `git diff`.

### Writing probe files

See [LANGUAGE.md](LANGUAGE.md) for the full language specification.
Probe files describe input states and invocations. The tool executes
every combination and writes observations to a `.results` file.

## How it works

1. **Contexts** declare input states — files, directories, symlinks,
   environment variables, and setup commands (`invoke`).
2. **Runs** declare invocations to observe. Each run executes in every
   applicable context.
3. **Collapsing** groups contexts that produce identical observations.
   The contexts that DON'T collapse reveal sensitivity to specific
   input perturbations.
4. **Results** are written to a `.results` file with observations,
   sensitivity analysis, universals, and diffs.

## Requirements

- Linux (uses bubblewrap for sandbox isolation)
- bubblewrap (`bwrap`) — install via your package manager
- Rust toolchain

## Development

```
nix develop              # enter dev shell (or use direnv)
cargo build              # build bgrid
cargo test               # run unit tests
```
