# Behavior Judgment

You are judging whether a command's output demonstrates the documented behavior for an option.

## Option

**ID**: {option_id}
**Description**: {description}

## Scenario

**Command**: `{command_line}`
**Exit code**: {exit_code}

### Output

```
{variant_stdout}
```

{stderr_section}

## Question

Based on the description above, this option should: **{description}**

Does the output demonstrate this behavior?

- Look for concrete evidence that the option's effect is visible in the output
- "Different from baseline" is NOT sufficient - the specific behavior must be observable
- If the option affects formatting, verify the format change is present
- If the option adds/removes information, verify that change is visible

## Response Format

Respond with JSON only:

```json
{
  "demonstrates_behavior": true/false,
  "reason": "One sentence explaining your judgment",
  "suggested_setup": ["command1", "command2"] or null
}
```

**demonstrates_behavior**: true if output shows the described behavior, false otherwise.

**reason**: Brief explanation. Examples:
- "Output shows stash information as expected"
- "No stash exists, so --show-stash has nothing to display"
- "Output is identical to baseline, no branch info visible"

**suggested_setup**: If false, suggest setup commands that would trigger the behavior.
Use null if you cannot determine what setup would help.

Respond ONLY with JSON.
