#!/usr/bin/env bash
#
# Tests for the two release gates: scripts/version-check.sh (the version is raised
# once per release) and scripts/changelog-check.sh (a change carries a fragment,
# and a release fold is recognised as one). Each case builds a throwaway git
# repository holding a copy of both scripts, a Cargo.toml, a CHANGELOG.md and a
# changelog.d/, so it needs no network, no database and no build; it runs in a
# second or two.
#
#   ./scripts/release-scripts-test.sh
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
FAILS=0

pass() { echo "ok   $1"; }
fail() { echo "FAIL $1"; FAILS=$((FAILS + 1)); }

# expect <name> <0|1> <command...>: the command must exit 0 (0) or not (1).
expect() {
    local name=$1 want=$2 got
    shift 2
    if "$@" > "$WORK/out" 2>&1; then got=0; else got=1; fi
    if [ "$got" = "$want" ]; then pass "$name"; else
        fail "$name"
        sed 's/^/     /' "$WORK/out"
    fi
}

git_q() { git -c user.name=test -c user.email=test@example.invalid "$@" > /dev/null; }

set_version() {
    cat > Cargo.toml << EOF
[workspace]
members = []

[workspace.package]
# the one version
version = "$1"
edition = "2024"
EOF
}

# A repository with one release, v2.4.0, on `main`, and two queued fragments.
# Leaves the shell in it.
fixture() {
    local dir="$WORK/$1"
    rm -rf "$dir"
    mkdir -p "$dir/scripts" "$dir/changelog.d" "$dir/crates/k"
    cd "$dir"
    cp "$ROOT/scripts/version-check.sh" "$ROOT/scripts/changelog-check.sh" scripts/
    git_q init -q -b main
    set_version 2.4.0
    printf '# Changelog\n\n## Unreleased\n\n## v2.4.0 — 2026-09-24\n\nOld.\n' > CHANGELOG.md
    echo "The queue." > changelog.d/README.md
    echo "fn a() {}" > crates/k/lib.rs
    git_q add -A
    git_q commit -q -m "Trovato 2.4.0"
    git_q tag -a v2.4.0 -m "Trovato 2.4.0"
    echo "An entry." > changelog.d/one.md
    echo "Another entry." > changelog.d/two.md
    echo "fn b() {}" >> crates/k/lib.rs
    git_q add -A
    git_q commit -q -m "two changes with their fragments"
}

# Commit whatever is in the tree on a new branch off the current HEAD.
on_branch() { git_q checkout -q -b "$1"; }
commit_all() { git_q add -A && git_q commit -q -m "$1"; }

# ── version-check.sh against the tag ─────────────────────────────────────────

fixture vtag
on_branch next
for case in "2.4.0 0" "2.4.1 0" "2.5.0 0" "3.0.0 0" "2.6.0 1" "2.5.1 1" \
    "2.3.0 1" "2.4.0-rc.1 1" "2.5.0-nightly.3 1" "2.5 1"; do
    read -r v want <<< "$case"
    set_version "$v"
    expect "version: $v over v2.4.0" "$want" ./scripts/version-check.sh
done
git_q checkout -q -- Cargo.toml

# ── version-check.sh against the base it merges into ─────────────────────────

fixture vbase
# main has already raised the version once, to 2.5.0.
set_version 2.5.0
commit_all "raise to 2.5.0"
on_branch second-raise
set_version 3.0.0
commit_all "a second raise"
expect "version: 3.0.0 over a base already at 2.5.0 fails" 1 ./scripts/version-check.sh main
expect "version: 3.0.0 is one step above the tag, so without a base it passes" 0 ./scripts/version-check.sh
git_q checkout -q main
on_branch keeps-it
echo "fn c() {}" >> crates/k/lib.rs
commit_all "a change that leaves the version alone"
expect "version: 2.5.0 over a base at 2.5.0 passes" 0 ./scripts/version-check.sh main
git_q checkout -q main
set_version 2.6.0
commit_all "main raised twice, as before the rule"
on_branch back-down
set_version 2.5.0
commit_all "set it back"
expect "version: lowering 2.6.0 to 2.5.0 passes" 0 ./scripts/version-check.sh main

fixture vfirst
on_branch first-raise
set_version 2.5.0
commit_all "the first change after the release"
expect "version: the first raise over a base at the tag passes" 0 ./scripts/version-check.sh main

# ── changelog-check.sh ───────────────────────────────────────────────────────

fixture cfrag
on_branch with-fragment
echo "fn d() {}" >> crates/k/lib.rs
echo "Says what changed." > changelog.d/with-fragment.md
commit_all "change with its fragment"
expect "changelog: a code change with a fragment passes" 0 ./scripts/changelog-check.sh main

fixture cnone
on_branch forgot
echo "fn d() {}" >> crates/k/lib.rs
commit_all "change without a fragment"
expect "changelog: a code change without a fragment fails" 1 ./scripts/changelog-check.sh main

# The fold as scripts/changelog-fold.sh leaves it: fragments gone, README kept,
# a new section for the declared version.
fold() {
    set_version 2.5.0
    printf '# Changelog\n\n## v2.5.0 — 2026-11-25\n\nAn entry.\n\nAnother entry.\n\n## v2.4.0 — 2026-09-24\n\nOld.\n' > CHANGELOG.md
    rm changelog.d/one.md changelog.d/two.md
    echo "fn e() {}" >> crates/k/lib.rs
}

fixture cfold
on_branch release
fold
commit_all "Trovato 2.5.0"
expect "changelog: a release fold passes" 0 ./scripts/changelog-check.sh main
if grep -q 'release fold for v2.5.0' "$WORK/out"; then
    pass "changelog: a release fold names its rule"
else
    fail "changelog: a release fold names its rule"
fi

fixture cnoreadme
on_branch release
fold
rm -r changelog.d
commit_all "Trovato 2.5.0 without changelog.d"
expect "changelog: a fold that deletes changelog.d/ fails" 1 ./scripts/changelog-check.sh main

fixture cleft
on_branch release
fold
echo "Left behind." > changelog.d/three.md
git_q add -A
git_q commit -q -m "fold that leaves a fragment"
git_q rm -q changelog.d/three.md
echo "Not folded." > changelog.d/zzz.txt
commit_all "fold that leaves a file"
expect "changelog: a fold that leaves anything but the README fails" 1 ./scripts/changelog-check.sh main

fixture cnoheading
on_branch release
fold
printf '# Changelog\n\n## Unreleased\n\n## v2.4.0 — 2026-09-24\n\nOld.\n' > CHANGELOG.md
commit_all "fragments deleted, no section opened"
expect "changelog: fragments deleted with no new section fails" 1 ./scripts/changelog-check.sh main

fixture coldheading
on_branch release
fold
set_version 2.4.0
commit_all "a heading for a version Cargo.toml does not declare"
expect "changelog: a section for a version Cargo.toml does not declare fails" 1 ./scripts/changelog-check.sh main

echo
if [ "$FAILS" -eq 0 ]; then
    echo "all checks passed"
else
    echo "$FAILS check(s) failed"
    exit 1
fi
