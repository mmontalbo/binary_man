# Binary Help Context Extractor

Capture raw `--help` output from a binary under a controlled environment and write it to an
artifact file for LM context.

## Usage

```
cargo run -- /path/to/bin --out-dir ./out

# Example (Nix shells typically expose ls via `which`):
cargo run -- "$(which ls)" --out-dir ./out
```

## Output

The extracted help text is written to:

```
out/context/<binary-name>/help.txt
```

## Environment Contract

Help capture runs with:

- `LC_ALL=C`
- `TZ=UTC`
- `TERM=dumb`

If `--help` produces no output, the extractor falls back to `-h`.
