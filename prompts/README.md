# LM Prompt Templates

Markdown files in this directory are embedded at compile time via `include_str!` and sent to language models during enrichment workflows.

## Files

| File | Purpose |
|------|---------|
| `behavior_base.md` | Main template with actions and response format |
| `behavior_reason_*.md` | Reason-specific guidance inserted into base |

## Behavior Prompt Assembly

The behavior prompt is assembled from:
1. `behavior_base.md` - header, actions, format
2. `behavior_reason_{reason_code}.md` - inserted at `{reason_section}`
3. Context data - inserted at `{context_section}` and `{targets}`

## Reason Codes

| Code | File | When Used |
|------|------|-----------|
| `initial_scenarios` | behavior_reason_initial_scenarios.md | First pass, need scenarios for all |
| `no_scenario` | behavior_reason_no_scenario.md | Option has no test scenario |
| `outputs_equal` | behavior_reason_outputs_equal.md | Scenario output matches baseline |
| `outputs_equal_retry` | behavior_reason_outputs_equal_retry.md | Still equal after retries |
| `assertion_failed` | behavior_reason_assertion_failed.md | Assertions didn't pass |

## Template Syntax

Templates use `{placeholder}` syntax for runtime substitution via `.replace()`.
