A pull request's security audit depends only on that pull request now. The
`Security Audit` job ran bare `cargo audit`, which fetches the advisory database
before it scans, so what it actually asked was not "does this branch have a
vulnerable dependency" but "does it have one as of whenever this job happened to
start". Every open branch went red the morning an advisory landed against a crate
none of them had touched, and the only cure was to rebase and burn another full
CI cycle. A red audit had stopped saying anything about the branch it was attached
to.

The job now audits against one commit of the advisory database, recorded in
`.cargo/advisory-db-pin`, with fetching disabled. A branch fails only for an
advisory that already existed when the pin was set, which is a real finding about
its own dependencies, and the same commit audited twice gives the same answer.

The live check did not go away and was not weakened. `Live Security Audit` fetches
live against `main` every morning and on demand, fails loudly, and opens an issue
naming the advisory and the crate, or comments on the one already open rather than
filing a duplicate; a clean run closes it, so no open issue means the last run was
clean rather than that nobody looked. That job is allowed to go red for a reason no
branch caused, because no merge is waiting on it.

A pin that never moves would freeze the floor at the day it was set and hide every
later advisory from every pull request, which is the one real risk here, so it is
gated rather than trusted: `scripts/security-audit.sh` fails, in CI and locally, on
a pin more than 90 days old. The pin moves in its own reviewed pull request, and a
real advisory is answered by upgrading the dependency, never by moving the pin past
it.
