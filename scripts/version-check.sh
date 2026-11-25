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
#   scripts/version-check.sh
#
# Passes when the version equals the newest `v*` tag reachable from HEAD (nothing
# has been raised since the release) or is exactly one release step above it:
# the next patch, the next minor with patch 0, or the next major with minor and
# patch 0. Anything else fails, naming both. A prerelease suffix on either side
# is not part of the comparison; this project does not tag prereleases.
#
# Needs the tags: in CI the checkout fetches them.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

die() {
    echo "version-check: $*" >&2
    exit 1
}

version="$(awk '
    /^\[/ { in_pkg = ($0 == "[workspace.package]"); next }
    in_pkg && /^version[[:space:]]*=/ {
        sub(/^version[[:space:]]*=[[:space:]]*"/, ""); sub(/".*$/, ""); print; exit
    }' Cargo.toml)"
[ -n "$version" ] || die "cannot read [workspace.package] version from Cargo.toml"

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
