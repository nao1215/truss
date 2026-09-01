//! Address rules for a URL truss is asked to fetch.
//!
//! These are pure predicates over a URL and an address, with no I/O and no adapter
//! vocabulary, so the two adapters that fetch read one copy of them. The HTTP server layers
//! a deny-list, DNS pinning, and a redirect limit on top; the CLI, which is expected to
//! fetch from a developer's own machine, applies only the rules that hold everywhere.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use url::Url;

/// The IPv4 address every major cloud answers instance metadata on.
pub(crate) const METADATA_IPV4: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 254);
/// The IPv6 address AWS answers IMDSv2 on.
pub(crate) const METADATA_IPV6: Ipv6Addr = Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);

/// The IPv4 address an IPv6 one carries, for the three encodings that carry one.
///
/// An IPv4 address written as an IPv6 one is still that address, so a check that reads only
/// the IPv4 form has to decode these first: the mapped form `::ffff:a.b.c.d`, the
/// IPv4-compatible form `::a.b.c.d`, and the 6to4 form `2002:aabb:ccdd::`.
pub(crate) fn embedded_ipv4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return Some(mapped);
    }

    let segments = ip.segments();
    if segments[..6] == [0, 0, 0, 0, 0, 0] {
        return Some(Ipv4Addr::new(
            (segments[6] >> 8) as u8,
            segments[6] as u8,
            (segments[7] >> 8) as u8,
            segments[7] as u8,
        ));
    }

    if segments[0] == 0x2002 {
        return Some(Ipv4Addr::new(
            (segments[1] >> 8) as u8,
            segments[1] as u8,
            (segments[2] >> 8) as u8,
            segments[2] as u8,
        ));
    }

    None
}

/// Reports whether an address is a cloud metadata endpoint.
///
/// This is the rule that holds for every adapter and every configuration. A deployment that
/// allows private addresses still may not reach these, and neither may a command line that
/// is expected to fetch from `localhost`, because no workflow fetches an image from one.
pub(crate) fn is_cloud_metadata_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip == METADATA_IPV4,
        IpAddr::V6(ip) => ip == METADATA_IPV6 || embedded_ipv4(ip) == Some(METADATA_IPV4),
    }
}

/// Reports whether a URL names a well-known cloud metadata service.
///
/// Checked hostnames:
/// - `169.254.169.254` (AWS / Azure / most clouds), in every spelling the `url` crate
///   canonicalizes and in every IPv6 encoding that carries it
/// - `metadata.google.internal` (GCP), with or without the trailing dot that makes a
///   domain name absolute
/// - `[fd00:ec2::254]` (AWS IMDSv2 IPv6)
///
/// A host name that resolves to one of those addresses is outside what a name check can
/// see; [`is_cloud_metadata_ip`] is what catches it, asked of the address the name resolved
/// to.
///
/// Other providers (DigitalOcean, Oracle, and the rest) also answer on `169.254.169.254`
/// and are therefore caught here. Alibaba's `100.100.100.200` falls in the CGNAT range and
/// is the server deny-list's to refuse.
pub(crate) fn is_cloud_metadata_host(url: &Url) -> bool {
    let host = url
        .host_str()
        .map(|host| host.strip_suffix('.').unwrap_or(host));
    if matches!(host, Some("169.254.169.254" | "metadata.google.internal")) {
        return true;
    }

    match url.host() {
        Some(url::Host::Ipv6(addr)) => is_cloud_metadata_ip(IpAddr::V6(addr)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn cloud_metadata_aws_with_various_paths_is_blocked() {
        let url = Url::parse("http://169.254.169.254/latest/api/token").unwrap();
        assert!(is_cloud_metadata_host(&url));
        let url = Url::parse("http://169.254.169.254/latest/user-data").unwrap();
        assert!(is_cloud_metadata_host(&url));
    }

    #[test]
    fn cloud_metadata_non_metadata_ip_is_allowed() {
        let url = Url::parse("http://169.254.169.253/something").unwrap();
        assert!(!is_cloud_metadata_host(&url));
    }

    /// Every spelling that reaches the same metadata endpoint has to be refused by the
    /// same rule, because that rule is the only one that runs when
    /// `TRUSS_ALLOW_INSECURE_URL_SOURCES` is set.
    ///
    /// A trailing dot makes a domain name absolute and resolves identically. The three
    /// IPv6 forms below all carry 169.254.169.254: IPv4-mapped, the deprecated
    /// IPv4-compatible form, and 6to4. The decimal, hexadecimal, and octal IPv4 forms
    /// arrive here already canonicalized by the `url` crate, and are listed so a change
    /// of parser cannot drop them silently.
    #[rstest]
    #[case("http://169.254.169.254/latest/meta-data")]
    #[case("http://169.254.169.254./latest/meta-data")]
    #[case("http://2852039166/latest/meta-data")]
    #[case("http://0xa9fea9fe/latest/meta-data")]
    #[case("http://0251.0376.0251.0376/latest/meta-data")]
    #[case("http://metadata.google.internal/computeMetadata/v1/")]
    #[case("http://metadata.google.internal./computeMetadata/v1/")]
    #[case("http://[::ffff:169.254.169.254]/latest/meta-data")]
    #[case("http://[::169.254.169.254]/latest/meta-data")]
    #[case("http://[2002:a9fe:a9fe::]/latest/meta-data")]
    #[case("http://[fd00:ec2::254]/latest/meta-data")]
    #[case("http://[fd00:0ec2:0:0:0:0:0:0254]/latest/meta-data")]
    fn cloud_metadata_spellings_are_all_blocked(#[case] value: &str) {
        let url = Url::parse(value).expect("parse metadata URL");
        assert!(
            is_cloud_metadata_host(&url),
            "{value} reaches a metadata endpoint and must be refused"
        );
    }

    /// The negative half, so normalizing the trailing dot and decoding the embedded
    /// IPv4 forms does not start refusing hosts that are not metadata endpoints.
    #[rstest]
    #[case("http://metadata.google.internal.example.com/")]
    #[case("http://notmetadata.google.internal/")]
    #[case("http://169.254.169.253/")]
    #[case("http://[::ffff:169.254.169.253]/")]
    #[case("http://[2002:a9fe:a9fd::]/")]
    #[case("http://[fd00:ec2::255]/")]
    fn cloud_metadata_lookalikes_are_not_blocked(#[case] value: &str) {
        let url = Url::parse(value).expect("parse lookalike URL");
        assert!(
            !is_cloud_metadata_host(&url),
            "{value} is not a metadata endpoint"
        );
    }

    #[test]
    fn cloud_metadata_gcp_with_path_is_blocked() {
        let url =
            Url::parse("http://metadata.google.internal/computeMetadata/v1/project/project-id")
                .unwrap();
        assert!(is_cloud_metadata_host(&url));
    }

    #[rstest]
    #[case("169.254.169.254", true)]
    #[case("fd00:ec2::254", true)]
    #[case("::ffff:169.254.169.254", true)]
    #[case("2002:a9fe:a9fe::", true)]
    #[case("127.0.0.1", false)]
    #[case("10.0.0.1", false)]
    #[case("::1", false)]
    fn an_address_is_a_metadata_endpoint_or_is_not(#[case] raw: &str, #[case] expected: bool) {
        let ip: IpAddr = raw.parse().expect("parse address");
        assert_eq!(is_cloud_metadata_ip(ip), expected, "{raw}");
    }
}
