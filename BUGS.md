# Bugs and Inconsistencies Found by bman

All findings are from systematic behavioral observation using bman's
grid execution and pairwise flag combination testing.

## Git: --stat + --shortstat duplicate summary line

**Severity:** Low (UI bug)
**Affected:** git diff, git log (shared diff machinery)
**Reproduced on:** git 2.50.1
**Found by:** `combine` pairwise flag testing (535 cells)

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

**Severity:** Low (inconsistency)
**Affected:** git diff
**Reproduced on:** git 2.50.1
**Found by:** manual flag interaction probing (216 cells)

`--raw` combined with `-p` shows BOTH outputs (raw lines then patch).
But `--raw` combined with `--word-diff` shows ONLY raw output — the
word-diff is silently suppressed.

Since `--word-diff` is a variant of patch format (`-p`), these should
behave the same: either both concatenate or both override.

```
$ git diff --raw -p          # shows raw + patch (both)
$ git diff --raw --word-diff  # shows raw only (word-diff dropped)
```

## Git: --no-merges + --merges produces silent empty output

**Severity:** Informational (questionable UX)
**Affected:** git log
**Reproduced on:** git 2.50.1
**Found by:** `combine` pairwise flag testing (152 cells)

Contradictory filters `--no-merges --merges` produce empty output with
exit 0 — no error, no warning. Both filters are applied (only merges
AND no merges = nothing matches).

Arguably should produce an error or warning since the flags are mutually
contradictory and the result is always empty.

## Git: Multiple --author flags are OR'd but --author + --grep is AND'd

**Severity:** Informational (inconsistent semantics)
**Affected:** git log
**Reproduced on:** git 2.50.1
**Found by:** `combine` pairwise flag testing (152 cells)

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

**Severity:** Medium (produces malformed output that breaks parsers)
**Affected:** git diff
**Reproduced on:** git 2.50.1
**Found by:** boundary-value probing (222 cells)

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
tools, IDE integrations) would fail on the malformed header. Git should
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

**Severity:** Low (inconsistent error handling)
**Affected:** git diff
**Reproduced on:** git 2.50.1
**Found by:** boundary-value probing (352 cells)

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
inconsistent. The regex should be validated eagerly.

## Git: -M101% (over 100% rename threshold) accepted silently

**Severity:** Low (nonsensical input, benign behavior)
**Affected:** git diff
**Reproduced on:** git 2.50.1
**Found by:** boundary-value probing (222 cells)

`-M101%` is accepted without error. Since nothing can be >100% similar,
this effectively means "never detect renames" — same as omitting -M.
Should arguably produce a warning.

## Git: --skip=-1 (negative skip) silently ignored

**Severity:** Low (nonsensical input, benign behavior)
**Affected:** git log
**Reproduced on:** git 2.50.1
**Found by:** boundary-value probing (352 cells)

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

**Severity:** Medium (inconsistent input validation, wrong output)
**Affected:** git grep
**Reproduced on:** git 2.50.1
**Found by:** boundary-value probing across git blame, show, format-patch, grep

`-A -1` and `-B -1` correctly error with "expects a non-negative integer
value." But `-C -1` is silently accepted and produces output with extra
context lines that shouldn't be there.

```
$ git grep -A -1 error    # error: expects non-negative (exit 129)
$ git grep -B -1 error    # error: expects non-negative (exit 129)
$ git grep -C -1 error    # SUCCESS: shows matches + mystery context (exit 0)
```

`-C N` is documented as equivalent to `-A N -B N`. If `-A` and `-B`
reject -1, `-C` should too. The negative value is likely wrapping to a
large unsigned integer, producing context lines from between matches.

Same class of bug as `git diff -U-1` — negative numeric values accepted
by some code paths but not others.

## Git: --stat --shortstat duplicate confirmed in show, format-patch

**Severity:** Low (shared diff machinery)
**Affected:** git show, git format-patch (in addition to diff, log)
**Reproduced on:** git 2.50.1

The `--stat --shortstat` duplicate summary line bug exists in every git
command that uses the diff output machinery. Confirmed in:
- `git diff --stat --shortstat`
- `git log --stat --shortstat`
- `git show --stat --shortstat`
- `git format-patch --stat --shortstat --stdout`

---

*All bugs found by bman's systematic behavioral probing across ~3000+
cells. Methods used: pairwise flag combination testing (`combine`),
boundary-value probing (negative/zero/extreme values), compound input
perturbation (`vary compound`), and adversarial context design.*

## Summary

| Finding | Method | Severity |
|---------|--------|----------|
| `--stat --shortstat` duplicate summary | `combine` pairwise | Low (UI) |
| `-U` negative corrupt hunk headers | boundary-value | Medium (breaks parsers) |
| `--raw + --word-diff` asymmetry | flag interaction | Low (inconsistency) |
| `--author + --author = OR` vs `--author + --grep = AND` | `combine` pairwise | Informational |
| `--word-diff-regex` lazy validation | boundary-value | Low (inconsistency) |
| `-M101%` accepted silently | boundary-value | Informational |
| `--skip=-1` ignored silently | boundary-value | Informational |
| `--no-merges + --merges` silent empty | `combine` pairwise | Informational |
| `grep -C -1` accepts negative, `-A`/`-B` reject | boundary-value | Medium (inconsistent validation) |
| `--stat --shortstat` duplicate in show, format-patch | pairwise | Low (shared machinery) |
| `-U-1` corrupt header in show | boundary-value | Medium (shared machinery) |
| `blame -M101%` accepted | boundary-value | Informational (shared) |

*All bugs found by bman's systematic pairwise flag combination testing.
The `combine` keyword generates all single + pair combinations from a
list of flags, enabling automated discovery of flag interaction issues
that single-flag testing misses.*
