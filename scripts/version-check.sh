#!/usr/bin/env bash
#
# Fail if the declared version has moved more than one release past the newest
# release tag.
#
# `[workspace.package] version` in Cargo.toml is the next release, not a count of
# changes. It is raised once, by the first change after a release, and never
# again before it is tagged. Without a check it was raised twice on an unreleased
# `main`: the plugin API section of docs/design/Versioning.md says a new host
# function or manifest key is a MINOR change, and two pull requests that each
# added one each read that as "bump the minor", so `main` declared a version two
# minors past the release it was heading for. See docs/design/Versioning.md.
#
#   scripts/version-check.sh               # the version against the tag
#   scripts/version-check.sh <base-ref>    # and against what it merges into
#
# Passes when the version equals the newest `v*` tag reachable from HEAD (nothing
# has been raised since the release) or is exactly one release step above it:
# the next patch, the next minor with patch 0, or the next major with minor and
# patch 0. Anything else fails, naming both.
#
# Given a base (CI passes the pull request's base SHA), it also fails a branch
# that raises the version above the base's when the base has already left the
# tag: `main` then heads for a release, and a second raise before that release
# is tagged is what this script exists to stop, even when the second value is
# itself one step above the tag (0.105.0 on main, 1.0.0 on the branch, v0.104.0
# tagged). Moving the version down toward the tag is not a raise and passes.
#
# The declared version is a release number, so a prerelease suffix fails: under
# SemVer `0.104.0-rc.1` sorts below the `v0.104.0` it would otherwise compare
# equal to. A suffix on the tag is ignored; this project does not tag
# prereleases.
#
# Needs the tags: in CI the checkout fetches them.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

die() {
    echo "version-check: $*" >&2
    exit 1
}

base="${1:-}"

# [workspace.package] version from a Cargo.toml read on stdin. Reads to the end
# rather than exiting at the match, so a `git show` feeding it never gets SIGPIPE.
workspace_version() {
    awk '
        /^\[/ { in_pkg = ($0 == "[workspace.package]"); next }
        in_pkg && /^version[[:space:]]*=/ {
            sub(/^version[[:space:]]*=[[:space:]]*"/, ""); sub(/".*$/, ""); print
            in_pkg = 0
        }'
}

version="$(workspace_version < Cargo.toml)"
[ -n "$version" ] || die "cannot read [workspace.package] version from Cargo.toml"
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' ||
    die "Cargo.toml version '$version' is not a release number MAJOR.MINOR.PATCH; the declared version is the next release, never a prerelease"

tag="$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2> /dev/null)" ||
    die "no v* tag is reachable from HEAD; fetch tags (git fetch --tags) and rerun"

# MAJOR.MINOR.PATCH, digits only, or nothing.
triple() {
    printf '%s\n' "$1" | sed -nE 's/^v?([0-9]+)\.([0-9]+)\.([0-9]+)([-+].*)?$/\1 \2 \3/p'
}

read -r vmaj vmin vpat <<< "$(triple "$version")"
read -r tmaj tmin tpat <<< "$(triple "$tag")"
[ -n "${vpat:-}" ] || die "Cargo.toml version '$version' is not MAJOR.MINOR.PATCH"
[ -n "${tpat:-}" ] || die "tag '$tag' is not vMAJOR.MINOR.PATCH"

# Sortable form of a MAJOR.MINOR.PATCH triple.
key() { printf '%010d%010d%010d' "$1" "$2" "$3"; }

if [ -n "$base" ]; then
    git rev-parse --verify --quiet "$base^{commit}" > /dev/null ||
        die "cannot resolve base ref '$base'"
    base_version="$(git show "$base:Cargo.toml" | workspace_version)"
    read -r bmaj bmin bpat <<< "$(triple "$base_version")"
    [ -n "${bpat:-}" ] || die "base $base declares '$base_version', not MAJOR.MINOR.PATCH"
    if [ "$bmaj.$bmin.$bpat" != "$tmaj.$tmin.$tpat" ] &&
        [[ "$(key "$vmaj" "$vmin" "$vpat")" > "$(key "$bmaj" "$bmin" "$bpat")" ]]; then
        cat >&2 << EOF
version-check: Cargo.toml declares $version, but the base already declares
$base_version, raised past the newest release tag $tag. The version is raised
once per release: main already heads for $base_version, and this change belongs
to that release. Leave the version at $base_version. If it needs a MAJOR while
main heads for a MINOR, that is a decision for the maintainer, not a second
bump. See docs/design/Versioning.md.
EOF
        exit 1
    fi
    echo "version-check: the base declares $base_version; this branch does not raise it a second time."
fi

if [ "$vmaj.$vmin.$vpat" = "$tmaj.$tmin.$tpat" ]; then
    echo "version-check: Cargo.toml declares $version, equal to the newest release tag $tag; not raised since the release."
    exit 0
fi

allowed="$tmaj.$tmin.$((tpat + 1)) $tmaj.$((tmin + 1)).0 $((tmaj + 1)).0.0"
for next in $allowed; do
    if [ "$vmaj.$vmin.$vpat" = "$next" ]; then
        echo "version-check: Cargo.toml declares $version, one release step above the newest release tag $tag."
        exit 0
    fi
done

cat >&2 << EOF
version-check: Cargo.toml declares $version, but the newest release tag
reachable from HEAD is $tag. The version may equal $tag, or be one release
step above it: one of ${allowed// /, }.

The version is the next release and is raised once per release, by the first
change after the tag. If main already heads for a release above $tag, this
change belongs to that release: leave the version where it is. If it needs a
MAJOR while main heads for a MINOR, that is a decision for the maintainer, not
a second bump. See docs/design/Versioning.md.
EOF
exit 1
