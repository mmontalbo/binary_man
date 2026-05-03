# bman

Observation-driven behavioral specification for CLI binaries. Given a
binary, `bman` runs invocations across varied input states and records
what happens — stdout, stderr, exit code, and filesystem changes.

## Usage

```
bman <binary>                      discover flags from --help
bman <binary> <probe-file>         run observation grid
bman --dry-run <binary> <file>     show resolved grid without executing
```

### Discovering a binary

`bman sort` runs `sort --help`, extracts flags, and prints a probe
skeleton to stdout. Pipe to a file, customize contexts and vary blocks,
then run:

```
bman sort > sort.probe       # discover flags, generate skeleton
# edit sort.probe — add vary blocks, organize runs
bman sort sort.probe          # run the observation grid
```

For subcommands: `bman git diff` discovers flags for `git diff`.

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

- Linux (uses tempfile for sandboxing)
- Rust toolchain

## Development

```
nix develop              # enter dev shell (or use direnv)
cargo build              # build bman
cargo test               # run unit tests
```
