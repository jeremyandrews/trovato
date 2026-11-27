#!/usr/bin/env bash
#
# Run the tests one change touches: the fast inner loop for an agent iterating
# on a branch, so it does not pay a full `cargo test --all` for a one-line fix.
# Everything it selects runs in one cargo invocation (one build, one link per
# binary), the same shape as a CI shard (scripts/ci-test-shard.sh).
#
#   ./scripts/test-changed.sh                  # from the change set alone
#   ./scripts/test-changed.sh front_page_test  # plus named targets
#
# Selection, in this order:
#   1. The lib/bin unit tests (`--lib --bins`) of every workspace package that
#      has a changed file — anywhere in the package, source or test.
#   2. Every integration test target whose own file changed.
#   3. Every integration test target named on the command line.
#   4. For a changed file under crates/*/src/, every integration target whose
#      file name contains that file's stem, or whose own source names the
#      file's module path (`services/redirect.rs` selects a target matching
#      `redirect` or mentioning `services::redirect`).
#   5. Everything (`cargo test --all`), when crates/kernel/tests/common/,
#      migrations/, or a Cargo.toml or Cargo.lock anywhere changed: too much
#      depends on those for a partial run to mean anything.
#
# "Changed" is `git diff --name-only` against the merge base with
# origin/main (which already includes staged and unstaged edits to tracked
# files, since the diff's other endpoint is the working tree) plus untracked
# files. Rule 3 does not need a change at all — naming a target always
# selects it.
#
# Always ends with a reminder that this is the inner loop, not the full gate:
# run the whole suite (CONTRIBUTING.md's "The local gate is stronger than CI")
# before opening the pull request.
#
# Bash 3.2 safe (macOS), like scripts/ci-test-shard.sh. Needs cargo, jq, git.
set -euo pipefail

# TEST_CHANGED_ROOT_OVERRIDE, like TEST_CHANGED_OVERRIDE below, is for
# scripts/ci-scripts-test.sh: a made-up workspace directory instead of this
# checkout, so the selection logic is testable against fixture files with no
# real build and no real git history.
ROOT="${TEST_CHANGED_ROOT_OVERRIDE:-$(cd "$(dirname "$0")/.." && pwd)}"
cd "$ROOT"

# ---- the change set ---------------------------------------------------------
#
# TEST_CHANGED_OVERRIDE, set (even to empty), replaces git entirely with its
# own newline-separated file list: what scripts/ci-scripts-test.sh drives this
# script with, against made-up change sets, with no git repo and no build.
if [ "${TEST_CHANGED_OVERRIDE+set}" = "set" ]; then
  CHANGED="$TEST_CHANGED_OVERRIDE"
else
  MERGE_BASE=$(git merge-base HEAD origin/main)
  CHANGED=$(
    {
      git diff --name-only "$MERGE_BASE"
      git ls-files --others --exclude-standard
    } | sort -u
  )
fi

if [ -n "$CHANGED" ]; then
  echo "Changed files:"
  printf '%s\n' "$CHANGED" | sed 's/^/  /'
else
  echo "No changed files."
fi

# ---- workspace metadata ------------------------------------------------------
#
# One `cargo metadata` call for both the package table (for rule 1 and for
# resolving which package a target belongs to) and the target table (every
# test target, its package and its source file).
META=$(cargo metadata --no-deps --format-version 1)

# "<package> <dir-relative-to-ROOT-or-.>", one per workspace member.
PKG_TABLE=$(printf '%s' "$META" | jq -r '.packages[] | "\(.name) \(.manifest_path)"' | while read -r name manifest; do
  dir=$(dirname "$manifest")
  case "$dir" in
    "$ROOT") echo "$name ." ;;
    "$ROOT"/*) echo "$name ${dir#"$ROOT"/}" ;;
  esac
done)

# "<package>\t<target>\t<src-path-relative-to-ROOT>", one per integration test
# target (kind "test") in the workspace.
TARGET_TABLE=$(printf '%s' "$META" | jq -r '
  .packages[] | .name as $p | .targets[]
  | select(.kind[] == "test")
  | "\($p)\t\(.name)\t\(.src_path)"
' | while IFS=$'\t' read -r pkg tgt src; do
  case "$src" in
    "$ROOT"/*) printf '%s\t%s\t%s\n' "$pkg" "$tgt" "${src#"$ROOT"/}" ;;
  esac
done)

# The package owning `$1` (a path relative to ROOT): the workspace member
# whose directory is the longest prefix of it. Empty if none is (a file
# outside every package, e.g. a top-level script).
owning_package() {
  local f="$1" best="" bestlen=-1 name dir prefix len
  while read -r name dir; do
    if [ "$dir" = "." ]; then
      prefix=""
    else
      prefix="$dir/"
    fi
    case "$f" in
      "$prefix"*)
        len=${#prefix}
        if [ "$len" -gt "$bestlen" ]; then
          best="$name"
          bestlen=$len
        fi
        ;;
    esac
  done <<PKGS
$PKG_TABLE
PKGS
  echo "$best"
}

# ---- rule 5: everything ------------------------------------------------------

RUN_ALL=0
RUN_ALL_WHY=""
while read -r f; do
  [ -z "$f" ] && continue
  case "$f" in
    crates/kernel/tests/common/*)
      RUN_ALL=1
      RUN_ALL_WHY="$RUN_ALL_WHY  $f touched the shared test fixture\n"
      ;;
    migrations/*)
      RUN_ALL=1
      RUN_ALL_WHY="$RUN_ALL_WHY  $f is a migration\n"
      ;;
  esac
  case "$(basename "$f")" in
    Cargo.toml | Cargo.lock)
      RUN_ALL=1
      RUN_ALL_WHY="$RUN_ALL_WHY  $f changes what the workspace resolves or builds\n"
      ;;
  esac
done <<CHANGED_FILES
$CHANGED
CHANGED_FILES

if [ "$RUN_ALL" -eq 1 ]; then
  echo "Running everything:"
  printf '%b' "$RUN_ALL_WHY"
  echo "Inner loop only: run the full suite before you finish (see CONTRIBUTING.md)."
  exec env RUST_TEST_THREADS=4 cargo test --all --no-fail-fast
fi

# ---- rule 1: packages with a changed file -----------------------------------

CHANGED_PKGS=""
while read -r f; do
  [ -z "$f" ] && continue
  pkg=$(owning_package "$f")
  [ -n "$pkg" ] && CHANGED_PKGS="$CHANGED_PKGS
$pkg (changed file $f)"
done <<CHANGED_FILES
$CHANGED
CHANGED_FILES
CHANGED_PKGS=$(printf '%s\n' "$CHANGED_PKGS" | grep -v '^$' || true)

# ---- rule 2: a target's own file changed ------------------------------------

OWN_FILE_TARGETS=""
while IFS=$'\t' read -r pkg tgt src; do
  [ -z "$pkg" ] && continue
  if printf '%s\n' "$CHANGED" | grep -qxF "$src"; then
    OWN_FILE_TARGETS="$OWN_FILE_TARGETS
$pkg	$tgt	own file $src changed"
  fi
done <<TARGETS
$TARGET_TABLE
TARGETS

# ---- rule 3: named on the command line --------------------------------------

NAMED_TARGETS=""
for want in "$@"; do
  found=0
  while IFS=$'\t' read -r pkg tgt src; do
    [ -z "$pkg" ] && continue
    if [ "$tgt" = "$want" ]; then
      NAMED_TARGETS="$NAMED_TARGETS
$pkg	$tgt	named on the command line"
      found=1
    fi
  done <<TARGETS
$TARGET_TABLE
TARGETS
  if [ "$found" -eq 0 ]; then
    echo "::error::no integration test target named '$want'" >&2
    exit 1
  fi
done

# ---- rule 4: stem or module path match under crates/*/src/ ------------------

STEM_TARGETS=""
while read -r f; do
  case "$f" in
    crates/*/src/*.rs) ;;
    *) continue ;;
  esac
  pkg=$(owning_package "$f")
  [ -z "$pkg" ] && continue
  stem=$(basename "$f" .rs)
  # crates/<crate>/src/<subpath>.rs -> <subpath> with / as ::; mod.rs names
  # its directory, not a "mod" module.
  subpath=${f#crates/*/src/}
  subpath=${subpath%.rs}
  if [ "$(basename "$subpath")" = "mod" ]; then
    subpath=$(dirname "$subpath")
    [ "$subpath" = "." ] && subpath=""
  fi
  modpath=$(printf '%s' "$subpath" | sed 's#/#::#g')
  while IFS=$'\t' read -r tpkg tgt src; do
    [ -z "$tpkg" ] && continue
    [ "$tpkg" != "$pkg" ] && continue
    matched=0
    case "$tgt" in
      *"$stem"*) matched=1 ;;
    esac
    if [ "$matched" -eq 0 ] && [ -n "$modpath" ] && grep -qF -- "$modpath" "$ROOT/$src" 2>/dev/null; then
      matched=1
    fi
    if [ "$matched" -eq 1 ]; then
      STEM_TARGETS="$STEM_TARGETS
$tpkg	$tgt	matches changed $f"
    fi
  done <<TARGETS
$TARGET_TABLE
TARGETS
done <<CHANGED_FILES
$CHANGED
CHANGED_FILES

# ---- compose -----------------------------------------------------------------

ALL_TARGETS=$(printf '%s\n%s\n%s\n' "$OWN_FILE_TARGETS" "$NAMED_TARGETS" "$STEM_TARGETS" | grep -v '^$' || true)
# Unique by package+target, keeping the first reason it was picked for.
SELECTED_TARGETS=$(printf '%s\n' "$ALL_TARGETS" | awk -F'\t' '!seen[$1 "\t" $2]++')

TARGET_PKGS=$(printf '%s\n' "$SELECTED_TARGETS" | awk -F'\t' 'NF {print $1}' | sort -u)
ALL_PKGS=$(printf '%s\n%s\n' "$(printf '%s\n' "$CHANGED_PKGS" | awk '{print $1}')" "$TARGET_PKGS" | grep -v '^$' | sort -u || true)

if [ -z "$ALL_PKGS" ] && [ -z "$SELECTED_TARGETS" ]; then
  echo "Nothing in the change set selects a package or a target; nothing to run."
  exit 0
fi

echo "Selected:"
printf '%s\n' "$CHANGED_PKGS" | grep -v '^$' | sed 's/^/  unit tests (--lib --bins): /' || true
printf '%s\n' "$SELECTED_TARGETS" | awk -F'\t' 'NF {printf "  integration target %s (%s): %s\n", $2, $1, $3}'

ARGS=(--no-fail-fast)
for p in $ALL_PKGS; do
  ARGS+=(-p "$p")
done
ARGS+=(--lib --bins)
if [ -n "$SELECTED_TARGETS" ]; then
  while IFS=$'\t' read -r _ tgt _; do
    [ -z "$tgt" ] && continue
    ARGS+=(--test "$tgt")
  done <<SEL
$SELECTED_TARGETS
SEL
fi

echo "Inner loop only: run the full suite before you finish (see CONTRIBUTING.md)."
exec env RUST_TEST_THREADS=4 cargo test "${ARGS[@]}"
