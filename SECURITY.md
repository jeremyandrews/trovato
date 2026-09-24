# Security Policy

## Reporting a vulnerability

**Report privately. Do not open a public issue for a suspected vulnerability.**

The channel is GitHub's private vulnerability reporting:

> https://github.com/jeremyandrews/trovato/security/advisories/new

That form is private to the reporter and the maintainers, it creates a draft
advisory that becomes the published one, and it lets a fix be prepared and
tested before anything is visible. Use it in preference to anything else.

<!-- MAINTAINER: fill in or delete. No address is published until Jeremy sets
     one, because a wrong or unmonitored security address is worse than none:
     it silently swallows reports that would otherwise have gone to the form
     above. -->
If you cannot use GitHub, email `[SECURITY CONTACT ADDRESS NOT YET SET]`.

### What to include

A report is actionable when it says what an attacker can do, not only what
looks wrong. Where you can:

- The version or commit you tested, and whether you ran it from source or from
  the `ghcr.io/jeremyandrews/trovato` image.
- Which role the attacker starts as: anonymous visitor, authenticated reader,
  content editor, plugin author, or delegated administrator. Trovato's threat
  model is written around those five, and the boundary that matters is the one
  the finding crosses.
- Steps to reproduce against a local install, ideally a request sequence or a
  short script. `INSTALL.md` gets a server up.
- What the impact is, and what you did not verify.

You do not need a proof-of-concept exploit, and you should not develop one
against a system you do not own.

## What to expect

| Stage | Target |
|---|---|
| Acknowledgement that a human has read it | 3 working days |
| First assessment: accepted, needs more information, or not a vulnerability | 10 working days |
| Fix released for an accepted high or critical finding | 30 days from the assessment |
| Fix released for an accepted low or moderate finding | the next scheduled release |

These are targets, not a contract. Trovato is maintained by one person with
substantial AI assistance, and the honest version of that is: you will get a
considered answer rather than a fast one. If a target slips you will be told it
has slipped and why, rather than left waiting.

A report that turns out not to be a vulnerability still gets an answer saying
why, so the reasoning can be argued with.

## Coordinated disclosure

The maintainers ask for 90 days from acknowledgement before public disclosure,
or until a fix ships, whichever is sooner. If a finding is being exploited in
the wild, say so in the report and that window does not apply.

Reporters are credited in the advisory and the changelog by whatever name they
give, or not at all if they prefer. There is no bug bounty.

## Supported versions

Trovato is pre-1.0 and releases roughly weekly. Security fixes land on the
current minor only, and the upgrade path is forward:

| Version | Supported |
|---|---|
| 0.104.x (current) | Yes |
| 0.103.x and earlier | No: upgrade to the current release |

There are no backports to earlier minors before 1.0. `UPGRADING.md` documents
what each step needs. This policy is deliberately narrow while the plugin
contract is still moving, and it will be revisited at 1.0.

A release that carries a security fix has a title beginning `[security]`, for
example `[security] 0.99.2 — session fixation in the recovery flow`. That
prefix is load-bearing: the kernel's update check reads the latest release
title (`is_security_title` in `crates/kernel/src/update_status.rs`) and the
admin dashboard renders a security release as an alarm rather than a notice.
See `CONTRIBUTING.md`.

## Scope

**In scope**, and the surfaces a report is most likely to concern:

- The WASM plugin sandbox: anything that lets a plugin read or write outside
  its declared capabilities, its `db_tables` allowlist, or its resource limits.
- The HTTP host interface and the SSRF fence on outbound requests.
- Authentication: sessions, WebAuthn passkeys, API tokens, CSRF.
- The permission model, including privilege escalation between the roles in the
  threat model.
- The config import path.
- Stored or reflected XSS through the theme engine, the page builder, or
  content fields.
- The AI assistant's tool execution path.

**Out of scope**, or handled elsewhere:

- Dependency advisories. Those follow the policy in
  [`docs/security-audit.md`](docs/security-audit.md), which also records why
  each currently suppressed advisory is suppressed. If you believe a
  suppression is wrong, that is a legitimate report and it belongs in the
  private channel, not a public issue.
- Findings that require an administrator acting against their own site. A
  delegated administrator escalating past what they were granted **is** in
  scope; an administrator with every permission doing something destructive is
  not.
- Missing hardening headers with no demonstrated impact, output of automated
  scanners pasted without analysis, and denial of service by unbounded traffic
  volume.
- Anything in `KNOWN-ISSUES.md` that is already recorded there with a stated
  status. Saying the recorded status is wrong is in scope.

## Prior art

- [`docs/security-audit.md`](docs/security-audit.md): dependency advisory
  policy and the suppression rules.
- [`KNOWN-ISSUES.md`](KNOWN-ISSUES.md): current suppressions and their
  justifications.
- [`docs/security/REVIEW-BRIEF.md`](docs/security/REVIEW-BRIEF.md): the
  security surfaces, their invariants, and the threat model, written for an
  external reviewer.
