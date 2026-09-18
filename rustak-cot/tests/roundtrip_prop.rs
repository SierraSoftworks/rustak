//! Randomised round-trip properties for the XML codec.
//!
//! The generator is a deterministic SplitMix64 rather than a property-testing
//! framework, so a failure is reproducible from the seed printed in the panic
//! message and the crate keeps its dependency-light leaf position.
//!
//! The properties are:
//!
//! 1. `parse(write(event)) == event` for every event the generator can build;
//! 2. writing is idempotent, so relaying a message is byte-stable;
//! 3. `parse` never panics, whatever bytes it is handed.

use std::time::Duration;

use rustak_cot::detail::{Detail, Element, Node};
use rustak_cot::{CotTime, Event, Point, xml};

/// Deterministic, tiny, and good enough to shake out structural bugs.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_add(0x9E37_79B9_7F4A_7C15))
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next_u64() % bound as u64) as usize
    }

    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }

    fn pick<'a, T>(&mut self, options: &'a [T]) -> &'a T {
        &options[self.below(options.len())]
    }
}

/// Values that exercise escaping, Unicode and emptiness without tripping the
/// documented normalisations (no control characters, no `]]>`, no `--`).
const VALUES: &[&str] = &[
    "",
    "ALPHA",
    "a & b",
    "\"quoted\"",
    "<angle>",
    "it's",
    "CHARLIE 🛰️ ÅÄÖ",
    "*:-1:stcp",
    "9999999.0",
    "line one line two",
    "&amp;",
    "  padded  ",
];

const NAMES: &[&str] = &[
    "contact",
    "__group",
    "takv",
    "track",
    "status",
    "precisionlocation",
    "marti",
    "dest",
    "remarks",
    "__chat",
    "chatgrp",
    "link",
    "_flow-tags_",
    "vendorElement",
    "x",
];

const ATTR_NAMES: &[&str] = &[
    "callsign",
    "endpoint",
    "name",
    "role",
    "uid",
    "type",
    "speed",
    "course",
    "battery",
    "source",
    "TAK-Server-rustak",
    "vendor-attr",
];

fn event(rng: &mut Rng) -> Event {
    let time = CotTime::from_millis(1_700_000_000_000 + (rng.below(100_000_000) as i64));
    let mut event = Event::builder(*rng.pick(VALUES), *rng.pick(VALUES))
        .time(time)
        .stale_after(Duration::from_secs(rng.below(600) as u64))
        .point_full(point(rng))
        .detail(detail(rng, 0))
        .build();

    event.version = (*rng.pick(VALUES)).to_owned();
    event.how = optional(rng);
    event.access = optional(rng);
    event.qos = optional(rng);
    event.opex = optional(rng);
    event.caveat = optional(rng);
    event.releasable_to = optional(rng);
    for index in 0..rng.below(3) {
        event
            .extra_attrs
            .push((format!("extra{index}"), (*rng.pick(VALUES)).to_owned()));
    }
    event
}

fn optional(rng: &mut Rng) -> Option<String> {
    rng.chance(3).then(|| (*rng.pick(VALUES)).to_owned())
}

fn point(rng: &mut Rng) -> Point {
    // A spread of magnitudes, all finite and exactly representable.
    let coordinate = |rng: &mut Rng| (rng.next_u64() % 360_000_000) as f64 / 1_000_000.0 - 180.0;
    Point {
        lat: coordinate(rng),
        lon: coordinate(rng),
        hae: *rng.pick(&[0.0, 35.5, -12.25, Point::UNKNOWN_HAE]),
        ce: *rng.pick(&[0.0, 9.5, Point::UNKNOWN_CE]),
        le: *rng.pick(&[0.0, 1.0, Point::UNKNOWN_LE]),
    }
}

fn detail(rng: &mut Rng, depth: usize) -> Detail {
    Detail {
        nodes: nodes(rng, depth),
    }
}

/// Builds a node list that avoids the two documented merge hazards: a run of
/// whitespace-only character data is dropped, and two adjacent text nodes
/// become one.
fn nodes(rng: &mut Rng, depth: usize) -> Vec<Node> {
    let mut nodes: Vec<Node> = Vec::new();
    for _ in 0..rng.below(4) {
        let previous_was_text = matches!(nodes.last(), Some(Node::Text(_)));
        let node = match rng.below(8) {
            0 if !previous_was_text => Node::Text(text(rng)),
            1 => Node::CData(text(rng)),
            2 => Node::Comment(comment(rng)),
            _ => Node::Element(element(rng, depth)),
        };
        nodes.push(node);
    }
    nodes
}

fn text(rng: &mut Rng) -> String {
    // Never whitespace-only: such a run is insignificant and is dropped.
    let candidate = *rng.pick(VALUES);
    if candidate.trim().is_empty() {
        "content".to_owned()
    } else {
        candidate.to_owned()
    }
}

fn comment(rng: &mut Rng) -> String {
    // `--` is illegal inside a comment and a trailing `-` closes it early.
    let candidate = text(rng).replace("--", "-");
    format!(" {} ", candidate.trim_end_matches('-'))
}

fn element(rng: &mut Rng, depth: usize) -> Element {
    let mut element = Element::new(*rng.pick(NAMES));
    let mut used: Vec<&str> = Vec::new();
    for _ in 0..rng.below(4) {
        let name = *rng.pick(ATTR_NAMES);
        if used.contains(&name) {
            continue;
        }
        used.push(name);
        element
            .attrs
            .push((name.to_owned(), (*rng.pick(VALUES)).to_owned()));
    }
    if depth < 4 {
        element.children = nodes(rng, depth + 1);
    }
    element
}

#[test]
fn every_generated_event_survives_a_write_and_parse() {
    for seed in 0..2_000_u64 {
        let mut rng = Rng::new(seed);
        let original = event(&mut rng);
        let written = xml::write(&original);
        let parsed = match xml::parse(&written) {
            Ok(parsed) => parsed,
            Err(err) => panic!(
                "seed {seed}: our own output failed to parse: {err}\n{}",
                String::from_utf8_lossy(&written)
            ),
        };
        assert_eq!(
            parsed,
            original,
            "seed {seed}: round trip lost meaning\n{}",
            String::from_utf8_lossy(&written)
        );
        assert_eq!(
            xml::write(&parsed),
            written,
            "seed {seed}: writing is not idempotent"
        );
    }
}

#[test]
fn every_generated_event_obeys_the_outbound_contract() {
    for seed in 0..500_u64 {
        let mut rng = Rng::new(seed ^ 0xDEAD_BEEF);
        let written = xml::write(&event(&mut rng));
        let text = std::str::from_utf8(&written).expect("output is UTF-8");
        assert!(
            text.starts_with(&format!("{}\n<event ", xml::DECLARATION)),
            "seed {seed}"
        );
        assert!(text.ends_with("</event>"), "seed {seed}");
        assert!(!text.contains("<event/>"), "seed {seed}");
        assert!(
            !text[xml::DECLARATION.len() + 1..]
                .chars()
                .any(|c| matches!(c, '\u{0}'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'..='\u{9f}')),
            "seed {seed}: control characters reached the wire"
        );
    }
}

#[test]
fn parsing_arbitrary_bytes_never_panics() {
    let seeds: &[&[u8]] = &[
        b"",
        b"<",
        b"<event",
        b"<event>",
        b"</event>",
        b"<event><point lat=",
        b"<?xml?><event uid='a'><point lat='0' lon='0'/></event>",
        b"\xff\xfe\x00<event/>",
        b"<event><detail><a><b><c/></b></a></detail></event>",
        "<event uid='\u{0}\u{1f}'/>".as_bytes(),
    ];

    for seed in seeds {
        let _ = xml::parse(seed);
    }

    let alphabet = b"<>/=\"'& \tevntpoiadl0123456789\xc3\xa9";
    for seed in 0..3_000_u64 {
        let mut rng = Rng::new(seed ^ 0x5EED);
        let length = rng.below(80);
        let bytes: Vec<u8> = (0..length)
            .map(|_| alphabet[rng.below(alphabet.len())])
            .collect();
        let _ = xml::parse(&bytes);
    }
}

#[test]
fn fragments_round_trip_independently_of_the_envelope() {
    for seed in 0..500_u64 {
        let mut rng = Rng::new(seed ^ 0xF00D);
        let original = detail(&mut rng, 0);
        let fragment = xml::write_fragment(&original.nodes);
        let parsed = xml::parse_fragment(&fragment)
            .unwrap_or_else(|err| panic!("seed {seed}: {err} in {fragment}"));
        // A fragment that is only insignificant whitespace legitimately
        // disappears; anything else must survive intact.
        if !fragment.trim().is_empty() {
            assert_eq!(parsed, original.nodes, "seed {seed}: {fragment}");
        }
    }
}
