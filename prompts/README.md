# LM Prompt Templates

Markdown files in this directory are embedded at compile time via `include_str!` and sent to language models during enrichment workflows.

## Files

- `enrich_agent_prompt.md` - Main agent system prompt for enrichment
- `behavior_base.md` - Base template for behavior verification prompts
- `behavior_reason_*.md` - Reason-specific sections concatenated with base
- `prereq_inference.md` - Prompt for inferring prerequisites from documentation

## Usage

Templates use `{placeholder}` syntax for runtime substitution via `.replace()`.
