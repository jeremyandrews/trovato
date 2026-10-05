//! The outbound-address policy: which IP addresses the kernel refuses to send a
//! plugin-influenced or administrator-configured request to.
//!
//! This is one classifier, shared by every layer of the SSRF fence in
//! [`crate::host::http`] (the pre-send literal check, the validating DNS
//! resolver, and the per-hop redirect policy) and by the AI provider's
//! [`crate::services::ai_provider::validate_base_url`]. It used to be two
//! copies with two different range lists, which is how `0.0.0.0/8` came to be
//! blocked for an administrator-set provider URL and allowed for a plugin, and
//! CGNAT the other way around.
//!
//! # Address forms
//!
//! An IPv6 address can carry an IPv4 address inside it, and a classifier that
//! looks only at the IPv6 bits reads those as ordinary global unicast. So the
//! embedded form is unwrapped first and the IPv4 address inside is what gets
//! classified: IPv4-mapped (`::ffff:a.b.c.d`), IPv4-compatible (`::a.b.c.d`),
//! NAT64 (`64:ff9b::/96`) and 6to4 (`2002::/16`).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Is this address one the kernel refuses to open an outbound connection to?
///
/// True for loopback, private, link-local, unique-local, multicast, broadcast,
/// unspecified, carrier-grade NAT, benchmarking and otherwise reserved space —
/// and for any of those reached through an IPv6 address that embeds an IPv4 one.
pub(crate) fn is_disallowed_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_disallowed_v4(v4),
        IpAddr::V6(v6) => match embedded_ipv4(v6) {
            Some(v4) => is_disallowed_v4(v4),
            None => is_disallowed_v6(v6),
        },
    }
}

/// The IPv4 address an IPv6 address carries inside it, if it carries one.
///
/// `::1` and `::` are *not* treated as the IPv4-compatible forms of `0.0.0.1`
/// and `0.0.0.0`: they are the IPv6 loopback and unspecified addresses, and the
/// IPv6 arm names them for what they are. Both are refused either way.
fn embedded_ipv4(v6: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = v6.to_ipv4_mapped() {
        return Some(v4);
    }

    let s = v6.segments();

    // NAT64 well-known prefix, 64:ff9b::/96.
    if s[0] == 0x0064 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return Some(ipv4_from_segments(s[6], s[7]));
    }

    // 6to4, 2002::/16: the IPv4 address of the relay is the next 32 bits.
    if s[0] == 0x2002 {
        return Some(ipv4_from_segments(s[1], s[2]));
    }

    // IPv4-compatible, ::a.b.c.d, excluding `::` and `::1`.
    if s[0..6] == [0, 0, 0, 0, 0, 0] && !v6.is_loopback() && !v6.is_unspecified() {
        return Some(ipv4_from_segments(s[6], s[7]));
    }

    None
}

/// Assemble an IPv4 address from the two IPv6 segments holding it.
fn ipv4_from_segments(hi: u16, lo: u16) -> Ipv4Addr {
    Ipv4Addr::from(((hi as u32) << 16) | lo as u32)
}

/// The IPv4 half of the policy: the union of every range either former copy
/// blocked, plus the reserved space neither of them did.
fn is_disallowed_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    v4.is_loopback()                        // 127.0.0.0/8
        || v4.is_private()                  // 10/8, 172.16/12, 192.168/16
        || v4.is_link_local()               // 169.254.0.0/16, cloud metadata
        || v4.is_multicast()                // 224.0.0.0/4
        || v4.is_broadcast()                // 255.255.255.255
        || o[0] == 0                        // 0.0.0.0/8, "this network"
        || o[0] == 100 && (o[1] & 0xC0) == 64 // 100.64.0.0/10, carrier-grade NAT
        || o[0] == 192 && o[1] == 0 && o[2] == 0 // 192.0.0.0/24, IETF assignments
        || o[0] == 198 && (o[1] & 0xFE) == 18 // 198.18.0.0/15, benchmarking
        || o[0] >= 240 // 240.0.0.0/4, reserved (and 255.255.255.255 with it)
}

/// The IPv6 half of the policy, for an address that embeds no IPv4 one.
fn is_disallowed_v6(v6: Ipv6Addr) -> bool {
    let s = v6.segments();
    v6.is_loopback()                   // ::1
        || v6.is_unspecified()         // ::
        || (s[0] & 0xffc0) == 0xfe80   // fe80::/10, link-local
        || (s[0] & 0xfe00) == 0xfc00   // fc00::/7, unique-local
        || (s[0] & 0xff00) == 0xff00 // ff00::/8, multicast
}

#[cfg(test)]
// Tests are allowed to use unwrap/expect freely.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("test address parses")
    }

    #[test]
    fn ipv6_literals_in_every_internal_range_are_refused() {
        for addr in ["::1", "::", "fd00::1", "fe80::1", "ff02::1", "fc00::abcd"] {
            assert!(is_disallowed_ip(ip(addr)), "{addr} must be refused");
        }
    }

    #[test]
    fn ipv6_addresses_embedding_an_internal_ipv4_are_refused() {
        for addr in [
            "::ffff:127.0.0.1",       // IPv4-mapped loopback
            "::ffff:169.254.169.254", // IPv4-mapped cloud metadata
            "::ffff:10.0.0.1",        // IPv4-mapped RFC 1918
            "::127.0.0.1",            // IPv4-compatible loopback
            "64:ff9b::127.0.0.1",     // NAT64 loopback
            "64:ff9b::a00:1",         // NAT64 10.0.0.1
            "2002:7f00:1::",          // 6to4 wrapping 127.0.0.1
            "2002:a9fe:a9fe::",       // 6to4 wrapping 169.254.169.254
        ] {
            assert!(is_disallowed_ip(ip(addr)), "{addr} must be refused");
        }
    }

    #[test]
    fn ipv6_addresses_embedding_a_public_ipv4_are_allowed() {
        for addr in ["::ffff:8.8.8.8", "64:ff9b::808:808", "2002:0808:0808::"] {
            assert!(!is_disallowed_ip(ip(addr)), "{addr} must be allowed");
        }
    }

    #[test]
    fn public_addresses_are_allowed() {
        for addr in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
        ] {
            assert!(!is_disallowed_ip(ip(addr)), "{addr} must be allowed");
        }
    }

    #[test]
    fn the_reserved_ipv4_ranges_neither_copy_blocked_are_refused() {
        for addr in [
            "0.0.0.0",
            "0.1.2.3",         // 0.0.0.0/8 beyond the unspecified address
            "100.64.0.1",      // carrier-grade NAT
            "192.0.0.1",       // IETF protocol assignments
            "198.18.0.1",      // benchmarking
            "198.19.255.255",  // benchmarking, top of the /15
            "224.0.0.1",       // multicast
            "239.255.255.255", // multicast, top of the /4
            "240.0.0.1",       // reserved
            "255.255.255.255", // broadcast
        ] {
            assert!(is_disallowed_ip(ip(addr)), "{addr} must be refused");
        }
    }

    /// The boundaries of the ranges written by hand, so an off-by-one in a mask
    /// shows up as a public address being refused rather than as silence.
    #[test]
    fn range_edges_fall_on_the_right_side() {
        assert!(!is_disallowed_ip(ip("100.63.255.255")));
        assert!(is_disallowed_ip(ip("100.64.0.0")));
        assert!(is_disallowed_ip(ip("100.127.255.255")));
        assert!(!is_disallowed_ip(ip("100.128.0.0")));
        assert!(!is_disallowed_ip(ip("192.0.1.1")));
        assert!(!is_disallowed_ip(ip("198.17.255.255")));
        assert!(!is_disallowed_ip(ip("198.20.0.0")));
        assert!(!is_disallowed_ip(ip("223.255.255.255")));
        assert!(is_disallowed_ip(ip("224.0.0.0")));
        assert!(is_disallowed_ip(ip("239.255.255.255")));
    }
}
