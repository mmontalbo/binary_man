-- Usage/help evidence lens for ls(1).
-- Placeholders: {{call_edges}}, {{callgraph_nodes}}, {{callsites}}, {{callsite_arg_observations}}, {{strings}}
WITH
call_edges AS (SELECT * FROM read_parquet('{{call_edges}}')),
callgraph_nodes AS (SELECT * FROM read_parquet('{{callgraph_nodes}}')),
callsite_arg_observations AS (SELECT * FROM read_parquet('{{callsite_arg_observations}}')),
callsites AS (SELECT * FROM read_parquet('{{callsites}}')),
strings AS (SELECT * FROM read_parquet('{{strings}}')),
usage_functions AS (
  SELECT function_id, name
  FROM callgraph_nodes
  WHERE name IN ('usage', '_usage_ls')
),
usage_callsites AS (
  SELECT c.callsite_id, c.callsite_addr_int, c.from_function_id, f.name AS from_function_name
  FROM callsites c
  JOIN usage_functions f ON f.function_id = c.from_function_id
),
string_args AS (
  SELECT callsite_id, arg_index, status, basis, string_id, string_value
  FROM callsite_arg_observations
  WHERE kind = 'string'
),
call_targets AS (
  SELECT e.callsite_id, e.to_function_id, n.name AS callee_name
  FROM call_edges e
  LEFT JOIN callgraph_nodes n ON n.function_id = e.to_function_id
),
usage_strings AS (
  SELECT
    u.from_function_id,
    u.from_function_name,
    u.callsite_id,
    u.callsite_addr_int,
    s.arg_index,
    s.status,
    s.basis,
    s.string_id,
    coalesce(s.string_value, strings.value) AS string_value,
    strings.tags AS tags,
    t.callee_name,
    t.to_function_id AS callee_id
  FROM usage_callsites u
  JOIN string_args s ON s.callsite_id = u.callsite_id
  LEFT JOIN strings ON strings.string_id = s.string_id
  LEFT JOIN call_targets t ON t.callsite_id = u.callsite_id
)
SELECT
  from_function_id,
  from_function_name,
  callsite_id,
  callsite_addr_int,
  arg_index,
  status,
  basis,
  string_id,
  string_value,
  tags,
  callee_name,
  callee_id,
  concat('evidence/decomp/f_', from_function_id, '.json') AS evidence_path
FROM usage_strings
WHERE string_value IS NOT NULL
  AND (
    coalesce(list_contains(tags, 'usage'), false)
    OR strpos(string_value, chr(10)) > 0
  )
ORDER BY callsite_addr_int, arg_index;
