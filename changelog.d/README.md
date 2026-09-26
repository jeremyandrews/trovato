# changelog.d

One file per pull request, holding that pull request's `CHANGELOG.md` entry.

Two branches that both append to `## Unreleased` conflict on the same lines of
the same file every time, and the conflict is pure bookkeeping: nobody disagrees
about anything. So the entry waits here, in a file named for the branch, and two
branches touch two different files instead.

This directory is a queue, not a record. `CHANGELOG.md` is the record.
`scripts/changelog-fold.sh` moves entries from here to there at release time and
empties the queue, so a file sitting here is an entry that has not shipped yet,
and a released tree has nothing here but this README.

## Writing one

Name the file for your branch, with any `/` replaced by `-`:

```sh
echo "changelog.d/$(git rev-parse --abbrev-ref HEAD | tr / -).md"
```

The body of the file is the entry. It is copied into `CHANGELOG.md` verbatim, so
write it the way it should read there: prose, naming a root cause rather than a
symptom, at whatever length the change deserves. Look at the entries already in
`CHANGELOG.md` for the register; the house style is paragraphs, not bullets, and
it has used no `###` category headings since v0.100.0.

A real example, `changelog.d/changelog-fragments.md` as it was written for the
pull request that added this directory:

```markdown
Changelog entries are written as fragments now, one file per pull request under
`changelog.d/`, and folded into `CHANGELOG.md` at release time by
`scripts/changelog-fold.sh`.

Two branches open at once both appended to `## Unreleased`, at the same place in
the same file, so every second one merged with a conflict that carried no
disagreement: each side wanted its own paragraph and neither touched the other's.
The entry now waits in a file named for the branch, which no other branch has a
reason to write to.
```

## If the change needs no entry

Make that the fragment's whole body:

```sh
echo none > changelog.d/my-branch.md
```

`none` is the deliberate way to say "no entry". The fold deletes the fragment
with the rest and writes nothing. This is the only escape hatch; there is no
label for it, because a file travels with the branch, shows up in the diff where
a reviewer is already looking, and works the same from a fork.

An **empty** fragment is not the same thing and is an error: it reads as an entry
that was started and forgotten, and would silently vanish at release time.

## Categories

A fragment may begin with a single header line and a blank line:

```markdown
Category: Fixed

The entry.
```

which groups it under a `### Fixed` subheading. `CHANGELOG.md` has not used
category headings since v0.100.0, so normally a fragment carries none. The
affordance is there for a release that wants the groupings the older sections
had; the order they are emitted in is the list at the top of
`scripts/changelog-fold.sh`.

## The gate

`scripts/changelog-check.sh` fails a pull request that changes anything under
`crates/`, `plugins/` or `templates/` and adds no fragment. CI runs it as the
`Changelog Fragment` job. Run it yourself first:

```sh
./scripts/changelog-check.sh
```
