-- Prefer scenario-generated help output for usage evidence.
-- Assumes DuckDB CWD is binary.lens/ so scenario evidence is at ../inventory/scenarios.
with
  manifest as (
    select binary_name
    from read_json_auto('manifest.json')
  ),
  scenarios as (
    select
      argv,
      stdout,
      timed_out,
      generated_at_epoch_ms
    from read_json_auto('../inventory/scenarios/*.json')
  ),
  ranked as (
    select
      argv,
      stdout,
      generated_at_epoch_ms,
      case
        when lower(list_extract(argv, list_count(argv))) = '--help' then 1
        when lower(list_extract(argv, list_count(argv))) = '--usage' then 2
        when lower(list_extract(argv, list_count(argv))) = '-?' then 3
        else 9
      end as preference
    from scenarios
    where coalesce(timed_out, false) = false
      and coalesce(stdout, '') <> ''
  ),
  ranked_help as (
    select *
    from ranked
    where preference < 9
  ),
  selected as (
    select *
    from ranked_help
    where preference = (select min(preference) from ranked_help)
  ),
  normalized as (
    select
      generated_at_epoch_ms,
      regexp_replace(
        replace(selected.stdout, '%s', manifest.binary_name),
        '(?i)usage:[ ]+[^ ]*/[^ ]*',
        'Usage: ' || manifest.binary_name
      ) as string_value
    from selected
    cross join manifest
  ),
  dedup as (
    select
      string_value,
      generated_at_epoch_ms,
      row_number() over (
        partition by string_value
        order by generated_at_epoch_ms
      ) as rk
    from normalized
  )
select
  string_value
from dedup
where rk = 1
order by generated_at_epoch_ms;
