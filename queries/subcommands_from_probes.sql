-- Extract subcommands from probe stdout (git-style indented lists).
with
  probes as (
    select
      filename as probe_path,
      stdout
    from read_json_auto('inventory/probes/*.json', filename=true)
    where coalesce(stdout, '') <> ''
  ),
  lines as (
    select
      probe_path,
      line
    from probes,
      unnest(str_split(stdout, chr(10))) as t(line)
  ),
  usage_hint as (
    select probe_path
    from lines
    group by probe_path
    having
      bool_or(regexp_matches(line, '(?i)^\s*usage:'))
      and bool_or(regexp_matches(line, '(?i)<command>'))
  ),
  candidates as (
    select
      lines.probe_path,
      regexp_extract(line, '^\s*([a-z][a-z0-9-]*)\s{2,}(.+)$', 1) as subcommand,
      regexp_extract(line, '^\s*([a-z][a-z0-9-]*)\s{2,}(.+)$', 2) as description
    from lines
    join usage_hint using (probe_path)
    where regexp_matches(line, '^\s+[a-z][a-z0-9-]*\s{2,}\S')
  ),
  dedup as (
    select
      probe_path,
      subcommand,
      description,
      row_number() over (
        partition by probe_path, subcommand
        order by length(description) desc nulls last
      ) as rk
    from candidates
    where subcommand is not null and subcommand <> ''
  )
select
  subcommand,
  description,
  probe_path,
  case when usage_hint.probe_path is not null then true else false end as multi_command_hint
from dedup
left join usage_hint using (probe_path)
where rk = 1
union all
select
  null as subcommand,
  null as description,
  probe_path,
  true as multi_command_hint
from usage_hint;
