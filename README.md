# binary_man

Static-first man page generator that consumes `binary_lens` context packs and
deterministically renders a comprehensive, plausible man page from usage
evidence. Dynamic validation is deferred.

Note: the current help-text extraction is tuned for `ls` as the guinea pig CLI.

## Usage

Generate a fresh `binary_lens` pack for `ls` and render a man page:

```
bman ls
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
- `--pack <dir>`: use an existing `binary.lens` pack (root or parent dir)
- `--refresh-pack`: regenerate the pack at the default location
- `--out-dir <dir>`: output root for packs and man pages

## Outputs

Default layout under `out/`:

- `out/packs/<binary>/binary.lens/` (fresh pack when generated)
- `out/man/<binary>/ls.1` (man page)
- `out/man/<binary>/help.txt` (extracted help/usage text)
- `out/man/<binary>/usage_evidence.json` (usage/help evidence rows)
- `out/man/<binary>/usage_lens.template.sql` (lens template)
- `out/man/<binary>/usage_lens.sql` (rendered lens SQL)
- `out/man/<binary>/meta.json` (provenance metadata)

## binary_lens integration

`bman` generates packs via:

```
nix run ../binary_lens#binary_lens -- <binary> -o out/packs/<binary>
```

## DuckDB extraction (lens-based)

Help/usage text is extracted exclusively via a local lens (`queries/ls_usage_evidence.sql`)
that ties string arguments to `usage`/`_usage_ls` callsites in the pack. DuckDB
is invoked via `nix run nixpkgs#duckdb --`.

## Rendering

`bman` renders `ls.1` directly from the usage lens output and the extracted
help text. No external LM is invoked.
