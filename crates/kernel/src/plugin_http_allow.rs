//! The operator override that lets one named plugin reach one named private or
//! loopback destination over HTTP (`PLUGIN_HTTP_PRIVATE_ALLOW`), beside the
//! classifier in [`crate::net_policy`] that refuses every such destination by
//! default.
//!
//! [`crate::net_policy::is_disallowed_ip`] stays unchanged and is still the
//! classifier a request falls back to whenever this module grants nothing.
//! This module is consulted first, but only to ask one question: did the
//! operator name exactly this plugin, this destination and this port? A
//! match here never *lowers* the default fence for anyone else: a different
//! plugin, a different port, or a redirect to anywhere else still meets the
//! same refusal every other request does. See
//! `docs/plugin-development.md#reaching-private-addresses-from-plugins` for the
//! operator-facing syntax and the Varnish example that motivated it.
//!
//! An entry's destination is an IPv4/IPv6 literal, a CIDR range, or a
//! hostname. Whichever form it takes, link-local (including the cloud-metadata
//! address), multicast, broadcast and unspecified addresses can never be named
//! by any entry: [`PluginHttpAllow::parse`] refuses a literal destination
//! outright at startup, and a CIDR whose own network address is one of them,
//! but a broader or misaligned CIDR that merely *contains* one (`169.0.0.0/8`
//! contains `169.254.169.254`) starts fine and is instead refused per request
//! by the same [`is_never_allowable`] check [`Entry::matches`] runs against
//! the request's actual address. A hostname destination has no address to
//! check until DNS resolves it, so it is checked the same way, per
//! resolution, by the kernel's resolver in [`crate::host::http`].

use std::net::{IpAddr, Ipv4Addr};

use url::Url;

use crate::client_addr::IpNet;

/// A destination named in one [`Entry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Destination {
    /// An exact IPv4 or IPv6 literal.
    Ip(IpAddr),
    /// A CIDR range.
    Cidr(IpNet),
    /// A hostname, already lowercased.
    Host(String),
}

impl std::fmt::Display for Destination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Destination::Ip(ip) => write!(f, "{ip}"),
            Destination::Cidr(net) => write!(f, "{net}"),
            Destination::Host(host) => write!(f, "{host}"),
        }
    }
}

/// One `<plugin>@<destination>:<port>` entry from `PLUGIN_HTTP_PRIVATE_ALLOW`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) plugin: String,
    pub(crate) destination: Destination,
    pub(crate) port: u16,
}

impl Entry {
    /// Parse one entry, as written between commas in the raw setting value.
    fn parse(raw: &str) -> Result<Self, String> {
        let (plugin, rest) = raw.split_once('@').ok_or_else(|| {
            format!("`{raw}` is missing `@` (expected <plugin>@<destination>:<port>)")
        })?;
        let plugin = plugin.trim();
        if !crate::routes::helpers::is_valid_machine_name(plugin) {
            return Err(format!(
                "`{raw}` names an invalid plugin machine name `{plugin}`"
            ));
        }

        let (dest_str, port) =
            parse_destination_and_port(rest).map_err(|e| format!("`{raw}`: {e}"))?;
        let destination = parse_destination(&dest_str).map_err(|e| format!("`{raw}`: {e}"))?;

        if let Some(ip) = destination_sample_ip(&destination)
            && is_never_allowable(ip)
        {
            return Err(format!(
                "`{raw}` names {dest_str}, which is link-local, multicast, \
                 broadcast or unspecified space and can never be allowed"
            ));
        }

        Ok(Self {
            plugin: plugin.to_string(),
            destination,
            port,
        })
    }

    /// Does this entry grant `plugin`'s request to `url`?
    ///
    /// The match is exact: the calling plugin's name, an `http`/`https`
    /// scheme, the entry's port (defaulting `url`'s own port the same way
    /// [`Url::port_or_known_default`] does), and the destination: literal
    /// equality, CIDR containment, or, for a hostname entry, case-insensitive
    /// name equality. Addresses are compared through
    /// [`IpAddr::to_canonical`], so an IPv4 address embedded in an IPv6 form
    /// (`::ffff:a.b.c.d`) matches the same entry its plain IPv4 form would.
    ///
    /// For a literal or CIDR destination, a match additionally requires that
    /// the request's own address is not itself [`is_never_allowable`]. A CIDR
    /// is validated at parse time only against its network address
    /// ([`destination_sample_ip`]), so a broad or misaligned range (an entry
    /// naming `169.0.0.0/8`, which is not itself link-local) could otherwise
    /// still contain a never-allowable address (`169.254.169.254`) that no
    /// check had ever run against. This second check is what actually closes
    /// that gap, for every request, regardless of how the CIDR was drawn.
    pub(crate) fn matches(&self, plugin: &str, url: &Url) -> bool {
        if self.plugin != plugin {
            return false;
        }
        if !matches!(url.scheme(), "http" | "https") {
            return false;
        }
        let Some(port) = url.port_or_known_default() else {
            return false;
        };
        if port != self.port {
            return false;
        }
        let Some(host) = url.host() else {
            return false;
        };
        match (&self.destination, host) {
            (Destination::Host(name), url::Host::Domain(d)) => d.eq_ignore_ascii_case(name),
            (Destination::Ip(ip), url::Host::Ipv4(v4)) => {
                let addr = IpAddr::V4(v4);
                addr.to_canonical() == ip.to_canonical() && !is_never_allowable(addr)
            }
            (Destination::Ip(ip), url::Host::Ipv6(v6)) => {
                let addr = IpAddr::V6(v6);
                addr.to_canonical() == ip.to_canonical() && !is_never_allowable(addr)
            }
            (Destination::Cidr(net), url::Host::Ipv4(v4)) => {
                let addr = IpAddr::V4(v4);
                net.contains(addr) && !is_never_allowable(addr)
            }
            (Destination::Cidr(net), url::Host::Ipv6(v6)) => {
                let addr = IpAddr::V6(v6);
                net.contains(addr) && !is_never_allowable(addr)
            }
            _ => false,
        }
    }
}

/// A representative address to run the never-allowable check against at parse
/// time: the literal itself, or a CIDR's network address. `None` for a
/// hostname entry: its address is not known until DNS resolves it, which is
/// checked separately, per resolution, by [`is_never_allowable`] itself.
///
/// This is an upfront, loud check for the common case (an entry whose literal
/// or whose CIDR's own network address is never-allowable); it is not the only
/// check. [`Entry::matches`] runs [`is_never_allowable`] again, against the
/// request's actual address, which is what catches a CIDR broad or misaligned
/// enough to contain a never-allowable address without its network address
/// being one itself.
fn destination_sample_ip(destination: &Destination) -> Option<IpAddr> {
    match destination {
        Destination::Ip(ip) => Some(*ip),
        Destination::Cidr(net) => Some(net.network()),
        Destination::Host(_) => None,
    }
}

/// Is this address one no entry may ever name, and no hostname entry may ever
/// resolve to: link-local (including `169.254.169.254`, the cloud-metadata
/// address), multicast, broadcast, or unspecified? Checked at parse time for a
/// literal or CIDR destination, at match time against the request's actual
/// address ([`Entry::matches`]), and at resolve time, against whatever address
/// DNS answers, for a hostname destination.
///
/// An IPv6 address embedding one of these as IPv4 (IPv4-mapped, NAT64, 6to4,
/// or IPv4-compatible) is classified by the embedded address, the same way
/// [`crate::net_policy::is_disallowed_ip`] is, through the same
/// [`crate::net_policy::embedded_ipv4`] this reuses: an IPv6-only check here
/// would let a DNS answer in one of those forms carry a cloud-metadata or
/// link-local address past a resolver that only sees ordinary global unicast.
///
/// Deliberately narrower than [`crate::net_policy::is_disallowed_ip`]: loopback
/// and RFC 1918 / unique-local space is exactly what an entry exists to allow.
pub(crate) fn is_never_allowable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_never_allowable_v4(v4),
        IpAddr::V6(v6) => match crate::net_policy::embedded_ipv4(v6) {
            Some(v4) => is_never_allowable_v4(v4),
            None => {
                v6.is_unspecified() || v6.is_multicast() || {
                    let s = v6.segments();
                    (s[0] & 0xffc0) == 0xfe80 // fe80::/10, link-local
                }
            }
        },
    }
}

/// The IPv4 half of [`is_never_allowable`].
fn is_never_allowable_v4(v4: Ipv4Addr) -> bool {
    v4.is_link_local() || v4.is_multicast() || v4.is_broadcast() || v4.is_unspecified()
}

/// Split `<destination>:<port>` into its two parts, handling a bracketed IPv6
/// literal (`[::1]:6081`) whose own colons must not be mistaken for the port
/// separator.
fn parse_destination_and_port(rest: &str) -> Result<(String, u16), String> {
    let (dest, port_str) = if let Some(stripped) = rest.strip_prefix('[') {
        let (addr, after) = stripped
            .split_once(']')
            .ok_or_else(|| "missing closing `]` for a bracketed IPv6 literal".to_string())?;
        let port_str = after.strip_prefix(':').ok_or_else(|| {
            "missing port after the bracketed IPv6 literal (expected ]:<port>)".to_string()
        })?;
        (addr.to_string(), port_str)
    } else {
        let (dest, port_str) = rest
            .rsplit_once(':')
            .ok_or_else(|| "missing port (expected <destination>:<port>)".to_string())?;
        (dest.to_string(), port_str)
    };
    if dest.is_empty() {
        return Err("missing destination".to_string());
    }
    let port: u16 = port_str
        .parse()
        .map_err(|_| format!("`{port_str}` is not a valid port"))?;
    Ok((dest, port))
}

/// Classify a destination string as an IP literal, a CIDR range, or a hostname.
fn parse_destination(dest: &str) -> Result<Destination, String> {
    if let Ok(ip) = dest.parse::<IpAddr>() {
        return Ok(Destination::Ip(ip));
    }
    if dest.contains('/') {
        let net = IpNet::parse(dest)?;
        return Ok(Destination::Cidr(net));
    }
    let valid_hostname = !dest.is_empty()
        && dest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_');
    if !valid_hostname {
        return Err(format!(
            "`{dest}` is not a valid IPv4/IPv6 literal, CIDR range, or hostname"
        ));
    }
    Ok(Destination::Host(dest.to_ascii_lowercase()))
}

/// The parsed `PLUGIN_HTTP_PRIVATE_ALLOW` setting: comma-separated
/// `<plugin>@<destination>:<port>` entries. Empty is today's behaviour: no
/// plugin reaches a private or loopback address.
#[derive(Debug, Clone, Default)]
pub(crate) struct PluginHttpAllow {
    entries: Vec<Entry>,
}

impl PluginHttpAllow {
    /// The empty allowlist: every plugin keeps today's behaviour.
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// Parse a comma-separated `PLUGIN_HTTP_PRIVATE_ALLOW` value.
    ///
    /// Every entry must parse and name an allowable destination; a typo is an
    /// error rather than a dropped entry, the same reasoning
    /// [`crate::client_addr::TrustedProxies::parse`] documents for
    /// `TRUSTED_PROXIES`.
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let entries = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(Entry::parse)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { entries })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Every configured entry's plugin name `is_known` says it has never
    /// heard of, for the startup warning: such an entry is inert rather than
    /// an error, since the plugin may simply not be installed on this
    /// deployment yet.
    pub(crate) fn unknown_plugins(&self, is_known: impl Fn(&str) -> bool) -> Vec<&str> {
        self.entries
            .iter()
            .map(|entry| entry.plugin.as_str())
            .filter(|name| !is_known(name))
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).expect("test URL parses")
    }

    #[test]
    fn parses_a_literal_ipv4_entry() {
        let allow = PluginHttpAllow::parse("trovato_cdn_purge@10.0.0.5:6081").unwrap();
        assert_eq!(allow.entries().len(), 1);
        let entry = &allow.entries()[0];
        assert_eq!(entry.plugin, "trovato_cdn_purge");
        assert_eq!(entry.port, 6081);
        assert_eq!(
            entry.destination,
            Destination::Ip("10.0.0.5".parse().unwrap())
        );
    }

    #[test]
    fn parses_a_bracketed_ipv6_entry() {
        let allow = PluginHttpAllow::parse("p1@[fd00::1]:6081").unwrap();
        assert_eq!(
            allow.entries()[0].destination,
            Destination::Ip("fd00::1".parse().unwrap())
        );
        assert_eq!(allow.entries()[0].port, 6081);
    }

    #[test]
    fn parses_a_cidr_entry() {
        let allow = PluginHttpAllow::parse("p1@10.0.0.0/8:6081").unwrap();
        assert!(matches!(
            allow.entries()[0].destination,
            Destination::Cidr(_)
        ));
    }

    #[test]
    fn parses_a_hostname_entry_and_lowercases_it() {
        let allow = PluginHttpAllow::parse("p1@Varnish.Internal:6081").unwrap();
        assert_eq!(
            allow.entries()[0].destination,
            Destination::Host("varnish.internal".to_string())
        );
    }

    #[test]
    fn parses_several_comma_separated_entries() {
        let allow = PluginHttpAllow::parse("p1@10.0.0.5:6081, p2@varnish.internal:80").unwrap();
        assert_eq!(allow.entries().len(), 2);
    }

    #[test]
    fn empty_value_is_the_empty_allowlist() {
        assert!(PluginHttpAllow::parse("").unwrap().is_empty());
        assert!(PluginHttpAllow::parse("   ").unwrap().is_empty());
    }

    /// An entry naming a plugin nobody installed is reported, not refused: it
    /// is inert, the state behind a one-time startup warning, not a parse error.
    #[test]
    fn unknown_plugins_reports_only_entries_whose_plugin_is_not_known() {
        let allow = PluginHttpAllow::parse("p1@127.0.0.1:80,p2@10.0.0.5:81").unwrap();
        let known = |name: &str| name == "p1";
        assert_eq!(allow.unknown_plugins(known), vec!["p2"]);
    }

    #[test]
    fn unknown_plugins_is_empty_when_every_plugin_is_known() {
        let allow = PluginHttpAllow::parse("p1@127.0.0.1:80").unwrap();
        assert!(allow.unknown_plugins(|_| true).is_empty());
    }

    #[test]
    fn missing_at_sign_is_refused() {
        let err = PluginHttpAllow::parse("10.0.0.5:6081").unwrap_err();
        assert!(err.contains('@'), "got: {err}");
    }

    #[test]
    fn missing_port_is_refused() {
        let err = PluginHttpAllow::parse("p1@10.0.0.5").unwrap_err();
        assert!(err.contains("port"), "got: {err}");
    }

    #[test]
    fn invalid_port_is_refused() {
        let err = PluginHttpAllow::parse("p1@10.0.0.5:not-a-port").unwrap_err();
        assert!(err.contains("port"), "got: {err}");
    }

    #[test]
    fn invalid_plugin_machine_name_is_refused() {
        let err = PluginHttpAllow::parse("Not-A-Valid-Name@10.0.0.5:6081").unwrap_err();
        assert!(err.contains("plugin"), "got: {err}");
    }

    #[test]
    fn unknown_destination_shape_is_refused() {
        let err = PluginHttpAllow::parse("p1@not a hostname!!:6081").unwrap_err();
        assert!(err.contains("valid"), "got: {err}");
    }

    /// Point (g): link-local (including the cloud-metadata address),
    /// multicast, broadcast and unspecified destinations can never be named,
    /// whether as a literal or a CIDR.
    #[test]
    fn never_allowable_literals_and_cidrs_are_refused() {
        for raw in [
            "p1@169.254.169.254:80", // cloud metadata
            "p1@169.254.0.0/16:80",  // link-local CIDR
            "p1@224.0.0.1:80",       // multicast
            "p1@255.255.255.255:80", // broadcast
            "p1@0.0.0.0:80",         // unspecified
            "p1@[fe80::1]:80",       // IPv6 link-local
            "p1@[ff02::1]:80",       // IPv6 multicast
            "p1@[::]:80",            // IPv6 unspecified
        ] {
            let err = PluginHttpAllow::parse(raw).unwrap_err();
            assert!(
                err.contains("link-local") || err.contains("multicast"),
                "{raw} should be refused, got: {err}"
            );
        }
    }

    #[test]
    fn loopback_and_private_literals_are_allowed_to_parse() {
        for raw in ["p1@127.0.0.1:80", "p1@10.0.0.5:80", "p1@192.168.1.1:80"] {
            PluginHttpAllow::parse(raw).expect("loopback/private literals must parse");
        }
    }

    #[test]
    fn matches_requires_the_same_plugin() {
        let allow = PluginHttpAllow::parse("p1@127.0.0.1:6081").unwrap();
        let entry = &allow.entries()[0];
        assert!(entry.matches("p1", &url("http://127.0.0.1:6081/x")));
        assert!(!entry.matches("p2", &url("http://127.0.0.1:6081/x")));
    }

    #[test]
    fn matches_requires_the_exact_port() {
        let allow = PluginHttpAllow::parse("p1@127.0.0.1:6081").unwrap();
        let entry = &allow.entries()[0];
        assert!(!entry.matches("p1", &url("http://127.0.0.1:6082/x")));
    }

    #[test]
    fn matches_requires_http_or_https_scheme() {
        let allow = PluginHttpAllow::parse("p1@127.0.0.1:21").unwrap();
        let entry = &allow.entries()[0];
        assert!(!entry.matches("p1", &url("ftp://127.0.0.1:21/x")));
    }

    #[test]
    fn cidr_entry_matches_containment_not_equality() {
        let allow = PluginHttpAllow::parse("p1@10.0.0.0/8:6081").unwrap();
        let entry = &allow.entries()[0];
        assert!(entry.matches("p1", &url("http://10.9.8.7:6081/")));
        assert!(!entry.matches("p1", &url("http://11.0.0.1:6081/")));
    }

    /// A CIDR's parse-time check only runs against its network address
    /// (`destination_sample_ip`), so a broad or misaligned range can parse
    /// successfully while still containing a never-allowable address: here
    /// `169.0.0.0/8`'s network address is not itself link-local, but the
    /// range fully contains `169.254.0.0/16`. `matches` must still refuse a
    /// request to that address, not just whatever address the CIDR was
    /// written against.
    #[test]
    fn cidr_entry_never_grants_a_never_allowable_address_it_happens_to_contain() {
        let allow =
            PluginHttpAllow::parse("p1@169.0.0.0/8:6081").expect("a broad CIDR still parses");
        let entry = &allow.entries()[0];
        assert!(
            !entry.matches("p1", &url("http://169.254.169.254:6081/")),
            "cloud metadata must never match, even inside a CIDR that contains it"
        );
        // An address the same CIDR contains that is NOT never-allowable still matches.
        assert!(entry.matches("p1", &url("http://169.1.2.3:6081/")));
    }

    /// A literal entry equal to a never-allowable address is refused at parse
    /// time already (`never_allowable_literals_and_cidrs_are_refused`); this
    /// is the second line of defense at the match layer, exercised the same
    /// way the CIDR case above is, so the two checks cannot silently diverge.
    #[test]
    fn literal_entry_match_also_runs_the_never_allowable_check() {
        // A hand-built entry bypassing `parse`'s own guard, so this test
        // exercises `matches`'s check in isolation.
        let entry = Entry {
            plugin: "p1".to_string(),
            destination: Destination::Ip("169.254.169.254".parse().unwrap()),
            port: 80,
        };
        assert!(!entry.matches("p1", &url("http://169.254.169.254:80/")));
    }

    #[test]
    fn hostname_entry_matches_case_insensitively_but_not_other_hosts() {
        let allow = PluginHttpAllow::parse("p1@Varnish.Internal:6081").unwrap();
        let entry = &allow.entries()[0];
        assert!(entry.matches("p1", &url("http://varnish.internal:6081/")));
        assert!(entry.matches("p1", &url("http://VARNISH.INTERNAL:6081/")));
        assert!(!entry.matches("p1", &url("http://other.internal:6081/")));
        // A literal address never matches a hostname entry, even the one the
        // hostname happens to resolve to.
        assert!(!entry.matches("p1", &url("http://10.0.0.5:6081/")));
    }

    #[test]
    fn literal_entry_matches_an_ipv4_mapped_ipv6_form_of_the_same_address() {
        let allow = PluginHttpAllow::parse("p1@10.0.0.5:6081").unwrap();
        let entry = &allow.entries()[0];
        assert!(entry.matches("p1", &url("http://[::ffff:10.0.0.5]:6081/")));
    }

    #[test]
    fn is_never_allowable_matches_the_four_categories_and_nothing_else() {
        for ip in ["169.254.169.254", "224.0.0.1", "255.255.255.255", "0.0.0.0"] {
            assert!(
                is_never_allowable(ip.parse().unwrap()),
                "{ip} must be refused"
            );
        }
        for ip in ["127.0.0.1", "10.0.0.5", "192.168.1.1", "8.8.8.8", "fd00::1"] {
            assert!(
                !is_never_allowable(ip.parse().unwrap()),
                "{ip} must be allowed"
            );
        }
    }

    /// A never-allowable IPv4 address embedded in an IPv6 form (IPv4-mapped,
    /// NAT64, or 6to4) is still refused, the same way a DNS answer in one of
    /// those forms is still refused by [`crate::host::http::EntryAllowResolver`]
    /// for a hostname entry's own matched name.
    #[test]
    fn is_never_allowable_sees_through_every_ipv6_embedding_of_a_forbidden_address() {
        for addr in [
            "::ffff:169.254.169.254", // IPv4-mapped cloud metadata
            "64:ff9b::a9fe:a9fe",     // NAT64 169.254.169.254
            "2002:a9fe:a9fe::",       // 6to4 wrapping 169.254.169.254
            "::ffff:224.0.0.1",       // IPv4-mapped multicast
        ] {
            assert!(
                is_never_allowable(addr.parse().unwrap()),
                "{addr} embeds a forbidden address and must be refused"
            );
        }
        // The same forms wrapping a public address must still be allowed.
        assert!(!is_never_allowable("::ffff:8.8.8.8".parse().unwrap()));
    }
}
