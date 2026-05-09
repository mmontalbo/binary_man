# Discoveries

Interesting behaviors, inconsistencies, and bugs found during bgrid
development. Not all are confirmed bugs — some are design choices,
some are edge cases, some are genuine issues worth reporting upstream.

## Git: --stat + --shortstat duplicate summary line
Observed in: git diff, git log (shared diff machinery)
Version: git 2.50.1
Method: `combine` pairwise flag testing (535 cells)

When `--stat` and `--shortstat` are both specified, the summary line
("N files changed, M insertions(+), K deletions(-)") appears twice.
`--stat` includes a summary by default, and `--shortstat` adds its own
identical copy.

```
$ git diff --stat --shortstat
 code.py   | 3 +--
 data.txt  | 7 ++++---
 readme.md | 2 +-
 3 files changed, 6 insertions(+), 6 deletions(-)
 3 files changed, 6 insertions(+), 6 deletions(-)    ← duplicate
```

Reproducible regardless of flag order. Also appears in:
- `git diff -p --stat --shortstat` (triple combo)
- `git log --stat --shortstat`
- `git log --oneline --stat --shortstat`

## Git: --raw + --word-diff silently drops word-diff
Observed in: git diff
Version: git 2.50.1
Method: manual flag interaction probing (216 cells)

`--raw` combined with `-p` shows BOTH outputs (raw lines then patch).
But `--raw` combined with `--word-diff` shows ONLY raw output — the
word-diff is silently suppressed.

Since `--word-diff` is a variant of patch format (`-p`), these might be expected to
behave the same: either both concatenate or both override.

```
$ git diff --raw -p          # shows raw + patch (both)
$ git diff --raw --word-diff  # shows raw only (word-diff dropped)
```

## Git: --no-merges + --merges produces silent empty output
Observed in: git log
Version: git 2.50.1
Method: `combine` pairwise flag testing (152 cells)

Contradictory filters `--no-merges --merges` produce empty output with
exit 0 — no error, no warning. Both filters are applied (only merges
AND no merges = nothing matches).

Could arguably produce an error or warning since the flags are mutually
contradictory and the result is always empty.

## Git: Multiple --author flags are OR'd but --author + --grep is AND'd
Observed in: git log
Version: git 2.50.1
Method: `combine` pairwise flag testing (152 cells)

```
git log --author=Alice --author=Bob    # OR: shows both authors
git log --author=Alice --grep=fix      # AND: only Alice's fix commits
git log --grep=fix --grep=feature      # OR: shows both patterns
```

Same-type filters (author+author, grep+grep) combine as OR.
Cross-type filters (author+grep) combine as AND.
This is documented but the asymmetry is surprising.

---

## Git: -U-1 (negative context) produces corrupt unified diff header
Observed in: git diff
Version: git 2.50.1
Method: boundary-value probing (222 cells)

Passing a negative value to `-U` (context lines) is accepted silently
and produces a corrupt unified diff hunk header:

```
$ git diff -U-1
@@ -2,1- +2,1- @@ line1
```

Valid format: `@@ -start,count +start,count @@`
Actual output: count is `1-` (not a number), line offset is wrong (`-2`
instead of `-1`), and context text `line1` leaks into the header after `@@`.

Compare with -U0 (zero context, valid):
```
$ git diff -U0
@@ -1 +1 @@
```

Any tool that parses unified diff format (patch, diffstat, code review
tools, IDE integrations) would fail on the malformed header. One approach would be to
either reject negative -U values or clamp to 0.

The corruption scales with the magnitude of the negative value:

```
-U-1:   @@ -2,1- +2,1- @@ line1          (slightly wrong)
-U-2:   @@ -3 +3 @@ bbb                   (worse offset)
-U-100: @@ -101,195- +101,195- @@         (wildly corrupt: line 101 of a 5-line file, count "195-")
```

The negative value appears to be used in arithmetic that wraps or
overflows, producing progressively more corrupt output.

## Git: --word-diff-regex validation is lazy (only on use)
Observed in: git diff
Version: git 2.50.1
Method: boundary-value probing (352 cells)

`--word-diff --word-diff-regex=[invalid` produces exit 128 ("fatal:
invalid regular expression") — but only for contexts that have diffs.
Contexts with no changes exit 0 successfully. The regex is not validated
at parse time; it's only compiled when the diff engine actually needs it.

```
$ git diff --word-diff --word-diff-regex='[invalid'  # with changes: exit 128
$ git diff --word-diff --word-diff-regex='[invalid'  # clean repo: exit 0
```

This means the same command with the same flags succeeds or fails
depending on whether there are diffs to show — surprising and
inconsistent. 

## Git: -M101% (over 100% rename threshold) accepted silently
Observed in: git diff
Version: git 2.50.1
Method: boundary-value probing (222 cells)

`-M101%` is accepted without error. Since nothing can be >100% similar,
this effectively means "never detect renames" — same as omitting -M.


## Git: --skip=-1 (negative skip) silently ignored
Observed in: git log
Version: git 2.50.1
Method: boundary-value probing (352 cells)

`git log --skip=-1` is accepted without error and behaves as if
`--skip=0` (shows all commits, skips nothing). Negative skip values
are silently clamped or ignored.

```
$ git log --oneline               # 3 commits
$ git log --oneline --skip=1      # 2 commits (correct)
$ git log --oneline --skip=-1     # 3 commits (same as no skip)
```

Similarly, `git log -n -1` shows 1 commit (same as `-n 1`). Negative
limit values are silently treated as their absolute value.

---

## Git: grep -C -1 accepts negative but -A -1 and -B -1 reject
Observed in: git grep
Version: git 2.50.1
Method: boundary-value probing across git blame, show, format-patch, grep

`-A -1` and `-B -1` correctly error with "expects a non-negative integer
value." But `-C -1` is silently accepted and produces output with extra
context lines from between matches.

```
$ git grep -A -1 error    # error: expects non-negative (exit 129)
$ git grep -B -1 error    # error: expects non-negative (exit 129)
$ git grep -C -1 error    # SUCCESS: shows matches + mystery context (exit 0)
```

`-C N` is documented as equivalent to `-A N -B N`. If `-A` and `-B`
reject -1, but `-C` does not. The negative value is likely wrapping to a
large unsigned integer, producing context lines from between matches.

Same class of bug as `git diff -U-1` — negative numeric values accepted
by some code paths but not others.

## Git: --inter-hunk-context with negative value produces overlapping hunks
Observed in: git diff, git show, git format-patch (shared machinery)
Version: git 2.50.1
Method: boundary-value probing (152 cells)

`--inter-hunk-context=-100` produces a diff with overlapping hunks and
misclassified lines. Normal diff of a file with 3 changed lines in 5
produces one hunk. With `--inter-hunk-context=-100`, it produces three
overlapping hunks:

```
$ git diff --inter-hunk-context=-100
@@ -1,4 +1,4 @@       ← hunk 1: lines 1-4
-aaa
+AAA
 bbb
 CCC                   ← shown as context, but was actually changed from 'ccc'
 ddd
@@ -1,5 +1,5 @@       ← hunk 2: lines 1-5 (OVERLAPS hunk 1!)
 AAA
 bbb
-ccc
+CCC
 ddd
 EEE
@@ -2,4 +2,4 @@ aaa    ← hunk 3: lines 2-5 (OVERLAPS both!)
 bbb
 CCC
 ddd
-eee
+EEE
```

Hunks 1, 2, and 3 all cover overlapping line ranges. Changed lines
appear as context in hunks that already handled them. Applying this
patch would produce corrupt results. Worse than `-U-1` which only
corrupts headers — this corrupts the actual diff content.

## Git: format-patch -v -1 produces [PATCH v-1] in subject
Observed in: git format-patch
Version: git 2.50.1
Method: boundary-value probing (152 cells)

Negative version number is accepted and shown literally:
```
Subject: [PATCH v-1] initial
```



## Git: --stat --shortstat duplicate confirmed in show, format-patch
Observed in: git show, git format-patch (in addition to diff, log)
Version: git 2.50.1

The `--stat --shortstat` duplicate summary line behavior exists in every git
command that uses the diff output machinery. Confirmed in:
- `git diff --stat --shortstat`
- `git log --stat --shortstat`
- `git show --stat --shortstat`
- `git format-patch --stat --shortstat --stdout`

---

## Git: fetch --jobs=2147483647 OOM crash via unchecked calloc
Observed in: git fetch
Version: git 2.50.1
Method: boundary-value probing of remote operations

`git fetch --jobs=2147483647` crashes with `fatal: Out of memory,
calloc failed`. Git passes the user-supplied value directly to calloc
to pre-allocate a job array, without checking whether the value is
reasonable.

```
$ git fetch --jobs=2147483647 origin
fatal: Out of memory, calloc failed
```

Meanwhile `--jobs=-1` is silently accepted and works (the negative
value wraps or is treated as "auto"). And `--jobs=0` also works.

The `--jobs` flag controls parallelism for fetching from multiple
remotes or submodules. The natural upper bound is the number of things
to fetch (typically 1-100). A fix could allocate based on
`min(jobs, actual_work_items)` rather than trusting the user value
for an allocation size.

Same root cause as the OPT_INTEGER class: user input flows into a
resource allocation without bounds checking.

## Git: rev-list, repack, ls-files accept negative values silently
Observed in: git rev-list, git repack, git ls-files
Version: git 2.50.1
Method: boundary-value probing across subcommands

Multiple subcommands accept negative values for flags where only
non-negative values make sense:

```
$ git rev-list --max-count=-1 HEAD    # shows all (wraps to unlimited)
$ git rev-list --skip=-1 HEAD         # no effect (wraps to 0)
$ git rev-list --min-parents=-1 HEAD  # no filter (wraps)
$ git rev-list --max-parents=-1 HEAD  # no filter (wraps)
$ git repack --window=-1              # accepted silently
$ git repack --depth=-1               # accepted silently
$ git repack --threads=-1             # accepted silently
$ git ls-files --abbrev=-1            # shows full hash (wraps to max)
```

Compare with flags that correctly reject:
```
$ git shortlog -w-1,0,0    # exit 129 (rejected)
$ git tag -l -n -1          # exit 129 (rejected)
$ git for-each-ref --count=-1  # exit 129 (rejected)
```

Same inconsistent validation pattern as the diff/grep flags.

---
## jq: 1e999 roundtrip inconsistency — output doesn't parse back to same value
Observed in: jq 1.7.1
Method: boundary-value probing (595 cells)

`1e999` as a jq filter literal outputs `1E+999`. But when jq parses
`1E+999` as input, it becomes `1.7976931348623157e+308` (DBL_MAX).
jq's output doesn't survive a roundtrip through itself.

```
$ jq -n '1e999'                     # outputs: 1E+999
$ echo '1E+999' | jq '. + 1'        # outputs: 1.7976931348623157e+308
```

The filter literal preserves the string representation, but the
parser clamps to DBL_MAX. Additionally, `1E+999` is not valid JSON
per RFC 8259 (numbers must be finite), though some parsers accept it.

Related non-finite number handling inconsistencies:
- `nan` → `null` (mapped to JSON null)
- `infinite` → `1.7976931348623157e+308` (clamped to DBL_MAX)
- `1e999` → `1E+999` (preserved as invalid literal)

Three different strategies for three non-finite cases.

## jq: length(null) = 0 but length(bool) = error
Observed in: jq 1.7.1
Method: type-coercion probing (595 cells)

`null | length` returns 0, but `true | length` errors with "boolean
(true) has no length." Both are scalar types, but null is treated as
an empty container while booleans are rejected entirely.

## ripgrep: Behavioral observations (not bugs)

The following were initially reported as bugs but on review are
defensible design choices that produce valid output.

**--json + -l/-c flag ordering:** `rg --json -l` outputs plain text
(last flag wins), while `rg -l --json` outputs JSON. This is the
standard "last flag wins" convention. `--json` + `--stats` composes
because stats ADD data, while `-l` CHANGES the output mode entirely.
Different interaction types, not an inconsistency.

**-F always overrides -P:** `-F` (fixed string) takes unconditional
precedence over `-P` (PCRE). This is a safety design — fixed string
mode prevents accidental regex injection. A user with `-F` in an alias
who adds `-P` gets the safer behavior.

## Observation: Root cause pattern — OPT_INTEGER vs OPT_UNSIGNED misuse

**Scope:** ~19 of 39 integer flag definitions across git's codebase
**Source file:** `parse-options.h`, various `builtin/*.c`
Method: tracing bug class back to option parsing macros

Git's parse-options system has two numeric types:
- `OPTION_INTEGER` — accepts any integer (positive, negative, zero)
- `OPTION_UNSIGNED` — validates non-negative (rejects negative)

These findings all trace to flags using `OPT_INTEGER` when they
could use `OPT_UNSIGNED`:

```
// parse-options.h — shared diff macros use INTEGER:
#define OPT_DIFF_UNIFIED(v)             OPT_INTEGER_F(...)   // uses INTEGER
#define OPT_DIFF_INTERHUNK_CONTEXT(v)   OPT_INTEGER_F(...)   // uses INTEGER

// builtin/grep.c — -A and -B use UNSIGNED, but -C uses CALLBACK:
OPT_UNSIGNED('B', "before-context", ...)    // CORRECT
OPT_UNSIGNED('A', "after-context", ...)     // CORRECT
OPT_CALLBACK('C', "context", ...)           // MISSING validation
```

Additionally, `PARSE_OPT_NONEG` (used on -U and --inter-hunk-context)
prevents `--no-unified` (boolean negation) but does NOT prevent `-U-1`
(negative numeric value). The flag name is misleading — developers
likely added it thinking it guarded against negative values.

Possible fix: Change ~22 `OPT_INTEGER` declarations to `OPT_UNSIGNED`
for flags where negative values are nonsensical (max-depth, max-count,
jobs, timeout, width, padding, depth, etc.). grep's -C callback could
validate non-negative. Add upper-bound validation for flags used as
allocation sizes (`--jobs` could clamp to actual work items, not
calloc the raw user value). This is a mechanical, low-risk change
using git's existing `OPTION_UNSIGNED` type.

---

## Git: inconsistent error messages for diff flags outside a repository
Observed in: git diff
Version: git 2.50.1
Method: automated exploration without a git repo context (2756 cells)

When `git diff` is run outside a repository, flags produce two different
error messages depending on when they're registered in git's option parser:

```
$ git diff --raw        → "warning: Not a git repository."
$ git diff --numstat    → "warning: Not a git repository."
$ git diff --cached     → "error: unknown option `cached'"
$ git diff --staged     → "error: unknown option `staged'"
$ git diff --merge-base → "error: unknown option `merge-base'"
```

Core diff formatting flags (`--raw`, `--numstat`, `--stat`, `--shortstat`,
`--name-only`, `--name-status`, `--no-color`, `--quiet`, `--summary`,
`--text`, `--unified`, `--minimal`, `--no-prefix`, `--no-renames`,
`--relative`, `--submodule`) are registered unconditionally and produce
the correct "Not a git repository" error.

Repo-specific flags (`--cached`, `--staged`, `--merge-base`, `--cc`,
`--combined`, `--ours`, `--theirs`, `--base`, `--refresh`) are only
registered when a repository is detected, so outside a repo they appear
as "unknown" — even though they are valid `git diff` flags.

All flags could produce a consistent "not a git repository" message
when run outside a repo.

