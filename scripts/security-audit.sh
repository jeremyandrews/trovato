#!/usr/bin/env bash
#
# Run `cargo audit` against a pinned copy of the RustSec advisory database.
#
# WHY A PIN. `cargo audit` fetches the advisory database before it scans, so a
# bare run answers a question that changes under it: not "does this branch have a
# vulnerable dependency" but "does it have one as of whenever the job happened to
# start". Every open branch therefore went red the morning an advisory landed
# against a crate none of them had touched, and the only cure was to rebase and
# burn another full CI cycle. A red audit stopped meaning anything about the
# branch it was attached to.
#
# So a pull request audits against a commit of the database that is recorded in
# this repository, with fetching disabled. A branch then fails only for an
# advisory that already existed when the pin was set, which is a real finding
# about its own dependencies, and two runs of the same commit give the same
# answer. `main` is audited live, daily, by .github/workflows/security-audit.yml,
# which is the job that is allowed to go red for reasons outside any branch
# because no merge is waiting on it.
#
#   scripts/security-audit.sh              # pinned; what pull requests run
#   scripts/security-audit.sh --live       # live fetch; what the daily job runs
#   scripts/security-audit.sh --db PATH    # reuse an advisory-db checkout on disk
#
# THE PIN IS A FLOOR, AND IT ROTS. A pin that never moves freezes the floor at
# the day it was set, and every advisory published afterwards becomes invisible to
# every pull request. That is the one real risk this whole arrangement introduces,
# so it is gated rather than trusted: a pin older than $max_pin_age_days fails
# this script, in CI and locally, with instructions. The pin cannot quietly rot;
# it can only be moved deliberately, in a reviewed pull request, on a date.
#
# A real advisory is fixed by upgrading the dependency, never by moving the pin
# past it. Moving the pin forward past a finding hides it from pull requests and
# leaves it live on `main`, where the daily job will keep reporting it and no
# branch will ever show it again.
set -euo pipefail

# The oldest pin this script will audit against. Chosen to match the "Quarterly
# Review Process" in docs/security-audit.md: the pin bump and the suppression
# review are the same visit to the same question, so they come due together.
max_pin_age_days=90

advisory_db_url="https://github.com/rustsec/advisory-db.git"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pin_file="$root/.cargo/advisory-db-pin"

die() {
    echo "security-audit: $*" >&2
    exit 1
}

# Is this directory itself the root of a checkout? However it was made: a clone
# leaves a .git directory, a `git worktree add` leaves a .git file, both are fine.
#
# The top level has to match, not merely resolve. A bare `rev-parse --git-dir`
# walks up until it finds a repository, so asked about a directory nested inside
# this one it answers yes and hands back Trovato's own .git — which looks like a
# valid advisory-db checkout, fetches the pin from the wrong remote, and reports
# the pinned commit as missing from the advisory database.
is_git_checkout() {
    local top
    top="$(git -C "$1" rev-parse --show-toplevel 2> /dev/null)" || return 1
    [ "$top" = "$(cd "$1" && pwd)" ]
}

usage() {
    cat <<'USAGE'
usage: scripts/security-audit.sh [--live] [--db PATH]

  --live      audit against a freshly fetched advisory database, ignoring the
              pin. What the daily scheduled job runs against main.
  --db PATH   an advisory-db checkout already on disk, which must be at the
              pinned commit. Without this, the script maintains its own under
              advisory-db/ at the repository root, which is gitignored.
USAGE
}

mode="pinned"
db=""
while [ $# -gt 0 ]; do
    case "$1" in
        --live) mode="live" ;;
        --db)
            shift
            [ $# -gt 0 ] || die "--db needs a path"
            db="$1"
            ;;
        --db=*) db="${1#--db=}" ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
    shift
done

# Absolute, before the cd below: CI passes a workspace-relative path.
if [ -n "$db" ]; then
    db="$(cd "$db" 2> /dev/null && pwd)" || die "no such directory: $db"
fi

cd "$root"

if [ "$mode" = "live" ]; then
    [ -z "$db" ] || die "--db and --live are contradictory"
    echo "security-audit: live fetch, no pin. Advisories published since the pin"
    echo "security-audit: was set are in scope, and a finding here is real."
    exec cargo audit
fi

[ -f "$pin_file" ] || die "no pin at ${pin_file#"$root"/}"

# The one 40-hex line in the file. Everything else in it is the comment saying
# when it was set and why.
pin="$(grep -oE '^[0-9a-f]{40}$' "$pin_file" | head -n 1 || true)"
[ -n "$pin" ] ||
    die "${pin_file#"$root"/} holds no 40-character commit id"

if [ -z "$db" ]; then
    # Gitignored, and outside target/ so that a Rust build cache cleaning target/
    # cannot take the checkout with it.
    db="$root/advisory-db"
    mkdir -p "$db"
    is_git_checkout "$db" || git init -q "$db"
    git -C "$db" remote get-url origin > /dev/null 2>&1 ||
        git -C "$db" remote add origin "$advisory_db_url"
    if [ "$(git -C "$db" rev-parse --verify --quiet HEAD || true)" != "$pin" ]; then
        echo "security-audit: fetching advisory-db at $pin"
        # Asking for one commit by id is cheap but not guaranteed: a server with
        # uploadpack.allowReachableSHA1InWant disabled refuses it outright with
        # "upload-pack: not our ref". So the cheap request is an optimization and a
        # full fetch is the fallback that always works. Not a retry of the same
        # request: a different one.
        if ! git -C "$db" fetch --depth 1 --quiet origin "$pin" 2> /dev/null; then
            echo "security-audit: fetch by commit id refused; fetching all refs"
            git -C "$db" fetch --quiet --tags origin ||
                die "could not fetch $advisory_db_url"
        fi
        git -C "$db" checkout -q --detach "$pin" ||
            die "$pin is not in $advisory_db_url"
    fi
fi

is_git_checkout "$db" ||
    die "$db is not a git checkout of the advisory database"

# Whoever produced the checkout, it has to be the pin. A CI step that checked out
# the wrong ref would otherwise audit against a database nobody recorded.
head_sha="$(git -C "$db" rev-parse HEAD)"
[ "$head_sha" = "$pin" ] ||
    die "$db is at $head_sha, not the pinned $pin"

pin_epoch="$(git -C "$db" log -1 --format=%ct)"
pin_date="$(git -C "$db" log -1 --format=%cs)"
age_days=$((($(date +%s) - pin_epoch) / 86400))

echo "security-audit: advisory-db pinned at $pin ($pin_date, $age_days days old)"

if [ "$age_days" -gt "$max_pin_age_days" ]; then
    cat >&2 <<EOF
security-audit: the pinned advisory database is $age_days days old, past the
security-audit: $max_pin_age_days day limit. Every advisory published since
security-audit: $pin_date is invisible to pull requests until it moves.

Bump it in its own pull request, so raising the floor is a reviewed act with a
date on it:

  git ls-remote $advisory_db_url HEAD
  \$EDITOR ${pin_file#"$root"/}      # the commit id, and the date you set it
  ./scripts/security-audit.sh        # see what the newer floor reports

If that turns up a real advisory, fix it by upgrading the dependency, in its own
pull request, before or alongside the bump. Never move the pin past a finding.
EOF
    exit 1
fi

# --no-fetch is the point: the database does not move under the run, so the same
# commit audited twice gives the same answer.
exec cargo audit --no-fetch --db "$db"
