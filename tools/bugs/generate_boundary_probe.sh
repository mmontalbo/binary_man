#!/usr/bin/env bash
# Usage: ./generate_boundary_probe.sh <binary> [subcommand args...]
# Generates a boundary-value probe mechanically from discovery output.
# No LM needed — purely rule-based.
#
# Example:
#   ./generate_boundary_probe.sh sort > sort_boundary.probe
#   bgrid sort sort_boundary.probe

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
BGRID="$REPO_DIR/target/release/bgrid"

BINARY="$1"
shift
SUBCMD_ARGS=("$@")

# Get discovery output
DISCOVERY=$("$BGRID" "$BINARY" "${SUBCMD_ARGS[@]}" 2>/dev/null)
if [ -z "$DISCOVERY" ]; then
    echo "ERROR: no discovery output for $BINARY" >&2
    exit 1
fi

# Extract flag info from discovery
SHORT_FLAGS=$(echo "$DISCOVERY" | grep -oP '(?<=run ")(-[a-zA-Z0-9])' | sort -u)
LONG_FLAGS=$(echo "$DISCOVERY" | grep -oP '(?<=run ")(--[a-zA-Z][-a-zA-Z0-9]*)' | sort -u)
LONG_VALUE_FLAGS=$(echo "$DISCOVERY" | grep -oP '(?<=run ")(--[a-zA-Z][-a-zA-Z0-9]*=)[^"]*"' | sed 's/=.*//' | sort -u)

# Detect base args from discovery (first run line)
BASE_RUN=$(echo "$DISCOVERY" | grep '^run ' | head -1 | sed 's/^run //')

# Detect pattern arg
PAT=$(echo "$DISCOVERY" | grep '^run ' | head -1 | grep -oP '^run "[^"]*" "[^"]*"' | awk -F'"' '{print $2}')
FIL=$(echo "$DISCOVERY" | grep '^run ' | head -1 | grep -oP '^run "[^"]*" "[^"]*"' | awk -F'"' '{print $4}')
if [ -z "$FIL" ]; then
    FIL=$(echo "$DISCOVERY" | grep '^run ' | head -1 | awk -F'"' '{print $2}')
    PAT=""
fi

# Build run helper
build_run() {
    local parts=""
    for arg in "$@"; do parts="$parts \"$arg\""; done
    echo "run$parts"
}

# Emit contexts and vary blocks from discovery as-is (stop before first run)
echo "$DISCOVERY" | awk '/^run /{exit} {print}'
echo ""

echo "# Boundary value testing — generated mechanically"
echo "# Tests: negative values, zero, overflow for numeric flags"
echo "# Tests: contradictory flag pairs"
echo "# Tests: empty string values"
echo ""

# --- Numeric boundary testing ---
# Find flags that had numeric values in discovery (=N patterns)
NUMERIC_FLAGS=$(echo "$DISCOVERY" | grep -oP '"--[a-zA-Z][-a-zA-Z0-9]*=[0-9]+"' | sed 's/=.*//' | tr -d '"' | sort -u)

if [ -n "$NUMERIC_FLAGS" ]; then
    echo "# Numeric flag boundaries"
    for flag in $NUMERIC_FLAGS; do
        for val in 0 -1 2147483647 2147483648; do
            if [ -n "$PAT" ]; then
                echo "run \"${flag}=${val}\" \"$PAT\" \"$FIL\""
            elif [ -n "$FIL" ]; then
                echo "run \"${flag}=${val}\" \"$FIL\""
            else
                echo "run \"${flag}=${val}\""
            fi
        done
    done
    echo ""
fi

# Short flags that likely take numeric args (from context: -n, -c, -w, -f, -s, etc.)
# Test common short flags with negative values
echo "# Short flag negative values"
for flag in $SHORT_FLAGS; do
    # Test each short flag with -1 as a separate arg
    if [ -n "$PAT" ]; then
        echo "run \"$flag\" \"-1\" \"$PAT\" \"$FIL\""
    elif [ -n "$FIL" ]; then
        echo "run \"$flag\" \"-1\" \"$FIL\""
    else
        echo "run \"$flag\" \"-1\""
    fi
done
echo ""

# --- Contradictory flag pairs ---
echo "# Contradictory flag pairs"
# Find --X and --no-X pairs
for flag in $LONG_FLAGS; do
    noflag="--no-${flag#--}"
    if echo "$LONG_FLAGS" | grep -qxF -- "$noflag"; then
        if [ -n "$PAT" ]; then
            echo "run \"$flag\" \"$noflag\" \"$PAT\" \"$FIL\""
        elif [ -n "$FIL" ]; then
            echo "run \"$flag\" \"$noflag\" \"$FIL\""
        else
            echo "run \"$flag\" \"$noflag\""
        fi
    fi
done
echo ""

# --- Empty string values for value-taking flags ---
if [ -n "$LONG_VALUE_FLAGS" ]; then
    echo "# Empty string values"
    for flag in $LONG_VALUE_FLAGS; do
        if [ -n "$PAT" ]; then
            echo "run \"${flag}=\" \"$PAT\" \"$FIL\""
        elif [ -n "$FIL" ]; then
            echo "run \"${flag}=\" \"$FIL\""
        else
            echo "run \"${flag}=\""
        fi
    done
    echo ""
fi

# --- Flag-like filename ---
echo "# Flag-like filename"
if [ -n "$PAT" ]; then
    echo "run \"--\" \"$PAT\" \"-rf\""
else
    echo "run \"--\" \"-rf\""
fi
