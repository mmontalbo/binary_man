-- Prefer probe-generated help output for usage evidence.
with
  manifest as (
    select binary_name
    from read_json_auto('manifest.json')
  ),
  probes as (
    select
      argv,
      stdout,
      timed_out,
      generated_at_epoch_ms
    from read_json_auto('../inventory/probes/*.json')
  ),
  ranked as (
    select
      argv,
      stdout,
      generated_at_epoch_ms,
      case
        when lower(list_extract(argv, list_count(argv))) = '--help' then 1
        when lower(list_extract(argv, list_count(argv))) = '-h' then 2
        when lower(list_extract(argv, list_count(argv))) = 'help' then 3
        when lower(list_extract(argv, list_count(argv))) = '--usage' then 4
        else 9
      end as preference
    from probes
    where coalesce(timed_out, false) = false
      and coalesce(stdout, '') <> ''
  ),
  selected as (
    select *
    from ranked
    where preference = (select min(preference) from ranked)
  )
select
  'resolved' as status,
  'string_direct' as basis,
  regexp_replace(
    selected.stdout,
    '(?i)usage:[ ]+[^ ]*/[^ ]*',
    'Usage: ' || manifest.binary_name
  ) as string_value
from selected
cross join manifest
order by selected.generated_at_epoch_ms;
