#!/usr/bin/env bash
# Usage: ./suggest_followup.sh <results-file> <binary>
# Reads collapsed results and generates a follow-up probe with:
#   1. Interaction testing — pairwise flag combinations within identical groups
#   2. Sensitivity refinement — graduated variants along dimensions that showed signal
#   3. Content variation — diverse content types to split remaining identical groups

RESULTS="$1"
BINARY="$2"

if [ ! -f "$RESULTS" ]; then
    echo "Usage: $0 <results-file> <binary>" >&2
    exit 1
fi

# --- Phase 1: Parse compact results ---

# Extract multi-run groups
MULTI_GROUPS=$(grep -a "^## group" "$RESULTS" | grep -v "(1 runs)")
N_GROUPS=$(echo "$MULTI_GROUPS" | grep -c . 2>/dev/null || echo 0)

# Extract sensitivity labels from full results
SENSITIVITIES=$(grep -a "sensitive to:" "$RESULTS" | sed 's/.*sensitive to: //' | head -20)

# Detect filenames referenced in runs
FILES=$(echo "$MULTI_GROUPS" | grep -oP '"\K[^"]+(?=")' | grep '\.' | grep -v '^-' | sort -u)

if [ "$N_GROUPS" -eq 0 ] && [ -z "$SENSITIVITIES" ]; then
    echo "# No identical groups or sensitivity data — nothing to follow up" >&2
    exit 0
fi

echo "# Follow-up probe for $BINARY"
echo "# Generated from: $RESULTS"
echo "# Strategies: interaction testing, sensitivity refinement, content variation"
echo ""

# --- Phase 2: Interaction testing via combine ---

interaction_count=0
groups_emitted=0

echo "$MULTI_GROUPS" | sort -t'(' -k2 -rn | while IFS= read -r group; do
    [ -z "$group" ] && continue
    [ "$groups_emitted" -ge 2 ] && break  # cap at 2 groups

    runs_str=$(echo "$group" | sed 's/^[^:]*: //')

    # Extract per-run flag lists and common positional args
    # Use perl to parse runs and separate flags from files
    parsed=$(echo "$runs_str" | perl -e '
        my $input = <STDIN>;
        chomp $input;
        my @runs = split /,\s+(?=")/, $input;
        my %flags;
        my $base = "";
        for my $r (@runs) {
            $r =~ s/^\s+|\s+$//g;
            next unless $r =~ /^"/;
            my @tokens = ($r =~ /"([^"]+)"/g);
            my @f;
            my @p;
            for my $t (@tokens) {
                if ($t =~ /^-/) { push @f, $t; }
                else { push @p, $t; }
            }
            my $flag_key = join(" ", @f);
            $flags{$flag_key} = 1 if @f;
            $base = join(" ", map { "\"$_\"" } @p) unless $base;
        }
        my @unique_flags = keys %flags;
        print "BASE=$base\n";
        print "COUNT=" . scalar(@unique_flags) . "\n";
        for my $f (@unique_flags) {
            print "FLAG=$f\n";
        }
    ')

    base=$(echo "$parsed" | grep "^BASE=" | sed 's/^BASE=//')
    count=$(echo "$parsed" | grep "^COUNT=" | sed 's/^COUNT=//')
    flags=$(echo "$parsed" | grep "^FLAG=" | sed 's/^FLAG=//')

    # Only generate combine for 3+ unique flags
    if [ -n "$count" ] && [ "$count" -ge 3 ]; then
        # Cap at 8 flags
        flag_list=$(echo "$flags" | head -8)
        n_flags=$(echo "$flag_list" | wc -l)

        if [ "$interaction_count" -eq 0 ]; then
            echo "# === Interaction testing ==="
            echo "# Test pairwise flag combinations within identical groups"
            echo ""
        fi

        echo "# Group: $runs_str"
        echo "combine $base"
        echo "$flag_list" | while IFS= read -r f; do
            # Each flag may be multiple tokens like "-s -u"
            # Wrap each token in quotes
            quoted=$(echo "$f" | awk '{for(i=1;i<=NF;i++) printf "\"%s\" ", $i; print ""}' | sed 's/ $//')
            echo "  $quoted"
        done
        echo ""

        interaction_count=$((interaction_count + 1))
        groups_emitted=$((groups_emitted + 1))
    fi
done

# --- Helper: emit file lines ---
emit_files() {
    local content="$1"
    for f in $FILES; do
        echo "  file \"$f\" $content"
    done
}

# --- Emit base context for vary blocks ---
echo "context \"alpha\""
emit_files '"cherry" "apple" "banana" "date" "elderberry"'
echo ""

# --- Phase 3: Sensitivity refinement ---

if [ -n "$SENSITIVITIES" ]; then
    # Parse sensitivity labels and classify by dimension
    dimensions=$(echo "$SENSITIVITIES" | perl -e '
        my %dims;
        while (<STDIN>) {
            chomp;
            # Split on ", " not inside parens
            my @labels = split /,\s+(?![^(]*\))/, $_;
            for my $label (@labels) {
                $label =~ s/^\s+|\s+$//g;
                $label =~ s/\s*\([^)]*\)\s*$//;  # strip effect annotation
                if ($label =~ /^remove\s+(\S+)/) { print "remove:$1\n"; }
                elsif ($label =~ /(\S+)=size:/) { print "size:$1\n"; }
                elsif ($label =~ /(\S+)=empty/) { print "empty:$1\n"; }
                elsif ($label =~ /(\S+)\s+mtime/) { print "mtime:$1\n"; }
                elsif ($label =~ /(\S+)\s+readonly/) { print "perms:$1\n"; }
                elsif ($label =~ /^env\s+(\S+)/) { print "env:$1\n"; }
            }
        }
    ' | sort | uniq -c | sort -rn | head -3)

    if [ -n "$dimensions" ]; then
        echo "# === Sensitivity refinement ==="
        echo "# Graduated variants along dimensions that showed behavioral splits"
        echo ""

        # Need a base context — use first file for the vary blocks
        first_file=$(echo "$FILES" | head -1)
        [ -z "$first_file" ] && first_file="input.txt"

        echo "$dimensions" | while read -r count dim_spec; do
            dim=$(echo "$dim_spec" | cut -d: -f1)
            path=$(echo "$dim_spec" | cut -d: -f2)

            case "$dim" in
                size)
                    echo "# Sensitivity: file size (seen $count times)"
                    echo "vary from \"alpha\""
                    echo "  file \"$path\" size 1"
                    echo "  file \"$path\" size 100"
                    echo "  file \"$path\" size 1000"
                    echo "  file \"$path\" size 10000"
                    echo "  file \"$path\" size 100000"
                    echo ""
                    ;;
                mtime)
                    echo "# Sensitivity: modification time (seen $count times)"
                    echo "vary from \"alpha\""
                    echo "  props \"$path\" mtime old"
                    echo "  props \"$path\" mtime recent"
                    echo ""
                    ;;
                remove)
                    echo "# Sensitivity: file existence (seen $count times)"
                    echo "vary from \"alpha\""
                    echo "  remove \"$path\""
                    echo "  file \"$path\" empty"
                    echo "  file \"$path\" -> \"nonexistent\""
                    echo ""
                    ;;
                empty)
                    echo "# Sensitivity: empty content (seen $count times)"
                    echo "vary from \"alpha\""
                    echo "  file \"$path\" empty"
                    echo "  file \"$path\" \"single line\""
                    echo "  file \"$path\" size 1"
                    echo ""
                    ;;
                perms)
                    echo "# Sensitivity: permissions (seen $count times)"
                    echo "vary from \"alpha\""
                    echo "  props \"$path\" readonly"
                    echo "  props \"$path\" executable"
                    echo ""
                    ;;
                env)
                    echo "# Sensitivity: environment variable (seen $count times)"
                    echo "vary from \"alpha\""
                    echo "  env $path \"alternate\""
                    echo "  env $path \"\""
                    echo "  remove env $path"
                    echo ""
                    ;;
            esac
        done
    fi
fi

# --- Phase 4: Content variation contexts ---

echo "# === Content variation ==="
echo "# Diverse content types to split remaining identical groups"
echo ""
for strategy in months duplicates case_mixed numeric whitespace special_chars unicode; do
    echo "context \"$strategy\""
    case "$strategy" in
        months)       emit_files '"Jan" "Mar" "Feb" "Dec" "Apr" "Nov"' ;;
        duplicates)   emit_files '"aaa" "aaa" "bbb" "bbb" "ccc" "aaa"' ;;
        case_mixed)   emit_files '"Apple" "BANANA" "cherry" "apple" "CHERRY" "banana"' ;;
        numeric)      emit_files '"100" "2" "30" "1" "20" "3"' ;;
        whitespace)   emit_files '"  leading" "trailing  " "  both  " "none" "\ttabbed"' ;;
        special_chars) emit_files '"hello!" "@world" "foo#bar" "a:b:c" "x=y=z"' ;;
        unicode)      emit_files '"αβγ" "δεζ" "ηθι" "κλμ"' ;;
    esac
    echo ""
done

# --- Phase 5: Untested flags ---

# Parse "# Not tested" and "# Aliases" lines from results
UNTESTED=$(grep -a "^# Not tested" "$RESULTS" | sed 's/^# Not tested ([^)]*): //')
ALIAS_LINE=$(grep -a "^# Aliases:" "$RESULTS" | sed 's/^# Aliases: //')

if [ -n "$UNTESTED" ]; then
    # Parse alias map into a lookup
    # Format: -a = --all, -r = --reverse, ...
    alias_tested=""
    if [ -n "$ALIAS_LINE" ]; then
        # For each untested flag, check if its alias was tested
        # (tested = appears in a run in the results but NOT in the untested list)
        alias_tested=$(printf '%s\n%s' "$ALIAS_LINE" "$UNTESTED" | perl -e '
            my $aliases = <STDIN>;
            chomp $aliases;
            my $untested = <STDIN>;
            chomp $untested;
            my %map;
            while ($aliases =~ /(-\S+)\s*=\s*(--?\S+)/g) {
                $map{$1} = $2;
                $map{$2} = $1;
            }
            my %unt = map { s/^\s+|\s+$//gr => 1 } split /,\s*/, $untested;
            # Print only flags where neither form was tested
            for my $f (sort keys %unt) {
                my $alias = $map{$f} // "";
                # Skip if alias exists and alias is NOT in untested (meaning alias was tested)
                next if $alias && !$unt{$alias};
                # Skip --help and --version
                next if $f eq "--help" || $f eq "--version";
                # For alias pairs where both are untested, only print the short form
                next if $f =~ /^--/ && $alias && $unt{$alias};
                print "$f\n";
            }
        ')
    else
        alias_tested=$(echo "$UNTESTED" | tr ',' '\n' | sed 's/^\s*//')
    fi

    # Determine base args for the runs
    first_file=$(echo "$FILES" | head -1)

    if [ -n "$alias_tested" ]; then
        truly_untested=$(echo "$alias_tested" | grep -c . 2>/dev/null || echo 0)
        if [ "$truly_untested" -gt 0 ]; then
            echo ""
            echo "# === Untested flags ==="
            echo "# $truly_untested flags discovered but not tested in any form"
            echo ""
            echo "$alias_tested" | head -20 | while IFS= read -r flag; do
                [ -z "$flag" ] && continue
                if [ -n "$first_file" ]; then
                    echo "run \"$flag\" \"$first_file\""
                else
                    echo "run \"$flag\""
                fi
            done
            echo ""
        fi
    fi
fi

# --- Phase 6: Re-emit grouped runs ---

echo "# Re-test identical groups across all contexts"

echo "$MULTI_GROUPS" | while IFS= read -r group; do
    [ -z "$group" ] && continue
    runs_str=$(echo "$group" | sed 's/^[^:]*: //')

    echo ""
    echo "# Group: $runs_str"

    echo "$runs_str" | perl -ne '
        my @runs = split /,\s+(?=")/, $_;
        for my $r (@runs) {
            $r =~ s/^\s+|\s+$//g;
            print "run $r\n" if $r =~ /^"/;
        }
    '
done
