#!/usr/bin/env bash
#
# Fail if a change to the code carries no changelog fragment.
#
# The entry is the part that gets skipped, because it is the part that is not
# needed to make the tests pass. This is the gate: a branch that touches
# `crates/`, `plugins/` or `templates/` has to add a file under `changelog.d/`
# saying what changed, or say in that file that nothing needs saying.
#
#   scripts/changelog-check.sh                  # against origin/main
#   scripts/changelog-check.sh <base-ref>       # against something else
#
# CI passes the pull request's base SHA. Locally, the default is `origin/main`,
# which is what the branch will be merged into.
#
# THE ESCAPE HATCH IS A FRAGMENT, NOT A LABEL. A change that genuinely needs no
# entry writes a fragment whose whole body is the single word `none`. A pull
# request label would work too and was the alternative; a file wins because it
# travels with the branch, shows up in the diff where a reviewer is already
# looking, needs no API call and no permissions, and works identically from a
# fork and on a local run of this script. `scripts/changelog-fold.sh` deletes a
# `none` fragment along with the rest and writes nothing to CHANGELOG.md.
set -euo pipefail

# A change under one of these needs an entry. Everything else — tooling, CI,
# documentation, fixtures — does not, which is not a claim that such changes
# never deserve an entry, only that a gate cannot tell.
watched_paths="crates/ plugins/ templates/"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

die() {
    echo "changelog-check: $*" >&2
    exit 1
}

base="${1:-origin/main}"
git rev-parse --verify --quiet "$base^{commit}" > /dev/null ||
    die "cannot resolve base ref '$base'"

# The merge base, not the tip: a branch is answerable for what it changed, not
# for what landed on main while it was open.
merge_base="$(git merge-base "$base" HEAD)" ||
    die "no merge base between '$base' and HEAD"

changed="$(git diff --name-only "$merge_base" HEAD)"

# A RELEASE FOLD ADDS NO FRAGMENT, BY CONSTRUCTION. The release branch runs
# scripts/changelog-fold.sh, which deletes every fragment, and it touches the
# watched paths (the version sweep rewrites every plugin manifest), so without
# this rule the one branch that ships the entries is the one the gate fails. It
# is recognised by what the fold leaves behind, all read from commits rather than
# the working tree: at least one fragment deleted against the merge base,
# changelog.d/ at HEAD holding nothing but README.md, and CHANGELOG.md at HEAD
# carrying a `## v<version>` heading, for the version Cargo.toml declares at
# HEAD, that the merge base does not have. A branch that merely forgot its entry
# deletes no fragment and opens no section, so it still fails below.
release_version=""
deleted_count=0
is_release_fold() {
    local deleted remaining heading_re
    deleted="$(git diff --name-only --diff-filter=D "$merge_base" HEAD |
        grep '^changelog\.d/.*\.md$' |
        grep -v '^changelog\.d/README\.md$' || true)"
    [ -n "$deleted" ] || return 1
    deleted_count="$(printf '%s\n' "$deleted" | wc -l | tr -d ' ')"

    remaining="$(git ls-tree --name-only HEAD changelog.d/ |
        grep -v '^changelog\.d/README\.md$' || true)"
    [ -z "$remaining" ] || return 1

    release_version="$(git show HEAD:Cargo.toml | awk '
        /^\[/ { in_pkg = ($0 == "[workspace.package]"); next }
        in_pkg && /^version[[:space:]]*=/ {
            sub(/^version[[:space:]]*=[[:space:]]*"/, ""); sub(/".*$/, ""); print
            in_pkg = 0
        }')"
    [ -n "$release_version" ] || return 1

    # Read into variables rather than piped into `grep -q`: under pipefail a
    # grep that stops at its first match kills `git show` with SIGPIPE, and the
    # pipeline then reports failure for a heading that is there.
    local head_log base_log
    heading_re="^## v$(printf '%s' "$release_version" | sed 's/\./\\./g')([[:space:]]|\$)"
    head_log="$(git show HEAD:CHANGELOG.md)" || return 1
    base_log="$(git show "$merge_base:CHANGELOG.md" 2> /dev/null || true)"
    printf '%s\n' "$head_log" | grep -E "$heading_re" > /dev/null || return 1
    if printf '%s\n' "$base_log" | grep -E "$heading_re" > /dev/null; then
        return 1
    fi
    return 0
}

needs_entry=0
for prefix in $watched_paths; do
    if printf '%s\n' "$changed" | grep -q "^$prefix"; then
        needs_entry=1
        break
    fi
done

if [ "$needs_entry" -eq 0 ]; then
    echo "changelog-check: nothing under ${watched_paths// /, } changed; no fragment required."
    exit 0
fi

# Fragments added or modified on this branch. README.md is the directory's own
# documentation, never an entry.
fragments="$(git diff --name-only --diff-filter=AM "$merge_base" HEAD |
    grep '^changelog\.d/.*\.md$' |
    grep -v '^changelog\.d/README\.md$' || true)"

if [ -z "$fragments" ] && is_release_fold; then
    echo "changelog-check: release fold for v$release_version: $deleted_count fragment(s) folded into CHANGELOG.md, changelog.d/ emptied; no fragment of its own required."
    exit 0
fi

if [ -z "$fragments" ]; then
    branch="$(git rev-parse --abbrev-ref HEAD)"
    suggested="changelog.d/$(printf '%s' "$branch" | tr '/' '-').md"
    cat >&2 <<EOF
changelog-check: this branch changes code and adds no changelog fragment.

Changed under ${watched_paths// /, }:
$(printf '%s\n' "$changed" | grep -E "^($(printf '%s' "$watched_paths" | tr -s ' ' '|' | sed 's/|$//'))" | sed 's/^/  /')

Write the entry in its own file, so two branches never conflict over
CHANGELOG.md:

  $suggested

Name a root cause, not a symptom, at the length the entry deserves; it is
copied into CHANGELOG.md verbatim at release time. If this change genuinely
needs no entry, make that the fragment's whole body:

  echo none > $suggested

See changelog.d/README.md.
EOF
    exit 1
fi

# A fragment that exists but says nothing folds to nothing and silently loses the
# entry, so an empty one is a failure rather than a pass.
for fragment in $fragments; do
    [ -f "$fragment" ] || continue
    if [ -z "$(tr -d '[:space:]' < "$fragment")" ]; then
        die "$fragment is empty. Write the entry, or the single word 'none'."
    fi
done

echo "changelog-check: fragment(s) present:"
printf '%s\n' "$fragments" | sed 's/^/  /'
