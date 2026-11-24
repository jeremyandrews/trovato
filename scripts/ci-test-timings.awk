# Read `cargo test` output on stdin, pass it through unchanged, and write down
# where the time went. Used by scripts/ci-test-shard.sh for the integration
# targets and by the Doc tests step of .github/workflows/ci.yml; tested by
# scripts/ci-scripts-test.sh.
#
#   -v targets=FILE   one `<package>::<target> <seconds>` line per target, from
#                     libtest's `finished in` (the partition's weights are these)
#   -v tests=FILE     one `<package>::<target> <test name> <seconds>` line per
#                     test, from libtest's `--report-time` (`ok <1.234s>`); this
#                     is what scripts/ci-slow-tests.sh holds to the budget
#   -v doc=1          doc tests: each crate's `Doc-tests <crate>` block is one
#                     entry, `doc::<crate>` (rustdoc rejects `--report-time` on
#                     a stable toolchain, so a doc test is timed with its crate)
#
# For integration targets, ENVIRON["MINE"] holds the shard's `<package>
# <target>` lines. A binary is matched to its target by file name, which cargo
# derives from the target name and not from the source path, so a `[[test]]`
# with a custom `path` is still recorded under its own name. A binary that is
# not on the list, or a listed target that never reports, fails the run: a
# target missing from the timings would be a target missing from the budget.
BEGIN {
  n = split(ENVIRON["MINE"], rows, "\n")
  for (i = 1; i <= n; i++) {
    if (split(rows[i], f, " ") < 2) continue
    stem = f[2]; gsub(/-/, "_", stem)
    name[stem] = f[1] "::" f[2]
    want[f[1] "::" f[2]] = 1
  }
  bad = 0
}
{ print; fflush() }
doc && /^ *Doc-tests / { cur = "doc::" $2; next }
!doc && /^ *Running / {
  cur = ""
  if (match($0, /\([^)]*\)$/)) {
    bin = substr($0, RSTART + 1, RLENGTH - 2)
    sub(/.*\//, "", bin)
    sub(/-[0-9a-f]+$/, "", bin)
    if (bin in name) cur = name[bin]
    else { printf "::error::test binary %s is not one of this shard's targets\n", bin > "/dev/stderr"; bad = 1 }
  }
  next
}
cur != "" && /^test .* <[0-9.]+s>$/ {
  t = $0
  sub(/^test /, "", t)
  sub(/ \.\.\. .*$/, "", t)
  match($0, /<[0-9.]+s>$/)
  secs = substr($0, RSTART + 1, RLENGTH - 3)
  if (tests != "") print cur " " t " " secs >> tests
  next
}
/^test result:/ && cur != "" {
  if (match($0, /finished in [0-9.]+s/)) {
    secs = substr($0, RSTART + 12, RLENGTH - 13)
    if (targets != "") print cur " " secs >> targets
    seen[cur] = 1
  }
  # A crate's doc tests can report twice (the merged ones, then any that
  # could not be merged); each is an entry. A test binary reports once.
  if (!doc) cur = ""
}
END {
  for (k in want)
    if (!(k in seen)) { printf "::error::target %s ran but reported no time\n", k > "/dev/stderr"; bad = 1 }
  exit bad
}
