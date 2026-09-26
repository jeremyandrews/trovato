Changelog entries are written as fragments now, one file per pull request under
`changelog.d/`, and folded into `CHANGELOG.md` at release time by
`scripts/changelog-fold.sh`.

Two branches open at once both appended to `## Unreleased`, at the same place in
the same file, so every second one merged with a conflict that carried no
disagreement: each side wanted its own paragraph and neither touched the other's.
The entry now waits in a file named for the branch, which no other branch has a
reason to write to. `scripts/changelog-check.sh` is the gate, run in CI as
`Changelog Fragment`: a pull request that changes `crates/`, `plugins/` or
`templates/` and adds no fragment fails, and a change that needs no entry says so
in a fragment whose body is the single word `none` rather than by omitting one.

The fold appends fragments to a section in a deterministic order and deletes the
ones it folded, so `changelog.d/` is empty on a released tree and every file in it
is an entry that has not shipped. The entries already sitting under
`## Unreleased` were left exactly where they are; nothing was retrofitted.
