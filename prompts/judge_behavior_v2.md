# Behavior Judgment

Judge whether a scenario meaningfully demonstrates the documented option behavior.

## Option

**ID**: {option_id}
**Description**: {description}

## Executed Scenario

**Command**: `{command_line}`
**Exit code**: {exit_code}

### Seed Setup
```
{seed_setup}
```

### Stdout
```
{variant_stdout}
```

{stderr_section}

## Task

Does this output demonstrate the behavior described above?

**Criteria:**
- The specific behavior must be observable in output (not just "different from baseline")
- Empty output usually means the scenario didn't create the right conditions
- Error output may indicate missing prerequisites

## Response Format

Respond with JSON only:

```json
{
  "verified": true,
  "reason": "Brief explanation"
}
```

OR if the scenario failed to demonstrate behavior:

```json
{
  "verified": false,
  "reason": "Brief explanation of what's wrong",
  "improved_scenarios": [
    {
      "argv": ["--option", "arg1"],
      "seed": {
        "setup": [
          ["sh", "-c", "command sequence that creates proper test state"]
        ]
      }
    }
  ]
}
```

**improved_scenarios rules:**
- `argv` is the option args only (binary name added automatically)
- `seed.setup` should be ONE `["sh", "-c", "..."]` command with the full setup sequence
- Create state that will produce observable output when the command runs
- For diff commands: create files, commit, then modify (so there's something to diff)
- For status commands: create the state the option reports on

Respond ONLY with JSON.
