#!/usr/bin/env bash
#
# Fold the fragments in changelog.d/ into CHANGELOG.md.
#
# WHY THIS EXISTS. Two pull requests that both append an entry to the same
# `## Unreleased` block conflict on the same lines of the same file, every time,
# and the conflict is pure bookkeeping: nobody disagrees about anything. So an
# entry is written to its own file instead, `changelog.d/<branch-name>.md`, and
# this script is what collects them. Two branches then touch two different files
# and cannot collide.
#
# The fragments are the queue; CHANGELOG.md is the ledger. This script moves work
# from one to the other and empties the queue in the same run, so `changelog.d/`
# is empty on a released tree and every file in it is an entry that has not
# shipped yet.
#
#   scripts/changelog-fold.sh                    # fold into ## Unreleased
#   scripts/changelog-fold.sh --version 0.105.0  # fold into ## v0.105.0 — <today>
#   scripts/changelog-fold.sh --dry-run          # print, change nothing
#
# Running it on an empty changelog.d/ is a no-op that exits 0, so a release
# script can call it unconditionally.
#
# FRAGMENT FORMAT. The body of the file is the entry, in the wording and at the
# length it would have had in CHANGELOG.md; it is copied through verbatim. An
# optional first line
#
#   Category: Fixed
#
# followed by a blank line groups the entry under a `### Fixed` subheading. A
# fragment whose whole body is the single word `none` records that the change
# needs no entry: it is deleted with the rest and contributes nothing.
#
# ON CATEGORIES. CHANGELOG.md has used none since v0.100.0 — its entries are
# prose paragraphs under the version heading, and that is the house style. The
# `Category:` header is therefore an affordance for a release that wants the
# `###` groupings the pre-v0.100.0 sections used, not something a fragment is
# expected to carry. Uncategorized entries are emitted first, in filename order,
# which is what the current style wants; categorized ones follow, grouped in the
# order named below. Filename order is the tie-break everywhere, so the fold is
# reproducible from the fragments alone.
set -euo pipefail

# The order `### ` groups are emitted in when fragments name categories. There is
# no order to inherit from CHANGELOG.md (see above), so this is the conventional
# Keep a Changelog sequence. A category not named here sorts after these,
# alphabetically. Edit the list rather than renaming anyone's fragment.
category_order="Added Changed Deprecated Removed Fixed Security"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
frag_dir="$root/changelog.d"
changelog="$root/CHANGELOG.md"

die() {
    echo "changelog-fold: $*" >&2
    exit 1
}

usage() {
    cat <<'USAGE'
usage: scripts/changelog-fold.sh [--version X.Y.Z] [--dry-run]

  --version X.Y.Z  fold into the `## vX.Y.Z` section, creating it above the
                   newest version section if it does not exist yet. Without
                   this, fragments fold into `## Unreleased`.
  --dry-run, -n    print the target section and the exact text that would be
                   inserted; touch nothing and delete nothing.
USAGE
}

version=""
dry_run=0
while [ $# -gt 0 ]; do
    case "$1" in
        --version)
            shift
            [ $# -gt 0 ] || die "--version needs a value"
            version="$1"
            ;;
        --version=*) version="${1#--version=}" ;;
        --dry-run | -n) dry_run=1 ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
    shift
done

[ -f "$changelog" ] || die "no CHANGELOG.md at $changelog"

tmp="$(mktemp -d)"
# shellcheck disable=SC2064
# $tmp is wanted at trap-setting time, not at trap-firing time.
trap "rm -rf '$tmp'" EXIT

# Collect the fragments. Recursive, so a fragment that ended up nested under a
# branch name containing a slash is still found; README.md is the directory's own
# documentation and is never an entry.
: > "$tmp/paths"
if [ -d "$frag_dir" ]; then
    find "$frag_dir" -type f -name '*.md' \
        ! -name 'README.md' -print |
        LC_ALL=C sort > "$tmp/paths"
fi

if [ ! -s "$tmp/paths" ]; then
    echo "changelog-fold: changelog.d/ holds no fragments; nothing to fold."
    exit 0
fi

# Trim leading and trailing blank lines, keeping the internal ones: an entry is
# several paragraphs and the blank lines between them are load bearing.
trim_blank() {
    awk '
        NF { while (pending-- > 0) print ""; pending = 0; started = 1; print; next }
        started { pending++ }
    '
}

# Split each fragment into its category (possibly empty) and its body, and index
# them by the order they will be considered in.
i=0
while IFS= read -r path; do
    i=$((i + 1))
    awk '
        NR == 1 && /^[Cc]ategory:[[:space:]]*/ {
            sub(/^[Cc]ategory:[[:space:]]*/, "")
            sub(/[[:space:]]+$/, "")
            print
        }
    ' "$path" > "$tmp/$i.cat"
    awk 'NR == 1 && /^[Cc]ategory:[[:space:]]*/ { next } { print }' "$path" |
        trim_blank > "$tmp/$i.body"
    printf '%s\n' "$path" > "$tmp/$i.path"
    printf '%s\t%s\n' "$i" "$path" >> "$tmp/index"
done < "$tmp/paths"
count="$i"

# `none` as the whole body is the escape hatch for a change that needs no entry.
is_none() {
    [ "$(tr '[:upper:]' '[:lower:]' < "$1" | tr -d '[:space:]')" = "none" ]
}

# Every category actually used, known ones first in the order above, then the
# rest alphabetically.
: > "$tmp/used"
i=0
while [ "$i" -lt "$count" ]; do
    i=$((i + 1))
    if [ -s "$tmp/$i.cat" ]; then cat "$tmp/$i.cat" >> "$tmp/used"; fi
done
: > "$tmp/known"
for known in $category_order; do
    if LC_ALL=C grep -qxF "$known" "$tmp/used" 2>/dev/null; then
        echo "$known" >> "$tmp/known"
    fi
done
LC_ALL=C sort -u "$tmp/used" | while IFS= read -r used; do
    [ -n "$used" ] || continue
    LC_ALL=C grep -qxF "$used" "$tmp/known" 2>/dev/null || echo "$used"
done > "$tmp/unknown"
cat "$tmp/known" "$tmp/unknown" > "$tmp/categories"

# Build the block to insert.
insert="$tmp/insert"
: > "$insert"
wrote=0
folded="$tmp/folded"
: > "$folded"

emit_body() {
    [ "$wrote" -eq 0 ] || printf '\n' >> "$insert"
    cat "$1" >> "$insert"
    wrote=1
}

# Uncategorized first, in filename order: the current house style.
i=0
while [ "$i" -lt "$count" ]; do
    i=$((i + 1))
    [ -s "$tmp/$i.cat" ] && continue
    echo "$i" >> "$folded"
    is_none "$tmp/$i.body" && continue
    [ -s "$tmp/$i.body" ] || die "$(cat "$tmp/$i.path") is empty; write the entry, or the single word 'none'"
    emit_body "$tmp/$i.body"
done

# Then each category, in the order resolved above, filename order within.
while IFS= read -r category; do
    [ -n "$category" ] || continue
    heading_written=0
    i=0
    while [ "$i" -lt "$count" ]; do
        i=$((i + 1))
        [ -s "$tmp/$i.cat" ] || continue
        [ "$(cat "$tmp/$i.cat")" = "$category" ] || continue
        echo "$i" >> "$folded"
        is_none "$tmp/$i.body" && continue
        [ -s "$tmp/$i.body" ] || die "$(cat "$tmp/$i.path") is empty; write the entry, or the single word 'none'"
        if [ "$heading_written" -eq 0 ]; then
            [ "$wrote" -eq 0 ] || printf '\n' >> "$insert"
            printf '### %s\n' "$category" >> "$insert"
            wrote=1
            heading_written=1
        fi
        emit_body "$tmp/$i.body"
    done
done < "$tmp/categories"

# Resolve the target section, creating a version section if asked for one that is
# not open yet.
work="$tmp/changelog"
cp "$changelog" "$work"

if [ -n "$version" ]; then
    heading_re="^## v?$(printf '%s' "$version" | sed 's/\./\\./g')([[:space:]]|\$)"
    created=0
    if ! awk -v re="$heading_re" '$0 ~ re { found = 1; exit } END { exit found ? 0 : 1 }' "$work"; then
        awk -v heading="## v$version — $(date +%F)" '
            !done && /^## v/ { print heading; print ""; done = 1 }
            { print }
            END { if (!done) { print ""; print heading } }
        ' "$work" > "$work.new"
        mv "$work.new" "$work"
        created=1
    fi
    target_re="$heading_re"
    target_label="## v$version"
    [ "$created" -eq 0 ] || target_label="$target_label (section opened by this run)"
else
    target_re='^## Unreleased[[:space:]]*$'
    target_label="## Unreleased"
fi

heading_line="$(awk -v re="$target_re" '$0 ~ re { print NR; exit }' "$work")"
[ -n "$heading_line" ] || die "no $target_label heading in CHANGELOG.md"

# Insert after the last non-blank line of the target section, so entries already
# there keep their wording, their order and their position.
insert_after="$(awk -v start="$heading_line" '
    NR <= start { last = NR; next }
    /^## / { exit }
    NF { last = NR }
    END { print last }
' "$work")"

if [ "$dry_run" -eq 1 ]; then
    echo "changelog-fold: would fold $(wc -l < "$tmp/paths" | tr -d ' ') fragment(s) into $target_label"
    echo "changelog-fold: insertion point is CHANGELOG.md line $insert_after"
    echo
    while IFS="$(printf '\t')" read -r n p; do
        c="$(cat "$tmp/$n.cat")"
        if is_none "$tmp/$n.body"; then
            state="none (no entry, fragment removed)"
        elif [ -n "$c" ]; then
            state="category: $c"
        else
            state="uncategorized"
        fi
        echo "  ${p#"$root"/} — $state"
    done < "$tmp/index"
    echo
    if [ "$wrote" -eq 0 ]; then
        echo "--- nothing would be inserted ---"
    else
        echo "--- text that would be inserted ---"
        cat "$insert"
        echo "--- end ---"
    fi
    echo
    echo "changelog-fold: dry run; CHANGELOG.md and changelog.d/ untouched."
    exit 0
fi

if [ "$wrote" -eq 1 ]; then
    awk -v at="$insert_after" -v block="$insert" '
        { print }
        NR == at {
            print ""
            while ((getline line < block) > 0) print line
            close(block)
        }
    ' "$work" > "$work.new"
    mv "$work.new" "$work"
fi

cp "$work" "$changelog"

removed=0
while IFS="$(printf '\t')" read -r n p; do
    if LC_ALL=C grep -qxF "$n" "$folded"; then
        rm -f "$p"
        removed=$((removed + 1))
    fi
done < "$tmp/index"

if [ "$wrote" -eq 1 ]; then
    echo "changelog-fold: folded into $target_label, removed $removed fragment(s)."
else
    echo "changelog-fold: no entries to add; removed $removed fragment(s)."
fi
echo "changelog-fold: review the diff before committing."
