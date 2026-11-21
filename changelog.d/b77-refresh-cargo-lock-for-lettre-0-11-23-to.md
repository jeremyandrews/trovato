Outgoing mail goes through lettre 0.11.24, up from 0.11.23, which fixes six
advisories published on 2026-11-17 against every earlier 0.11 release.
RUSTSEC-2026-0560, scored 4.8, let a man in the middle inject SMTP replies ahead
of the STARTTLS handshake so that they were read as if they came over the
encrypted channel. RUSTSEC-2026-0561 left a bare LF or CR in a message body
without dot-stuffing, so a body could end the DATA section early and inject SMTP
commands of its own. RUSTSEC-2026-0563 sent a message addressed to a quoted
local part to a different address than the one written. The other three are
denial of service: RUSTSEC-2026-0562, a panic on an `EHLO` reply line of only
white space; RUSTSEC-2026-0564, a panic on a mailbox display name containing CR
or LF; and RUSTSEC-2026-0565, unbounded buffering of an SMTP reply, which let a
malicious server exhaust memory.

Every site that sends mail through SMTP is affected, whether the mail comes from
the kernel (registration, password reset, notifications) or from a plugin
through the `mail` host interface. The upgrade is a lock file change only: the
workspace already asked for lettre `0.11`, nothing else in `Cargo.lock` moved,
and no Trovato code or configuration changes.
