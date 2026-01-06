# M6 Scenario Runner

Run exactly one binary inside a sandbox and emit an evidence bundle. M6 does not
infer semantics, mutate inputs, or retry. The binary behavior is the oracle.

## Usage

```
cargo run -- scenario.json
```

Optional flags:

```
cargo run -- scenario.json --out-dir ./out
cargo run -- scenario.json --direct
```

`--direct` skips bwrap and is intended for debugging only.

## Scenario JSON

Example (`scenario.json`):

```json
{
  "scenario_id": "ls_help_smoke",
  "binary": { "path": "/nix/store/.../bin/ls" },
  "args": ["--help"],
  "fixture": { "id": "fs/empty_dir" },
  "limits": {
    "wall_time_ms": 200,
    "cpu_time_ms": 100,
    "memory_kb": 65536,
    "file_size_kb": 1024
  },
  "artifacts": {
    "capture_stdout": true,
    "capture_stderr": true,
    "capture_exit_code": true
  }
}
```

Notes:
- `args` is an array of strings only (no shell parsing).
- `binary.path` must be an executable file (symlinks are resolved before hashing).
- `fixture.id` maps to `fixtures/<id>/`.
- Limits are required and bounded in code.

## Fixtures

Fixtures live under `fixtures/`:

```
fixtures/
  fs/
    empty_dir/
      manifest.json
      tree/
```

`manifest.json` is authoritative. The runner copies `tree/` into a temp dir,
applies permissions and mtimes from the manifest, and verifies file hashes.

## Evidence bundle

Each run writes to `out/evidence/<run_id>/` (or `<out-dir>/evidence/<run_id>/`):

```
out/evidence/<run_id>/
  scenario.json
  meta.json
  stdout.txt   (when captured)
  stderr.txt   (when captured)
```

`meta.json` includes hashes for the binary, scenario, fixture manifest, and
stdout/stderr, plus exit code and timing.

## Environment contract

All runs set:
- `LC_ALL=C`
- `TZ=UTC`
- `TERM=dumb`

stdin is always `/dev/null`. Network is disabled inside the sandbox.
