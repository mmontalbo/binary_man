-- Prefer scenario-generated help output for usage evidence.
-- `{{scenarios_glob}}` is an absolute glob to inventory/scenarios/*.json.
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
    from read_json_auto('{{scenarios_glob}}')
    where scenario_id like 'help--%'
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
    select
      *,
      row_number() over (
        order by preference, generated_at_epoch_ms desc
      ) as rk
    from ranked_help
  ),
  normalized as (
    select
      generated_at_epoch_ms,
      regexp_replace(
        replace(
          regexp_replace(selected.stdout, '\x1b\\[[0-9;?]*[ -/]*[@-~]', ''),
          '%s',
          manifest.binary_name
        ),
        '(?i)usage:[ ]+[^ ]*/[^ ]*',
        'Usage: ' || manifest.binary_name
      ) as string_value
    from selected
    cross join manifest
    where rk = 1
  )
select
  string_value
from normalized
order by generated_at_epoch_ms desc;
