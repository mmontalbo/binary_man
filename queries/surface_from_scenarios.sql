-- Extract surface items from scenario stdout (help output).
-- Handles two formats:
-- 1. Dash-prefixed options (e.g., "-a, --all    description")
-- 2. Indented command lists (e.g., "   clone    Clone a repository")
with
  scenario_context as (
    select
      regexp_extract(replace(filename, '\\', '/'), '(inventory/scenarios/.*)', 1) as scenario_path,
      argv,
      stdout,
      -- Entry point chain: argv minus binary (first) and help flag (last)
      -- ["git", "config", "--help"] → ["config"]
      -- ["git", "--help"] → []
      case
        when len(argv) > 2 then list_slice(argv, 2, len(argv) - 1)
        else []::VARCHAR[]
      end as entry_point_chain
    from read_json_auto('inventory/scenarios/*.json', filename=true)
    where coalesce(stdout, '') <> ''
      and (scenario_id like 'help--%' or scenario_id like 'help::%')
  ),
  scenarios as (
    select
      scenario_path,
      stdout,
      entry_point_chain,
      case
        when len(entry_point_chain) > 0 then entry_point_chain[-1]
        else null
      end as parent_id
    from scenario_context
  ),
  lines as (
    select
      scenario_path,
      idx as line_no,
      -- Strip ANSI escape codes (e.g., \e[1m for bold) before processing
      regexp_replace(
        list_extract(str_split(stdout, chr(10)), idx),
        '\x1b\[[0-9;]*[a-zA-Z]',
        '',
        'g'
      ) as line
    from scenarios,
      range(1, list_count(str_split(stdout, chr(10))) + 1) as t(idx)
  ),

  -- ==========================================================================
  -- Entry point extraction (indented command lists like git subcommands)
  -- ==========================================================================
  usage_hint as (
    select scenario_path
    from lines
    group by scenario_path
    having
      bool_or(regexp_matches(line, '(?i)^\s*usage:'))
      and bool_or(regexp_matches(line, '(?i)<command>'))
  ),
  entry_point_candidates as (
    select
      lines.scenario_path,
      regexp_extract(line, '^\s*([a-z][a-z0-9-]*)\s{2,}(.+)$', 1) as item_id,
      regexp_extract(line, '^\s*([a-z][a-z0-9-]*)\s{2,}(.+)$', 2) as description
    from lines
    join usage_hint using (scenario_path)
    where regexp_matches(line, '^\s+[a-z][a-z0-9-]*\s{2,}\S')
  ),
  entry_point_dedup as (
    select
      scenario_path,
      item_id,
      description,
      row_number() over (
        partition by scenario_path, item_id
        order by length(description) desc nulls last
      ) as rk
    from entry_point_candidates
    where item_id is not null and item_id <> ''
  ),
  entry_point_items as (
    select
      item_id as id,
      item_id as display,
      description,
      scenarios.parent_id as parent_id,
      -- Entry points include their own id in context_argv
      to_json(list_append(scenarios.entry_point_chain, item_id)) as context_argv,
      to_json([]::VARCHAR[]) as forms,
      to_json(struct_pack(
        value_arity := 'unknown',
        value_separator := 'unknown',
        value_placeholder := null,
        value_examples := []::VARCHAR[],
        requires_argv := []::VARCHAR[]
      )) as invocation,
      entry_point_dedup.scenario_path,
      true as multi_command_hint
    from entry_point_dedup
    left join scenarios on entry_point_dedup.scenario_path = scenarios.scenario_path
    where rk = 1
  ),
  -- Emit multi_command_hint marker for scenarios with usage hint
  multi_command_markers as (
    select
      '' as id,
      '' as display,
      null as description,
      null as parent_id,
      to_json([]::VARCHAR[]) as context_argv,
      to_json([]::VARCHAR[]) as forms,
      to_json(struct_pack(
        value_arity := 'unknown',
        value_separator := 'unknown',
        value_placeholder := null,
        value_examples := []::VARCHAR[],
        requires_argv := []::VARCHAR[]
      )) as invocation,
      scenario_path,
      true as multi_command_hint
    from usage_hint
  ),

  -- ==========================================================================
  -- Option extraction (dash-prefixed lines like "-a, --all")
  -- ==========================================================================
  classified as (
    select
      scenario_path,
      line_no,
      line,
      regexp_matches(line, '^\s+-') as is_header,
      regexp_matches(line, '^\s*$') as is_blank,
      regexp_matches(line, '^\s+') as is_indented
    from lines
  ),
  sequenced as (
    select
      scenario_path,
      line_no,
      line,
      is_header,
      is_blank,
      is_indented,
      sum(case when is_header then 1 else 0 end) over (
        partition by scenario_path
        order by line_no
      ) as header_idx,
      max(case when is_header then line_no end) over (
        partition by scenario_path
        order by line_no
        rows between unbounded preceding and current row
      ) as last_header_line_no,
      max(case when not is_header and (is_blank or not is_indented) then line_no end) over (
        partition by scenario_path
        order by line_no
        rows between unbounded preceding and current row
      ) as last_break_line_no
    from classified
  ),
  option_headers as (
    select
      scenario_path,
      header_idx,
      line,
      regexp_extract(line, '^\s*(\S.*?)(?:\s{2,}|\t+|$)', 1) as forms_chunk,
      nullif(
        trim(regexp_extract(line, '^\s*\S.*?(?:\s{2,}|\t+)(.+)$', 1)),
        ''
      ) as inline_description
    from sequenced
    where is_header
  ),
  continuation_lines as (
    select
      scenario_path,
      header_idx,
      line_no,
      trim(line) as continuation_line
    from sequenced
    where not is_header
      and not is_blank
      and is_indented
      and last_header_line_no is not null
      and (last_break_line_no is null or last_header_line_no > last_break_line_no)
  ),
  continuations as (
    select
      scenario_path,
      header_idx,
      string_agg(continuation_line, ' ' order by line_no) as continuation_description
    from continuation_lines
    group by scenario_path, header_idx
  ),
  option_lines as (
    select
      headers.scenario_path,
      headers.header_idx,
      headers.line,
      headers.forms_chunk,
      case
        when headers.inline_description is not null
          and continuations.continuation_description is not null
          then headers.inline_description || ' ' || continuations.continuation_description
        when headers.inline_description is not null
          then headers.inline_description
        else continuations.continuation_description
      end as description
    from option_headers as headers
    left join continuations
      on headers.scenario_path = continuations.scenario_path
      and headers.header_idx = continuations.header_idx
  ),
  raw_forms as (
    select
      scenario_path,
      header_idx,
      description,
      trim(form) as raw_form
    from option_lines,
      unnest(str_split(forms_chunk, ',')) as t(form)
    where forms_chunk is not null and forms_chunk <> ''
  ),
  parsed_tokens as (
    select
      scenario_path,
      header_idx,
      description,
      raw_form,
      case
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*\[=.+\]$')
          then regexp_extract(raw_form, '^(--[a-zA-Z0-9][a-zA-Z0-9-]*)', 1)
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*=\S+$')
          then regexp_extract(raw_form, '^(--[a-zA-Z0-9][a-zA-Z0-9-]*)', 1)
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*\s+\S+$')
          then regexp_extract(raw_form, '^(--[a-zA-Z0-9][a-zA-Z0-9-]*)', 1)
        when regexp_matches(raw_form, '^-[a-zA-Z0-9]\s+\S+$')
          then regexp_extract(raw_form, '^(-[a-zA-Z0-9])', 1)
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*$')
          then raw_form
        when regexp_matches(raw_form, '^-[a-zA-Z0-9]$')
          then raw_form
        else null
      end as option_id,
      case
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*\[=.+\]$') then 'optional'
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*=\S+$') then 'required'
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*\s+\S+$') then 'required'
        when regexp_matches(raw_form, '^-[a-zA-Z0-9]\s+\S+$') then 'required'
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*$') then 'none'
        when regexp_matches(raw_form, '^-[a-zA-Z0-9]$') then 'none'
        else 'unknown'
      end as value_arity,
      case
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*\[=.+\]$') then 'equals'
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*=\S+$') then 'equals'
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*\s+\S+$') then 'space'
        when regexp_matches(raw_form, '^-[a-zA-Z0-9]\s+\S+$') then 'space'
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*$') then 'none'
        when regexp_matches(raw_form, '^-[a-zA-Z0-9]$') then 'none'
        else 'unknown'
      end as value_separator,
      case
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*\[=.+\]$')
          then regexp_extract(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*\[=([^\]]+)\]$', 1)
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*=\S+$')
          then regexp_extract(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*=([^\s]+)$', 1)
        when regexp_matches(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*\s+\S+$')
          then regexp_extract(raw_form, '^--[a-zA-Z0-9][a-zA-Z0-9-]*\s+([^\s]+)$', 1)
        when regexp_matches(raw_form, '^-[a-zA-Z0-9]\s+\S+$')
          then regexp_extract(raw_form, '^-[a-zA-Z0-9]\s+([^\s]+)$', 1)
        else null
      end as value_token
    from raw_forms
    where raw_form is not null and raw_form <> ''
  ),
  parsed as (
    select
      scenario_path,
      header_idx,
      description,
      raw_form,
      option_id,
      value_arity,
      value_separator,
      case
        when value_token is not null
          and (
            (regexp_matches(value_token, '[A-Z]') and not regexp_matches(value_token, '[a-z]'))
            or regexp_matches(value_token, '^<[^>]+>$')
          )
          then value_token
        else null
      end as value_placeholder,
      case
        when value_token is not null
          and not (
            (regexp_matches(value_token, '[A-Z]') and not regexp_matches(value_token, '[a-z]'))
            or regexp_matches(value_token, '^<[^>]+>$')
          )
          then value_token
        else null
      end as value_example
    from parsed_tokens
  ),
  parsed_filtered as (
    select *
    from parsed
    where option_id is not null and option_id <> ''
  ),
  block_shapes as (
    select
      scenario_path,
      header_idx,
      case
        when max(
          case
            when value_arity = 'optional'
              and value_placeholder is not null
              and value_placeholder <> ''
              then 1
            else 0
          end
        ) = 1
          then 'optional'
        when max(
          case
            when value_arity = 'required'
              and value_placeholder is not null
              and value_placeholder <> ''
              then 1
            else 0
          end
        ) = 1
          then 'required'
        when max(case when value_arity = 'none' then 1 else 0 end) = 1 then 'none'
        else 'unknown'
      end as block_value_arity,
      max(
        case
          when value_placeholder is not null and value_placeholder <> '' then value_placeholder
          else null
        end
      ) as block_value_placeholder
    from parsed_filtered
    group by scenario_path, header_idx
  ),
  adjusted as (
    select
      scenario_path,
      description,
      raw_form,
      option_id,
      case
        when is_upgraded then block_value_arity
        else value_arity
      end as value_arity,
      case
        when is_upgraded and value_separator = 'none' then 'unknown'
        else value_separator
      end as value_separator,
      case
        when is_upgraded
          and (value_placeholder is null or value_placeholder = '')
          then block_value_placeholder
        else value_placeholder
      end as value_placeholder,
      value_example
    from (
      select
        parsed_filtered.*,
        block_shapes.block_value_arity,
        block_shapes.block_value_placeholder,
        case
          when parsed_filtered.value_arity in ('none', 'unknown')
            and block_shapes.block_value_arity in ('required', 'optional')
            then true
          else false
        end as is_upgraded
      from parsed_filtered
      left join block_shapes
        on parsed_filtered.scenario_path = block_shapes.scenario_path
        and parsed_filtered.header_idx = block_shapes.header_idx
    ) as joined_blocks
  ),
  dedup_forms as (
    select distinct
      scenario_path,
      option_id,
      raw_form,
      value_arity,
      value_separator,
      value_placeholder,
      value_example
    from adjusted
  ),
  option_descriptions as (
    select
      scenario_path,
      option_id,
      description,
      row_number() over (
        partition by scenario_path, option_id
        order by length(description) desc nulls last
      ) as rk
    from parsed_filtered
  ),
  option_items as (
    select
      dedup_forms.option_id as id,
      dedup_forms.option_id as display,
      option_descriptions.description as description,
      scenarios.parent_id as parent_id,
      to_json(scenarios.entry_point_chain) as context_argv,
      to_json([dedup_forms.raw_form]) as forms,
      to_json(struct_pack(
        value_arity := dedup_forms.value_arity,
        value_separator := dedup_forms.value_separator,
        value_placeholder := nullif(dedup_forms.value_placeholder, ''),
        value_examples := case
          when dedup_forms.value_example is not null and dedup_forms.value_example <> ''
            then [dedup_forms.value_example]
          else []::VARCHAR[]
        end,
        requires_argv := []::VARCHAR[]
      )) as invocation,
      dedup_forms.scenario_path as scenario_path,
      false as multi_command_hint
    from dedup_forms
    left join option_descriptions
      on dedup_forms.scenario_path = option_descriptions.scenario_path
      and dedup_forms.option_id = option_descriptions.option_id
      and option_descriptions.rk = 1
    left join scenarios
      on dedup_forms.scenario_path = scenarios.scenario_path
  )

-- ==========================================================================
-- Final output: combine entry points, options, and multi-command markers
-- ==========================================================================
select * from entry_point_items
union all
select * from option_items
union all
select * from multi_command_markers;
