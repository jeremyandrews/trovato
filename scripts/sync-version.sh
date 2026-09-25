#!/usr/bin/env bash
#
# shellcheck disable=SC2016
# The generated bodies below are Markdown held in single quotes, so their
# backticks are code spans and fenced blocks, never command substitution. The
# values are interpolated by printf as %s arguments.
#
# Write the project version into every file that repeats it.
#
# The version is authored in exactly one place, `[workspace.package] version` in
# the root Cargo.toml. Most of the tree derives it without help: every crate
# inherits it, KERNEL_API_VERSION is parsed from it at compile time, and the
# publish workflow reads it out of Cargo.toml. What is left is the plugin
# manifests, which the loader reads at run time and which therefore hold literal
# strings, and the documentation that states the current release.
#
# This script writes both. `cargo test` fails by name if any of them disagree,
# so a bump is one edit to Cargo.toml, one run of this, and a CHANGELOG entry.
#
#   scripts/sync-version.sh
#
# It takes no arguments: there is nothing to tell it that Cargo.toml does not
# already say. Run it from anywhere; the repository root is resolved from this
# file's location.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

die() {
    echo "sync-version: $*" >&2
    exit 1
}

# `[workspace.package] version`, the one authored copy.
version="$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"$/\1/p' \
    "$root/Cargo.toml" | head -n 1)"
[ -n "$version" ] || die "could not read [workspace.package] version from Cargo.toml"

api="${version%.*}"
major="${version%%.*}"
minor="${api#*.}"
next_minor=$((minor + 1))
prev_minor=$((minor - 1))

echo "Trovato ${version}, plugin API ${api} = (${major}, ${minor})"

# Replace a file with the result of a sed program, reporting only real changes.
# A temporary file because `sed -i` differs between BSD and GNU.
rewrite() {
    local path="$1"
    shift
    local tmp
    tmp="$(mktemp)"
    sed "$@" "$path" > "$tmp"
    if cmp -s "$path" "$tmp"; then
        rm -f "$tmp"
    else
        cat "$tmp" > "$path"
        echo "  rewrote ${path#"$root"/}"
    fi
    rm -f "$tmp"
}

# Replace the lines between a `version:begin` marker and its matching end with
# the contents of a file, keeping the markers. The body arrives as a file
# because BSD awk will not take a newline in `-v`.
write_block() {
    local path="$1" body_file="$2"
    grep -q '<!-- version:begin -->' "$path" ||
        die "$path has no version:begin marker"
    local tmp
    tmp="$(mktemp)"
    awk -v body_file="$body_file" '
        /<!-- version:begin -->/ {
            print
            while ((getline line < body_file) > 0) print line
            close(body_file)
            skip = 1
            next
        }
        /<!-- version:end -->/ { skip = 0 }
        !skip                  { print }
    ' "$path" > "$tmp"
    if cmp -s "$path" "$tmp"; then
        rm -f "$tmp"
    else
        cat "$tmp" > "$path"
        echo "  rewrote ${path#"$root"/}"
    fi
    rm -f "$tmp"
}

# Replace only the Nth marker block in a file, for the documents that carry
# several. Everything outside the chosen block is copied through untouched.
write_nth_block() {
    local path="$1" index="$2" body_file="$3"
    local tmp
    tmp="$(mktemp)"
    awk -v body_file="$body_file" -v want="$index" '
        /<!-- version:begin -->/ {
            seen++
            print
            if (seen == want) {
                while ((getline line < body_file) > 0) print line
                close(body_file)
                skip = 1
            }
            next
        }
        /<!-- version:end -->/ { skip = 0 }
        !skip                  { print }
    ' "$path" > "$tmp"
    if cmp -s "$path" "$tmp"; then
        rm -f "$tmp"
    else
        cat "$tmp" > "$path"
        echo "  rewrote ${path#"$root"/} (block ${index})"
    fi
    rm -f "$tmp"
}

body="$(mktemp)"
trap 'rm -f "$body"' EXIT

# --- The plugin manifests -------------------------------------------------
#
# The loader reads these at run time, so they hold literal strings rather than
# anything derived. There are dozens of them and they are the reason this script
# exists; the test that walks them is what makes the literals safe.
while IFS= read -r manifest; do
    rewrite "$manifest" \
        -e "s/^version = \".*\"\$/version = \"${version}\"/" \
        -e "s/^api_version = \".*\"\$/api_version = \"${api}\"/"
done < <(find "$root/plugins" -name '*.info.toml' | sort)

# --- docs/design/Versioning.md -------------------------------------------
printf 'At %s the plugin API is `(%s, %s)` and every manifest declares\n`api_version = "%s"`. At 1.0.0 the API becomes `(1, 0)` and every manifest\ndeclares `"1.0"`. There is no case where one of these numbers moves and the\nothers do not.\n' \
    "$version" "$major" "$minor" "$api" > "$body"
write_nth_block "$root/docs/design/Versioning.md" 1 "$body"

printf '```toml\n[workspace.package]\nversion = "%s"\n```\n' "$version" > "$body"
write_nth_block "$root/docs/design/Versioning.md" 2 "$body"

printf '`%s` gives `(%s, %s)`.\n' "$version" "$major" "$minor" > "$body"
write_nth_block "$root/docs/design/Versioning.md" 3 "$body"

printf 'With a kernel at API %s:\n\n| Plugin API | Compatible? | Reason |\n|------------|-------------|--------|\n| %s | Yes | Exact match |\n| %s.42 | Yes | Same major, older minor: the kernel provides everything it asks for |\n| %s.%s | No | Needs host functions this kernel may not export |\n' \
    "$api" "$api" "$major" "$major" "$next_minor" > "$body"
write_nth_block "$root/docs/design/Versioning.md" 4 "$body"

printf '```toml\nname = "my_plugin"\ndescription = "Example plugin"\nversion = "%s"\napi_version = "%s"\n```\n' \
    "$version" "$api" > "$body"
write_nth_block "$root/docs/design/Versioning.md" 5 "$body"

# --- docs/design/version-map.md ------------------------------------------
printf 'Current version: **%s**, plugin API **(%s, %s)**.\n' \
    "$version" "$major" "$minor" > "$body"
write_block "$root/docs/design/version-map.md" "$body"

# --- The prose that names the current release ----------------------------
printf 'Trovato is at %s, working toward 1.0. The plugin contract is frozen; the version number is pre-1.0 because the CMS around it is not finished yet.\n' \
    "$version" > "$body"
write_block "$root/README.md" "$body"

printf 'Trovato is at %s and the work between here and 1.0 is happening in public.\n' \
    "$version" > "$body"
write_block "$root/CONTRIBUTING.md" "$body"

printf 'Trovato is at %s.\n' "$version" > "$body"
write_block "$root/ROADMAP.md" "$body"

printf 'What is outstanding in %s.\n' "$version" > "$body"
write_block "$root/KNOWN-ISSUES.md" "$body"

# --- The supported-versions table ----------------------------------------
#
# Security fixes land on the current minor only, so this table moves every
# release and a stale one misstates the policy rather than merely reading old.
printf '| Version | Supported |\n|---|---|\n| %s.%s.x (current) | Yes |\n| %s.%s.x and earlier | No: upgrade to the current release |\n' \
    "$major" "$minor" "$major" "$prev_minor" > "$body"
write_block "$root/SECURITY.md" "$body"

# --- The worked `git tag` example ----------------------------------------
#
# The one stale number a reader is most likely to paste into a terminal.
printf '```sh\ngit tag -a v%s -m "Trovato %s"\ngit push origin v%s\n```\n' \
    "$version" "$version" "$version" > "$body"
write_block "$root/docs/RELEASING.md" "$body"

# --- The issue template --------------------------------------------------
#
# YAML, so there is nowhere to hang an HTML comment: the line is matched and
# rewritten in place, and the test checks it like the rest.
rewrite "$root/.github/ISSUE_TEMPLATE/config.yml" \
    -e "s/What is already known to be outstanding in [0-9][0-9.]*, before you file it\./What is already known to be outstanding in ${version}, before you file it./"

echo "done"
