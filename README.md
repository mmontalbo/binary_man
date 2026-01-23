# binary_man

Static-first man page generator that consumes `binary_lens` context packs and
deterministically renders a comprehensive, plausible man page from usage
evidence. Optional scenario runs can be used to populate a real `.SH EXAMPLES`
section from captured stdout/stderr, proving documented invocations behave as
described.

Note: the current help-text extraction is tuned for `ls` as the guinea pig CLI.

## Usage

Generate a fresh `binary_lens` pack for `ls` and render a man page:

```
bman ls
```

Run the scenario catalog and render scenario-backed examples:

```
bman ls --run-scenarios
```

Generate a portable doc pack and run scenarios:

```
bman ls --doc-pack /tmp/ls-docpack --run-scenarios
```

Use an existing pack:

```
bman ls --pack out/packs/ls/binary.lens
```

Custom output directory:

```
bman ls --out-dir ./out
```

Flags:
- `--doc-pack <dir>`: co-locate pack, scenarios, fixtures, and outputs under `<dir>/`
- `--pack <dir>`: use an existing `binary.lens` pack (root or parent dir)
- `--refresh-pack`: regenerate the pack at the default location
- `--out-dir <dir>`: output root for packs and man pages (ignored with `--doc-pack`)
- `--run-scenarios`: run the scenarios catalog via `binary_lens run=1` and emit `examples_report.json`
- `--scenarios <file>`: override the scenarios catalog path
- `--lens-flake <ref>`: override the `binary_lens` flake ref (default: `../binary_lens#binary_lens`)

## Outputs

Default layout under `out/`:

- `out/packs/<binary>/binary.lens/` (fresh pack when generated)
- `out/man/<binary>/ls.1` (man page)
- `out/man/<binary>/help.txt` (extracted help/usage text)
- `out/man/<binary>/usage_evidence.json` (usage/help evidence rows)
- `out/man/<binary>/usage_lens.template.sql` (lens template)
- `out/man/<binary>/usage_lens.sql` (rendered lens SQL)
- `out/man/<binary>/examples_report.json` (scenario validation + run refs; only when `--run-scenarios`)
- `out/man/<binary>/coverage_ledger.json` (coverage ledger; only when scenarios catalog exists)
- `out/man/<binary>/meta.json` (provenance metadata)

Doc pack layout under `<dir>/` (when `--doc-pack` is set):

- `<dir>/binary.lens/` (pack)
- `<dir>/scenarios/<binary>.json` (scenario catalog)
- `<dir>/fixtures/...` (fixture trees)
- `<dir>/queries/<binary>_usage_evidence.sql` (usage lens template)
- `<dir>/man/<binary>.1` (man page)
- `<dir>/man/help.txt` (extracted help/usage text)
- `<dir>/man/usage_evidence.json` (usage/help evidence rows)
- `<dir>/man/usage_lens.template.sql` (lens template)
- `<dir>/man/usage_lens.sql` (rendered lens SQL)
- `<dir>/man/examples_report.json` (scenario validation + run refs; only when `--run-scenarios`)
- `<dir>/coverage_ledger.json` (coverage ledger)
- `<dir>/man/meta.json` (provenance metadata)

## binary_lens integration

`bman` generates packs via:

```
nix run <lens-flake> -- <binary> -o out/packs/<binary>
```

When `--run-scenarios` is enabled, `bman` appends runtime runs to the existing
pack via:

```
nix run <lens-flake> -- run=1 out/packs/<binary>/binary.lens --help
```

## DuckDB extraction (lens-based)

Help/usage text is extracted exclusively via a local lens (`queries/<binary>_usage_evidence.sql`)
that ties string arguments to `usage`/`_usage_ls` callsites in the pack. When
`--doc-pack` is set, the template is copied under `<doc-pack>/queries/` and used
from there. DuckDB is invoked via `nix run nixpkgs#duckdb --`.

## Rendering

`bman` renders `ls.1` directly from the usage lens output and the extracted
help text. No external LM is invoked.
