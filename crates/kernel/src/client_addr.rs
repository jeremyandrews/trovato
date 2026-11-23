//! The client address: who sent this request, and what of that may be kept.
//!
//! **One resolver.** [`resolve_client_addr`] is the only code in the kernel
//! that decides a request's client address. The `resolve_client_ip` middleware
//! calls it once per request and stores the answer as the
//! [`ClientIp`](crate::middleware::ClientIp) request extension; rate limiting,
//! sign-in lockout, access rules, the security audit stream and the session
//! index all read that extension. Nothing else reads `X-Forwarded-For`.
//!
//! The socket peer is the client unless the peer is a configured trusted proxy
//! (`TRUSTED_PROXIES`, addresses or CIDR ranges, empty by default). Only then is
//! `X-Forwarded-For` read, from the right, skipping trusted hops; the first
//! address that is not a trusted proxy is the client. A forwarding header from
//! any other peer is ignored, whatever it says: whoever sends it would
//! otherwise pick their own address.
//!
//! `X-Forwarded-Proto` goes through the same gate: [`forwarded_https`] believes
//! it only from a trusted proxy, and it is what decides whether a response
//! carries `Strict-Transport-Security`. A direct client's claim to have
//! connected over HTTPS is ignored.
//!
//! The RFC 7239 `Forwarded` header is not read. Every proxy a Trovato site is
//! deployed behind (nginx, HAProxy, Caddy, Traefik, the Kubernetes ingress
//! controllers, Cloudflare) writes `X-Forwarded-For`; `Forwarded` needs a
//! parser for quoted strings, bracketed IPv6, ports and obfuscated identifiers,
//! and reading two headers raises the question of which wins when they
//! disagree, which is exactly where a spoofed header slips through.
//!
//! **One storage policy.** [`ClientIpStorage`] (`CLIENT_IP_STORAGE`) says what
//! of the address may be written anywhere that outlives the request: the
//! database, the session index, the logs. Rate limiting and lockout always use
//! the full address, in Redis, under keys that expire within the hour; they
//! never persist it.

use std::fmt;
use std::net::IpAddr;

/// An address or CIDR range in `TRUSTED_PROXIES`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IpNet {
    network: IpAddr,
    prefix: u8,
}

impl IpNet {
    /// Parse `10.0.0.5`, `10.0.0.0/8`, `::1` or `fd00::/8`.
    ///
    /// Host bits set below the prefix (`10.0.0.5/8`) are accepted and masked
    /// off, as `ip route` does.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        let (addr, prefix) = match raw.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (raw, None),
        };
        let addr: IpAddr = addr
            .parse()
            .map_err(|_| format!("`{raw}` is not an IP address or CIDR range"))?;
        let addr = addr.to_canonical();
        let max = max_prefix(addr);
        let prefix = match prefix {
            None => max,
            Some(p) => match p.parse::<u8>() {
                Ok(p) if p <= max => p,
                _ => {
                    return Err(format!(
                        "`{raw}` has an invalid prefix length (0-{max} for this address family)"
                    ));
                }
            },
        };
        Ok(Self {
            network: mask(addr, prefix),
            prefix,
        })
    }

    /// Whether `ip` falls inside this range.
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        ip.is_ipv4() == self.network.is_ipv4() && mask(ip, self.prefix) == self.network
    }
}

impl fmt::Display for IpNet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

fn max_prefix(ip: IpAddr) -> u8 {
    if ip.is_ipv4() { 32 } else { 128 }
}

/// `ip` with every bit below `prefix` cleared.
fn mask(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let bits = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            IpAddr::V4((u32::from(v4) & bits).into())
        }
        IpAddr::V6(v6) => {
            let bits = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            IpAddr::V6((u128::from(v6) & bits).into())
        }
    }
}

/// The proxies whose `X-Forwarded-For` is believed. Empty trusts none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<IpNet>);

impl TrustedProxies {
    /// Parse a comma-separated `TRUSTED_PROXIES` value.
    ///
    /// Every entry must parse. A typo is an error rather than a dropped entry:
    /// a dropped entry silently stops trusting a real proxy, and every visitor
    /// then shares the proxy's one address and one rate-limit budget.
    pub fn parse(raw: &str) -> Result<Self, String> {
        raw.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(IpNet::parse)
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    /// Whether `ip` is a trusted proxy.
    pub fn contains(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|net| net.contains(ip))
    }

    /// Whether no proxy is trusted.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many entries are configured.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl From<Vec<IpAddr>> for TrustedProxies {
    fn from(addrs: Vec<IpAddr>) -> Self {
        Self(
            addrs
                .into_iter()
                .map(|a| {
                    let a = a.to_canonical();
                    IpNet {
                        network: a,
                        prefix: max_prefix(a),
                    }
                })
                .collect(),
        )
    }
}

/// One `X-Forwarded-For` hop as an address: a bare address, or `addr:port` /
/// `[v6]:port`, which some load balancers write.
fn parse_hop(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim();
    entry
        .parse::<IpAddr>()
        .or_else(|_| entry.parse::<std::net::SocketAddr>().map(|s| s.ip()))
        .ok()
        .map(|ip| ip.to_canonical())
}

/// The client address for a request. **The one resolver** (see the module docs).
///
/// - No peer (a transport with no socket): `"unknown"`.
/// - A peer that is not a trusted proxy: the peer. Forwarding headers ignored.
/// - A trusted peer: walk `X-Forwarded-For` (every header line, in order) from
///   the right, skipping trusted proxies; the first untrusted address is the
///   client. An entry that is not an address stops the walk at the last hop
///   that was vouched for. Every entry trusted: the leftmost. No
///   `X-Forwarded-For` at all: a parseable `X-Real-IP`, else the peer.
pub fn resolve_client_addr(
    peer: Option<IpAddr>,
    headers: &axum::http::HeaderMap,
    trusted: &TrustedProxies,
) -> String {
    let Some(peer) = peer.map(|p| p.to_canonical()) else {
        return "unknown".to_string();
    };
    if !trusted.contains(peer) {
        return peer.to_string();
    }

    let mut client = peer;
    let mut saw_forwarded_for = false;
    let lines: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    for entry in lines
        .iter()
        .rev()
        .flat_map(|line| line.rsplit(','))
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        saw_forwarded_for = true;
        let Some(hop) = parse_hop(entry) else {
            // Not an address: nothing to its left can be vouched for either.
            return client.to_string();
        };
        client = hop;
        if !trusted.contains(hop) {
            return hop.to_string();
        }
    }

    if !saw_forwarded_for
        && let Some(real_ip) = headers
            .get("x-real-ip")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_hop)
    {
        return real_ip.to_string();
    }
    client.to_string()
}

/// Whether a trusted proxy says the client connected over HTTPS.
///
/// The same gate as [`resolve_client_addr`]: `X-Forwarded-Proto` is read only
/// when the socket peer is a trusted proxy. From any other peer (or with no
/// peer) the answer is `false`, whatever the header says, because a direct
/// client would otherwise choose the scheme its own response is built for.
///
/// When the header carries several values (a comma list, or several header
/// lines, as a chain of proxies may send), the rightmost is the one the trusted
/// peer itself wrote, and it alone decides. It must be `https`
/// (case-insensitive); anything else, junk included, is `false`.
pub fn forwarded_https(
    peer: Option<IpAddr>,
    headers: &axum::http::HeaderMap,
    trusted: &TrustedProxies,
) -> bool {
    let Some(peer) = peer else {
        return false;
    };
    if !trusted.contains(peer) {
        return false;
    }
    headers
        .get_all("x-forwarded-proto")
        .iter()
        .next_back()
        .and_then(|v| v.to_str().ok())
        .and_then(|line| line.rsplit(',').next())
        .is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"))
}

/// What of a client address may be kept (`CLIENT_IP_STORAGE`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClientIpStorage {
    /// The whole address. The default, and the behaviour before the setting.
    #[default]
    Full,
    /// IPv4 masked to its /24, IPv6 to its /48: the network, not the host.
    Truncated,
    /// Nothing. Every column that would hold an address gets NULL (or the empty
    /// string where the column is `NOT NULL`).
    None,
}

impl ClientIpStorage {
    /// Parse `full`, `truncated` or `none` (case-insensitive).
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "full" | "" => Ok(Self::Full),
            "truncated" => Ok(Self::Truncated),
            "none" => Ok(Self::None),
            other => Err(format!(
                "`{other}` is not a client IP storage setting (full, truncated or none)"
            )),
        }
    }

    /// The setting's name, as `CLIENT_IP_STORAGE` spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Truncated => "truncated",
            Self::None => "none",
        }
    }

    /// The form of `raw` that may be stored, or `None` when nothing may be.
    ///
    /// Anything that is not an address becomes `"invalid"` rather than being
    /// stored verbatim (under `full` and `truncated`).
    pub fn apply(self, raw: &str) -> Option<String> {
        let parsed = raw
            .trim()
            .parse::<IpAddr>()
            .ok()
            .map(|ip| ip.to_canonical());
        match (self, parsed) {
            (Self::None, _) => None,
            (_, None) => Some("invalid".to_string()),
            (Self::Full, Some(ip)) => Some(ip.to_string()),
            (Self::Truncated, Some(ip)) => {
                let prefix = if ip.is_ipv4() { 24 } else { 48 };
                Some(mask(ip, prefix).to_string())
            }
        }
    }

    /// [`apply`](Self::apply) for a field that cannot be NULL (a log field, the
    /// session index): the empty string when nothing may be kept.
    pub fn kept_or_empty(self, raw: &str) -> String {
        self.apply(raw).unwrap_or_default()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    fn peer(s: &str) -> Option<IpAddr> {
        Some(s.parse().unwrap())
    }

    fn trusted(list: &str) -> TrustedProxies {
        TrustedProxies::parse(list).unwrap()
    }

    fn xff(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", value.parse().unwrap());
        h
    }

    const PROXY: &str = "10.9.8.7";

    /// The spoofing case: a direct client sends `X-Forwarded-For` and
    /// `X-Real-IP` naming someone else. Neither is read.
    #[test]
    fn a_spoofed_header_from_an_untrusted_peer_is_ignored() {
        let mut headers = xff("1.2.3.4");
        headers.insert("x-real-ip", "10.0.0.1".parse().unwrap());
        // Nothing trusted (the default).
        assert_eq!(
            resolve_client_addr(peer("203.0.113.9"), &headers, &TrustedProxies::default()),
            "203.0.113.9"
        );
        // Something trusted, but not this peer.
        assert_eq!(
            resolve_client_addr(peer("203.0.113.9"), &headers, &trusted(PROXY)),
            "203.0.113.9"
        );
        // Nor a peer just outside a trusted range.
        assert_eq!(
            resolve_client_addr(peer("10.9.9.1"), &headers, &trusted("10.9.8.0/24")),
            "10.9.9.1"
        );
    }

    /// Behind two trusted proxies, the client is the first entry from the right
    /// that is not itself a trusted proxy; the client's own claim to its left
    /// is ignored.
    #[test]
    fn a_chain_through_two_trusted_proxies_resolves_the_right_hop() {
        // client claimed 6.6.6.6; the edge appended the real 198.51.100.7;
        // the inner proxy (the socket peer) appended the edge, 10.9.8.1.
        let headers = xff("6.6.6.6, 198.51.100.7, 10.9.8.1");
        assert_eq!(
            resolve_client_addr(peer(PROXY), &headers, &trusted("10.9.8.7, 10.9.8.1")),
            "198.51.100.7"
        );
        // The same chain with the two proxies given as one CIDR range.
        assert_eq!(
            resolve_client_addr(peer(PROXY), &headers, &trusted("10.9.8.0/24")),
            "198.51.100.7"
        );
        // Split over two header lines, as some proxies send it.
        let mut split = HeaderMap::new();
        split.append("x-forwarded-for", "6.6.6.6, 198.51.100.7".parse().unwrap());
        split.append("x-forwarded-for", "10.9.8.1".parse().unwrap());
        assert_eq!(
            resolve_client_addr(peer(PROXY), &split, &trusted("10.9.8.0/24")),
            "198.51.100.7"
        );
    }

    #[test]
    fn a_trusted_proxy_without_forwarded_for_falls_back_to_real_ip_then_itself() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            resolve_client_addr(peer(PROXY), &headers, &trusted(PROXY)),
            PROXY
        );
        headers.insert("x-real-ip", "not an address".parse().unwrap());
        assert_eq!(
            resolve_client_addr(peer(PROXY), &headers, &trusted(PROXY)),
            PROXY
        );
        headers.insert("x-real-ip", "198.51.100.9".parse().unwrap());
        assert_eq!(
            resolve_client_addr(peer(PROXY), &headers, &trusted(PROXY)),
            "198.51.100.9"
        );
    }

    /// `X-Real-IP` is a fallback for a proxy that writes no `X-Forwarded-For`,
    /// not a second opinion that overrides it.
    #[test]
    fn forwarded_for_wins_over_real_ip() {
        let mut headers = xff("198.51.100.7");
        headers.insert("x-real-ip", "6.6.6.6".parse().unwrap());
        assert_eq!(
            resolve_client_addr(peer(PROXY), &headers, &trusted(PROXY)),
            "198.51.100.7"
        );
    }

    #[test]
    fn a_garbage_hop_stops_the_walk_at_the_last_vouched_address() {
        let headers = xff("198.51.100.7, <script>, 10.9.8.1");
        assert_eq!(
            resolve_client_addr(peer(PROXY), &headers, &trusted("10.9.8.0/24")),
            "10.9.8.1"
        );
    }

    #[test]
    fn hops_with_ports_whitespace_and_empty_entries_parse() {
        let headers = xff(" 6.6.6.6 , 198.51.100.7:4711 , ");
        assert_eq!(
            resolve_client_addr(peer(PROXY), &headers, &trusted(PROXY)),
            "198.51.100.7"
        );
        let headers = xff("[2001:db8::7]:443");
        assert_eq!(
            resolve_client_addr(peer(PROXY), &headers, &trusted(PROXY)),
            "2001:db8::7"
        );
    }

    #[test]
    fn every_hop_trusted_gives_the_leftmost() {
        let headers = xff("10.9.8.2, 10.9.8.1");
        assert_eq!(
            resolve_client_addr(peer(PROXY), &headers, &trusted("10.9.8.0/24")),
            "10.9.8.2"
        );
    }

    #[test]
    fn an_ipv4_mapped_peer_matches_an_ipv4_range() {
        assert_eq!(
            resolve_client_addr(
                peer("::ffff:10.9.8.7"),
                &xff("198.51.100.7"),
                &trusted("10.0.0.0/8")
            ),
            "198.51.100.7"
        );
    }

    #[test]
    fn no_peer_is_unknown() {
        assert_eq!(
            resolve_client_addr(None, &xff("1.2.3.4"), &trusted(PROXY)),
            "unknown"
        );
    }

    fn proto(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-proto", value.parse().unwrap());
        h
    }

    /// The spoofing case: a direct client claims HTTPS. Not believed.
    #[test]
    fn forwarded_https_from_an_untrusted_peer_is_false() {
        assert!(!forwarded_https(
            peer("203.0.113.9"),
            &proto("https"),
            &trusted(PROXY)
        ));
        assert!(!forwarded_https(None, &proto("https"), &trusted(PROXY)));
    }

    #[test]
    fn forwarded_https_from_a_trusted_peer() {
        assert!(forwarded_https(
            peer(PROXY),
            &proto("https"),
            &trusted(PROXY)
        ));
        assert!(forwarded_https(
            peer(PROXY),
            &proto("HTTPS"),
            &trusted(PROXY)
        ));
        assert!(forwarded_https(
            peer(PROXY),
            &proto(" https "),
            &trusted(PROXY)
        ));
        assert!(!forwarded_https(
            peer(PROXY),
            &proto("http"),
            &trusted(PROXY)
        ));
        // A trusted proxy that sends no header says nothing about the scheme.
        assert!(!forwarded_https(
            peer(PROXY),
            &HeaderMap::new(),
            &trusted(PROXY)
        ));
    }

    /// The default: nothing trusted, so even the loopback peer is a client.
    #[test]
    fn forwarded_https_with_no_trusted_proxies_is_false() {
        assert!(!forwarded_https(
            peer("127.0.0.1"),
            &proto("https"),
            &TrustedProxies::default()
        ));
    }

    #[test]
    fn forwarded_https_from_a_peer_inside_a_trusted_range() {
        let range = trusted("10.9.8.0/24, fd00::/8");
        assert!(forwarded_https(peer("10.9.8.200"), &proto("https"), &range));
        assert!(forwarded_https(peer("fd12::1"), &proto("https"), &range));
        assert!(forwarded_https(
            peer("::ffff:10.9.8.7"),
            &proto("https"),
            &range
        ));
        assert!(!forwarded_https(peer("10.9.9.1"), &proto("https"), &range));
    }

    /// Several values: the rightmost, the one the trusted peer wrote, decides.
    #[test]
    fn forwarded_https_takes_the_value_nearest_the_trusted_hop() {
        let t = trusted(PROXY);
        assert!(forwarded_https(peer(PROXY), &proto("http, https"), &t));
        assert!(!forwarded_https(peer(PROXY), &proto("https, http"), &t));
        assert!(!forwarded_https(peer(PROXY), &proto("https,"), &t));
        let mut lines = HeaderMap::new();
        lines.append("x-forwarded-proto", "https".parse().unwrap());
        lines.append("x-forwarded-proto", "http".parse().unwrap());
        assert!(!forwarded_https(peer(PROXY), &lines, &t));
        let mut lines = HeaderMap::new();
        lines.append("x-forwarded-proto", "http".parse().unwrap());
        lines.append("x-forwarded-proto", "https".parse().unwrap());
        assert!(forwarded_https(peer(PROXY), &lines, &t));
    }

    #[test]
    fn forwarded_https_junk_is_false() {
        let t = trusted(PROXY);
        for junk in ["", "httpsx", "https;", "ftp", "<script>", "1"] {
            assert!(
                !forwarded_https(peer(PROXY), &proto(junk), &t),
                "{junk:?} must not mean https"
            );
        }
        let mut bytes = HeaderMap::new();
        bytes.insert(
            "x-forwarded-proto",
            axum::http::HeaderValue::from_bytes(b"https\xff").unwrap(),
        );
        assert!(!forwarded_https(peer(PROXY), &bytes, &t));
    }

    #[test]
    fn trusted_proxies_parse_addresses_and_ranges() {
        let t = trusted("127.0.0.1, , 10.0.0.0/8 ,::1, fd00::/8");
        assert_eq!(t.len(), 4);
        assert!(t.contains("127.0.0.1".parse().unwrap()));
        assert!(t.contains("10.200.3.4".parse().unwrap()));
        assert!(t.contains("::1".parse().unwrap()));
        assert!(t.contains("fd12:3456::1".parse().unwrap()));
        assert!(!t.contains("11.0.0.1".parse().unwrap()));
        assert!(!t.contains("fe80::1".parse().unwrap()));
        assert!(TrustedProxies::parse("").unwrap().is_empty());
        // Host bits below the prefix are masked, not refused.
        assert!(trusted("10.0.0.5/8").contains("10.1.1.1".parse().unwrap()));
        assert!(trusted("0.0.0.0/0").contains("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn a_bad_trusted_proxy_entry_is_an_error_naming_it() {
        for bad in ["notanip", "10.0.0.0/33", "::/129", "10.0.0.0/x", "10.0.0/8"] {
            let err = TrustedProxies::parse(&format!("127.0.0.1, {bad}")).unwrap_err();
            assert!(err.contains(bad), "{err}");
        }
    }

    #[test]
    fn storage_full_truncated_none() {
        use ClientIpStorage::*;
        assert_eq!(Full.apply("203.0.113.77").as_deref(), Some("203.0.113.77"));
        assert_eq!(
            Truncated.apply("203.0.113.77").as_deref(),
            Some("203.0.113.0")
        );
        assert_eq!(
            Truncated.apply("2001:db8:abcd:1234::99").as_deref(),
            Some("2001:db8:abcd::")
        );
        assert_eq!(
            Truncated.apply("::ffff:203.0.113.77").as_deref(),
            Some("203.0.113.0")
        );
        assert_eq!(None.apply("203.0.113.77"), Option::None);
        assert_eq!(Full.apply("unknown").as_deref(), Some("invalid"));
        assert_eq!(Truncated.apply("x").as_deref(), Some("invalid"));
        assert_eq!(None.kept_or_empty("203.0.113.77"), "");
        // Truncating twice changes nothing.
        assert_eq!(
            Truncated.apply("203.0.113.0").as_deref(),
            Some("203.0.113.0")
        );
    }

    /// The cost per request, for the record: `cargo test --release -p
    /// trovato-kernel --lib client_addr::tests::resolver_cost -- --ignored
    /// --nocapture`. Worst realistic case: a trusted peer, a three-hop header,
    /// a CIDR list of four.
    #[test]
    #[ignore = "a timing, not a check"]
    fn resolver_cost() {
        let trusted = trusted("10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, fd00::/8");
        let headers = xff("6.6.6.6, 198.51.100.7, 10.9.8.1");
        let proxy_peer = peer(PROXY);
        let n = 1_000_000u32;
        let start = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(resolve_client_addr(
                std::hint::black_box(proxy_peer),
                std::hint::black_box(&headers),
                &trusted,
            ));
        }
        let proxied = start.elapsed() / n;
        let direct_headers = HeaderMap::new();
        let start = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(resolve_client_addr(
                std::hint::black_box(peer("203.0.113.9")),
                std::hint::black_box(&direct_headers),
                &trusted,
            ));
        }
        let direct = start.elapsed() / n;
        let start = std::time::Instant::now();
        for _ in 0..n {
            std::hint::black_box(
                ClientIpStorage::Truncated.apply(std::hint::black_box("203.0.113.77")),
            );
        }
        let truncate = start.elapsed() / n;
        println!(
            "resolve, trusted proxy, 3 hops: {proxied:?}/request; direct peer: {direct:?}/request; truncate: {truncate:?}/call"
        );
    }

    #[test]
    fn storage_parses_and_refuses_junk() {
        assert_eq!(
            ClientIpStorage::parse("FULL").unwrap(),
            ClientIpStorage::Full
        );
        assert_eq!(
            ClientIpStorage::parse(" truncated ").unwrap(),
            ClientIpStorage::Truncated
        );
        assert_eq!(
            ClientIpStorage::parse("none").unwrap(),
            ClientIpStorage::None
        );
        assert!(
            ClientIpStorage::parse("hashed")
                .unwrap_err()
                .contains("hashed")
        );
    }
}
