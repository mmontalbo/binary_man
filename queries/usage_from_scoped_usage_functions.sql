-- Scope usage evidence to binary-specific usage/help functions when possible.
with
  manifest as (
    select lower(binary_name) as binary_name
    from read_json_auto('manifest.json')
  ),
  targets as (
    select
      '_usage_' || binary_name as usage_name,
      'single_binary_main_' || binary_name as main_name
    from manifest
  ),
  scoped as (
    select function_id
    from usage_help_functions, targets
    where lower(name) = targets.usage_name
       or lower(name) = targets.main_name
  ),
  selected_functions as (
    select function_id from scoped
    union all
    select function_id
    from usage_help_functions
    where not exists (select 1 from scoped)
  )
select
  a.status,
  a.basis,
  replace(coalesce(s.value, a.string_value), '%s', manifest.binary_name) as string_value
from callsite_arg_observations a
left join callsites c on c.callsite_id = a.callsite_id
left join strings s on s.string_id = a.string_id
cross join manifest
where a.kind = 'string'
  and c.from_function_id in (select function_id from selected_functions)
order by
  c.callsite_addr_int,
  a.arg_index,
  a.callsite_id;
