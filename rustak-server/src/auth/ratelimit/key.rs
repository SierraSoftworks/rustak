//! What the limiter counts, and the one keyed hash that turns it into cells.
//!
//! # The address
//!
//! An IPv4 address counts at /32 and an IPv6 address at /64 — the smallest
//! block a network hands one subscriber, so rotating through it buys nothing.
//! An IPv6 address counts again at /48, with a higher allowance, because one
//! hosting customer's /48 is 65 536 /64s. An IPv4-mapped IPv6 address
//! (`::ffff:192.0.2.1`, which a dual-stack listener reports) is the IPv4
//! address it maps. A request whose address the server could not read is a
//! key of its own rather than nobody's.
//!
//! # The subject
//!
//! Folded the way account names compare — trimmed, then lower-cased per
//! character, which is what `Username::parse` does — so `Ada`, `ada` and
//! ` ADA ` are one account to the limiter as they are to the database.
//! Everything is folded, endpoint names and client identifiers too: folding
//! can only merge keys, which makes the limiter stricter, never looser. The
//! fold is streamed into the hash, so a subject of any length costs no
//! allocation; what an administrator is shown is the first [`SHOWN_BYTES`] of
//! it.
//!
//! # The hash
//!
//! SipHash-2-4 — a keyed pseudo-random function — under a 128-bit key drawn
//! from `rand`'s thread generator (a CSPRNG seeded by the operating system)
//! when the limiter is built, never logged, never in a `Debug` dump and never
//! sent anywhere, so nobody outside the process can choose subjects that land
//! on a cell of their choosing, or learn which keys share cells. Each
//! kind of key starts with its own domain byte, so an address counted at /64,
//! the same bytes counted at /48, and a pair can never be one input.

use std::hash::Hasher as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use siphasher::sip::SipHasher24;

/// The longest folded subject an administrator is shown, in bytes.
///
/// Room for any valid username (64 characters of the allowed set); a longer
/// subject is an attacker's junk, and is shown cut short.
pub const SHOWN_BYTES: usize = 96;

/// The prefix lengths an address is counted at.
pub const IPV4_HOST: u8 = 32;
pub const IPV6_HOST: u8 = 64;
pub const IPV6_NETWORK: u8 = 48;

/// Which input a hash is of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Domain {
    /// Tier 1 at /32 or /64.
    Host = 1,
    /// Tier 1 at /48.
    Network = 2,
    /// Tier 2: the address at /32 or /64, and the subject.
    Pair = 3,
}

/// An address cut to the prefix it is counted at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Source {
    network: Option<IpAddr>,
    prefix: u8,
}

impl Source {
    /// The address at /32 or /64, which tier 2 and tier 1's first level use.
    pub fn host(address: Option<IpAddr>) -> Self {
        match address.map(canonical) {
            Some(IpAddr::V4(v4)) => Self::v4(v4),
            Some(IpAddr::V6(v6)) => Self::v6(v6, IPV6_HOST),
            None => Self {
                network: None,
                prefix: 0,
            },
        }
    }

    /// The address at /48, for an IPv6 address only.
    pub fn network(address: Option<IpAddr>) -> Option<Self> {
        match address.map(canonical) {
            Some(IpAddr::V6(v6)) => Some(Self::v6(v6, IPV6_NETWORK)),
            _ => None,
        }
    }

    fn v4(address: Ipv4Addr) -> Self {
        Self {
            network: Some(IpAddr::V4(address)),
            prefix: IPV4_HOST,
        }
    }

    fn v6(address: Ipv6Addr, prefix: u8) -> Self {
        let mut octets = address.octets();
        octets[usize::from(prefix / 8)..].fill(0);

        Self {
            network: Some(IpAddr::V6(Ipv6Addr::from(octets))),
            prefix,
        }
    }

    /// The network address, if there was an address at all.
    pub fn address(&self) -> Option<IpAddr> {
        self.network
    }

    /// The prefix length, if there was an address at all.
    pub fn prefix(&self) -> Option<u8> {
        self.network.map(|_| self.prefix)
    }

    /// Which tier-1 level this is.
    pub fn domain(&self) -> Domain {
        match self.prefix {
            IPV6_NETWORK => Domain::Network,
            _ => Domain::Host,
        }
    }

    /// `198.51.100.4/32`, `2001:db8::/48`, or `unknown`.
    pub fn shown(&self) -> String {
        match self.network {
            Some(network) => format!("{network}/{}", self.prefix),
            None => "unknown".to_string(),
        }
    }

    /// [`Source::shown`] backwards; [`None`] for anything it could not have
    /// produced.
    pub fn parse_shown(shown: &str) -> Option<Self> {
        if shown == "unknown" {
            return Some(Self::host(None));
        }

        let (address, prefix) = shown.split_once('/')?;
        let address: IpAddr = address.parse().ok()?;
        let source = match (canonical(address), prefix.parse::<u8>().ok()?) {
            (IpAddr::V4(v4), IPV4_HOST) => Self::v4(v4),
            (IpAddr::V6(v6), prefix @ (IPV6_HOST | IPV6_NETWORK)) => Self::v6(v6, prefix),
            _ => return None,
        };

        (source.network == Some(address)).then_some(source)
    }

    fn write(&self, hasher: &mut SipHasher24) {
        match self.network {
            Some(IpAddr::V4(v4)) => {
                hasher.write_u8(4);
                hasher.write(&v4.octets());
            }
            Some(IpAddr::V6(v6)) => {
                hasher.write_u8(6);
                hasher.write(&v6.octets());
            }
            None => hasher.write_u8(0),
        }
        hasher.write_u8(self.prefix);
    }
}

/// An IPv4-mapped IPv6 address as the IPv4 address it is.
fn canonical(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address, IpAddr::V4),
        v4 => v4,
    }
}

/// Feeds the folded subject to `each`, a character at a time.
fn fold(subject: &str, mut each: impl FnMut(&str)) {
    let mut buffer = [0u8; 4];

    for character in subject.trim().chars().flat_map(char::to_lowercase) {
        each(character.encode_utf8(&mut buffer));
    }
}

/// The start of a folded subject, kept without allocating.
#[derive(Clone, Copy)]
pub(super) struct ShownKey {
    bytes: [u8; SHOWN_BYTES],
    len: usize,
    cut: bool,
}

impl ShownKey {
    /// Folds `subject` and keeps what fits, whole characters only.
    pub fn of(subject: &str) -> Self {
        let mut shown = Self {
            bytes: [0; SHOWN_BYTES],
            len: 0,
            cut: false,
        };

        fold(subject, |character| {
            let end = shown.len + character.len();
            if shown.cut || end > SHOWN_BYTES {
                shown.cut = true;
                return;
            }

            shown.bytes[shown.len..end].copy_from_slice(character.as_bytes());
            shown.len = end;
        });

        shown
    }

    /// The folded subject, or as much of it as fitted.
    pub fn as_str(&self) -> &str {
        // Only ever filled with whole UTF-8 characters.
        std::str::from_utf8(&self.bytes[..self.len]).unwrap_or_default()
    }

    /// Whether the subject was longer than what is kept.
    pub fn is_cut(&self) -> bool {
        self.cut
    }
}

/// The process's hash key.
pub(super) struct KeyedHash {
    k0: u64,
    k1: u64,
}

impl KeyedHash {
    /// A fresh key from the thread's CSPRNG.
    pub fn random() -> Self {
        Self {
            k0: rand::random(),
            k1: rand::random(),
        }
    }

    /// A key the caller chooses, so a test or a simulation is repeatable.
    #[cfg(test)]
    pub fn fixed(k0: u64, k1: u64) -> Self {
        Self { k0, k1 }
    }

    fn start(&self, domain: Domain) -> SipHasher24 {
        let mut hasher = SipHasher24::new_with_keys(self.k0, self.k1);
        hasher.write_u8(domain as u8);
        hasher
    }

    /// A tier-1 key: the address at the level `source` was cut to.
    pub fn source(&self, source: &Source) -> u64 {
        let mut hasher = self.start(source.domain());
        source.write(&mut hasher);
        hasher.finish()
    }

    /// A tier-2 key: the address at /32 or /64 and the folded subject.
    ///
    /// The subject goes last and the address is fixed-length per family, so
    /// no two pairs feed the hash the same bytes.
    pub fn pair(&self, source: &Source, subject: &str) -> u64 {
        let mut hasher = self.start(Domain::Pair);
        source.write(&mut hasher);
        fold(subject, |character| hasher.write(character.as_bytes()));
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> Option<IpAddr> {
        Some(text.parse().unwrap())
    }

    #[test]
    fn an_ipv6_address_is_counted_at_its_64_and_its_48() {
        let host = Source::host(ip("2001:db8:1:2:aaaa:bbbb:cccc:dddd"));
        let network = Source::network(ip("2001:db8:1:2:aaaa:bbbb:cccc:dddd")).unwrap();

        assert_eq!(host.shown(), "2001:db8:1:2::/64");
        assert_eq!(network.shown(), "2001:db8:1::/48");
        assert_eq!(host, Source::host(ip("2001:db8:1:2::1")));
        assert_ne!(host, Source::host(ip("2001:db8:1:3::1")));
        assert_eq!(network, Source::network(ip("2001:db8:1:ffff::1")).unwrap());
    }

    #[test]
    fn an_ipv4_mapped_address_is_the_ipv4_address() {
        assert_eq!(
            Source::host(ip("::ffff:198.51.100.4")),
            Source::host(ip("198.51.100.4"))
        );
        assert_eq!(Source::network(ip("::ffff:198.51.100.4")), None);
        assert_eq!(Source::host(ip("198.51.100.4")).shown(), "198.51.100.4/32");
    }

    #[test]
    fn no_address_is_a_key_of_its_own() {
        let keyed = KeyedHash::fixed(1, 2);
        let unknown = Source::host(None);

        assert_eq!(unknown.shown(), "unknown");
        assert_ne!(
            keyed.source(&unknown),
            keyed.source(&Source::host(ip("0.0.0.0")))
        );
        assert_ne!(
            keyed.source(&unknown),
            keyed.source(&Source::host(ip("::")))
        );
    }

    #[test]
    fn what_is_shown_parses_back_and_nothing_else_does() {
        for text in ["198.51.100.4", "2001:db8:1:2::9", "::ffff:192.0.2.1"] {
            let host = Source::host(ip(text));
            assert_eq!(Source::parse_shown(&host.shown()), Some(host));

            if let Some(network) = Source::network(ip(text)) {
                assert_eq!(Source::parse_shown(&network.shown()), Some(network));
            }
        }
        assert_eq!(Source::parse_shown("unknown"), Some(Source::host(None)));

        for refused in [
            "198.51.100.4/24",
            "2001:db8::/56",
            "2001:db8::1/64",
            "ada",
            "198.51.100.4",
        ] {
            assert_eq!(Source::parse_shown(refused), None, "{refused}");
        }
    }

    #[test]
    fn subjects_are_folded_the_way_account_names_compare() {
        let keyed = KeyedHash::fixed(1, 2);
        let host = Source::host(ip("198.51.100.4"));
        let ada = keyed.pair(&host, "ada");

        assert_eq!(keyed.pair(&host, "Ada"), ada);
        assert_eq!(keyed.pair(&host, "  ADA "), ada);
        assert_ne!(keyed.pair(&host, "grace"), ada);
        assert_eq!(ShownKey::of(" ÉLODIE ").as_str(), "élodie");
    }

    #[test]
    fn the_same_bytes_in_another_domain_are_another_key() {
        let keyed = KeyedHash::fixed(1, 2);
        let host = Source::host(ip("198.51.100.4"));

        assert_ne!(keyed.source(&host), keyed.pair(&host, ""));
        assert_ne!(
            keyed.source(&Source::host(ip("2001:db8::"))),
            keyed.source(&Source::network(ip("2001:db8::")).unwrap()),
        );
    }

    #[test]
    fn the_hash_depends_on_the_key() {
        let host = Source::host(ip("198.51.100.4"));

        assert_ne!(
            KeyedHash::fixed(1, 2).pair(&host, "ada"),
            KeyedHash::fixed(1, 3).pair(&host, "ada"),
        );
    }

    #[test]
    fn a_long_subject_is_shown_cut_at_a_character() {
        let long = "é".repeat(SHOWN_BYTES);
        let shown = ShownKey::of(&long);

        assert!(shown.is_cut());
        assert_eq!(shown.as_str().len(), SHOWN_BYTES);
        assert!(!ShownKey::of("ada").is_cut());
    }
}
