An item's edit lock now lapses 3 minutes after the holder's last heartbeat,
not 15. PR 242 had the edit forms take the lock on open and heartbeat it every
minute, but left the lease at 900 seconds, so a closed tab, a navigated-away
tab or a crashed browser — none of which can send a heartbeat again — blocked
every other editor from the item for up to 15 minutes with nobody at the
keyboard. Three missed heartbeats is enough margin for ordinary jitter and
still a short wait.

A well-behaved exit no longer has to wait out even that: the edit form now
beacons a release, via `navigator.sendBeacon` on `pagehide`, to a new
`/api/v1/lock/release`, so closing the tab or navigating away frees the item
immediately. A beacon cannot set a custom header, so this route is the one
place the CSRF token travels in a form-urlencoded `_token` field rather than
`X-CSRF-Token`; the server accepts either. A browser process killed outright
still gets no chance to send it, which is what the shorter lease is for. The
form also sends an extra heartbeat when a backgrounded tab becomes visible
again, since a hidden tab's timers can be throttled or frozen well past a
minute.
