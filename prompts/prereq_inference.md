# Prereq Inference for `{binary_name}`

Analyze each option's documentation and determine what prerequisites it needs for auto-verification.

## Categories

- **filesystem**: needs specific directory/file structure (provide seed)
- **config**: needs config files present (provide seed)
- **state**: needs existing state like commits, staged files (provide seed)
- **interactive**: requires TTY/editor (exclude from auto-verify)
- **network**: requires network access (exclude from auto-verify)
- **privilege**: requires elevated permissions (exclude from auto-verify)
- **null**: no special requirements

{existing_definitions}

## Surface Items to Analyze

{surface_items}

## Output Format

Return JSON with:
1. `definitions`: New prereq definitions (only if no existing one fits)
2. `surface_map`: Mapping from option id to prereq keys (or empty array)

```json
{
  "definitions": {
    "project_root": {
      "description": "project directory structure",
      "seed": {"dirs": ["src"]},
      "exclude": false
    },
    "interactive_mode": {
      "description": "requires interactive TTY",
      "seed": null,
      "exclude": true
    }
  },
  "surface_map": {
    "--edit": ["interactive_mode"],
    "--path": ["project_root"],
    "--help": []
  }
}
```

Seed fields: `dirs` (array), `files` (object), `symlinks` (object), `executables` (object)

Rules:
- Reference existing definitions when applicable
- Define new prereqs only when no existing one fits
- Use `exclude: true` for interactive, network, privilege categories
- Empty array `[]` means no prereqs needed

Respond ONLY with JSON.
