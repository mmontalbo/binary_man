-- SPECIFICATION: outputs_differ assertion behavior
--
-- Documents intended behavior. Self-contained test.
--
-- Rules:
-- 1. If baseline_exit_code IS NULL, return NULL (no baseline to compare)
-- 2. If baseline_exit_code != 0, return NULL (can't evaluate)
-- 3. If variant_exit_code != 0, return NULL (can't evaluate)
-- 4. If both exit 0 and outputs differ, return 1 (verified)
-- 5. If both exit 0 and outputs equal, return 0 (not verified)

WITH test_cases AS (
    SELECT * FROM (VALUES
        ('no_baseline', NULL, 0, NULL, 'output', NULL),
        ('baseline_error', 129, 0, 'usage msg', '', NULL),
        ('variant_error', 0, 128, 'good', 'fatal', NULL),
        ('both_error', 1, 1, 'err1', 'err2', NULL),
        ('both_ok_differ', 0, 0, 'old', 'new', 1),
        ('both_ok_equal', 0, 0, 'same', 'same', 0),
        ('both_ok_empty', 0, 0, '', '', 0)
    ) AS t(name, baseline_exit, variant_exit, baseline_stdout, variant_stdout, expected)
),

evaluated AS (
    SELECT
        name, expected,
        CASE
            WHEN baseline_exit IS NULL THEN NULL  -- No baseline to compare against
            WHEN baseline_exit <> 0 THEN NULL
            WHEN variant_exit <> 0 THEN NULL
            WHEN variant_stdout <> baseline_stdout THEN 1
            ELSE 0
        END AS actual
    FROM test_cases
)

SELECT
    name,
    COALESCE(CAST(expected AS VARCHAR), 'NULL') AS expected,
    COALESCE(CAST(actual AS VARCHAR), 'NULL') AS actual,
    CASE
        WHEN expected IS NOT DISTINCT FROM actual THEN 'PASS'
        ELSE 'FAIL'
    END AS status
FROM evaluated
ORDER BY status DESC, name;
