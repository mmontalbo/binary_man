-- Extract options from scenario stdout (help output).
with
  scenarios as (
    select
      filename as scenario_path,
      stdout
    from read_json_auto('inventory/scenarios/*.json', filename=true)
    where coalesce(stdout, '') <> ''
  ),
  lines as (
    select
      scenario_path,
      line
    from scenarios,
      unnest(str_split(stdout, chr(10))) as t(line)
  ),
  long_opts as (
    select
      scenario_path,
      regexp_extract(line, '(^|\s)(--[a-zA-Z0-9][a-zA-Z0-9-]*)(?:[=,\s]|$)', 2) as option,
      regexp_extract(line, '^\s*[^-]*-{1,2}[^\s]+\s{2,}(.+)$', 1) as description
    from lines
    where regexp_matches(line, '^\s+-')
      and regexp_matches(line, '\s--[a-zA-Z0-9]')
  ),
  short_opts as (
    select
      scenario_path,
      regexp_extract(line, '(^|\s)(-[a-zA-Z0-9])(?:[=,\s]|$)', 2) as option,
      regexp_extract(line, '^\s*[^-]*-{1,2}[^\s]+\s{2,}(.+)$', 1) as description
    from lines
    where regexp_matches(line, '^\s+-')
      and regexp_matches(line, '\s-[a-zA-Z0-9]')
  ),
  candidates as (
    select * from long_opts
    union all
    select * from short_opts
  ),
  dedup as (
    select
      scenario_path,
      option,
      description,
      row_number() over (
        partition by scenario_path, option
        order by length(description) desc nulls last
      ) as rk
    from candidates
    where option is not null and option <> ''
  )
select
  'option' as kind,
  option as id,
  option as display,
  description,
  scenario_path,
  false as multi_command_hint
from dedup
where rk = 1;
