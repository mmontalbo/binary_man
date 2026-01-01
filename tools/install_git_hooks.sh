#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
hook_repo_dir="$repo_root/tools/git_hooks"
git_hook_dir="$repo_root/.git/hooks"

if [[ ! -d "$git_hook_dir" ]]; then
    echo "error: $git_hook_dir does not exist; is this a Git checkout?" >&2
    exit 1
fi

mkdir -p "$hook_repo_dir"

for hook in commit-msg; do
    src="$hook_repo_dir/$hook"
    dest="$git_hook_dir/$hook"

    if [[ ! -f "$src" ]]; then
        echo "warning: missing hook script $src, skipping" >&2
        continue
    fi

    ln -sf "../../tools/git_hooks/$hook" "$dest"
    chmod +x "$src"
done

echo "Installed Git hooks into $git_hook_dir"
