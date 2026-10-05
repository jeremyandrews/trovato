The outbound HTTP fence now refuses an internal address however it is written.
Every IPv6 literal used to pass it: the policy took `Url::host_str()` and tried
to parse the result as an IP address, and for an IPv6 literal that string keeps
its brackets, so `[::1]` never parsed and the whole address check was skipped as
though the host were an ordinary name with no matching internal suffix. All
three layers of the fence shared the one function, so a plugin could reach a
loopback sidecar with `http://[::1]:8080/`, a redirect could land on one
mid-chain, and the AI provider's base-URL validator, which had copied the same
pattern, accepted one from an administrator form.

Hosts are classified from the parsed `Url::host()` now, as a domain, an IPv4
address or an IPv6 address, so the brackets never enter the question and the
hostname rules apply only to actual hostnames.

Behind that, the two copies of the range list have become one shared classifier
in `net_policy`. They had drifted: the plugin fence blocked carrier-grade NAT
and the broadcast address, the AI provider blocked `0.0.0.0/8`, and neither
blocked the other's, so which internal addresses were reachable depended on
which caller you were. The shared list is the union of both plus the reserved
space neither had: `192.0.0.0/24`, `198.18.0.0/15`, multicast in both families,
and `240.0.0.0/4`.

It also unwraps an IPv6 address that carries an IPv4 one inside it and
classifies the address within. `::ffff:127.0.0.1` and `::ffff:169.254.169.254`
read as ordinary global unicast to a classifier that only looks at the IPv6
bits, which made them a way to reach loopback and the cloud metadata endpoint
both as literals and through a hostname whose AAAA record answered with one.
The IPv4-compatible form, the NAT64 well-known prefix and 6to4 are unwrapped the
same way.

Nothing a legitimate caller does changes. A public IPv6 literal still works, the
AI provider still accepts the `.internal` and `.local` names an on-premises
model server is reached by, and a denial is still the same
`ERR_HTTP_INVALID_URL` the fence has always returned.
