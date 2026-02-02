-- Extract options from scenario stdout (help output).
with
  scenarios as (
    select
      regexp_extract(replace(filename, '\\', '/'), '(inventory/scenarios/.*)', 1) as scenario_path,
      stdout
    from read_json_auto('inventory/scenarios/*.json', filename=true)
    where coalesce(stdout, '') <> ''
      and scenario_id like 'help--%'
  ),
  lines as (
    select
      scenario_path,
      idx as line_no,
      list_extract(str_split(stdout, chr(10)), idx) as line
    from scenarios,
      range(1, list_count(str_split(stdout, chr(10))) + 1) as t(idx)
  ),
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
  descriptions as (
    select
      scenario_path,
      option_id,
      description,
      row_number() over (
        partition by scenario_path, option_id
        order by length(description) desc nulls last
      ) as rk
    from parsed_filtered
  )
select
  'option' as kind,
  dedup_forms.option_id as id,
  dedup_forms.option_id as display,
  descriptions.description as description,
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
left join descriptions
  on dedup_forms.scenario_path = descriptions.scenario_path
  and dedup_forms.option_id = descriptions.option_id
  and descriptions.rk = 1;
