#!/usr/bin/env bash
# Usage: ./suggest_followup.sh <results-file> <binary>
# Reads collapsed results, finds groups with >1 run that are identical,
# and generates a follow-up probe with varied content to try to split them.

RESULTS="$1"
BINARY="$2"

# Check for multi-run groups
N_GROUPS=$(grep -a "^## group" "$RESULTS" | grep -cv "(1 runs)" || true)

if [ "$N_GROUPS" -eq 0 ]; then
    echo "# No identical groups found — nothing to split" >&2
    exit 0
fi

echo "# Follow-up probe for $BINARY"
echo "# Generated from: $RESULTS"
echo "# Targeting $N_GROUPS identical groups with varied content"
echo ""

# Detect filenames referenced in runs (quoted strings that look like filenames)
FILES=$(grep -a "^## group" "$RESULTS" | grep -v "(1 runs)" | \
    grep -oP '"\K[^"]+(?=")' | grep '\.' | grep -v '^-' | sort -u)

# Generate a helper to emit file lines for all detected filenames
emit_files() {
    local content="$1"
    for f in $FILES; do
        echo "  file \"$f\" $content"
    done
}

# Generate contexts with different content perturbation strategies
echo "# Strategy: isolate flag behaviors by varying content type"
for strategy in months duplicates case_mixed numeric whitespace special_chars unicode; do
    echo "context \"$strategy\""
    case "$strategy" in
        months)       emit_files '"Jan" "Mar" "Feb" "Dec" "Apr" "Nov"' ;;
        duplicates)   emit_files '"aaa" "bbb" "aaa" "ccc" "bbb" "aaa"' ;;
        case_mixed)   emit_files '"Apple" "BANANA" "cherry" "apple" "CHERRY" "banana"' ;;
        numeric)      emit_files '"100" "2" "30" "1" "20" "3"' ;;
        whitespace)   emit_files '"  leading" "trailing  " "  both  " "none" "\ttabbed"' ;;
        special_chars) emit_files '"hello!" "@world" "foo#bar" "a:b:c" "x=y=z"' ;;
        unicode)      emit_files '"αβγ" "δεζ" "ηθι" "κλμ"' ;;
    esac
    echo ""
done

echo ""
echo "# Re-test identical groups across all content strategies"

# Parse each multi-run group and emit runs
grep -a "^## group" "$RESULTS" | grep -v "(1 runs)" | while IFS= read -r group; do
    # Extract everything after ": " (the run list)
    runs_str=$(echo "$group" | sed 's/^[^:]*: //')

    echo ""
    echo "# Group: $runs_str"

    # Each run is separated by ", " at the boundary between quoted args
    # Split on the pattern: ", " followed by a quote (start of next run)
    # Use perl for reliable splitting
    echo "$runs_str" | perl -ne '
        # Split on ", " that is followed by a double quote
        my @runs = split /,\s+(?=")/, $_;
        for my $r (@runs) {
            $r =~ s/^\s+|\s+$//g;
            print "run $r\n" if $r =~ /^"/;
        }
    '
done
