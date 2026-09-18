//! Randomised round-trip properties for the XML and protobuf codecs.
//!
//! The XML half uses a deterministic SplitMix64 generator, so a failure is
//! reproducible from the seed printed in the panic message. Its properties:
//!
//! 1. `parse(write(event)) == event` for every event the generator can build;
//! 2. writing is idempotent, so relaying a message is byte-stable;
//! 3. `parse` never panics, whatever bytes it is handed.
//!
//! The protobuf half (module `proto_conversion` at the bottom) uses proptest,
//! because the interesting inputs there are shapes — an element with one
//! attribute too many, a duplicate, a bad number — and shrinking a failing
//! shape is worth the dependency. Its properties are the conversion's fixed
//! point, the wire round trip, and the exact list of documented losses.

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

/// Protobuf conversion properties, generated with proptest.
///
/// The three properties together pin the contract without re-implementing the
/// converter in the test:
///
/// * **fixed point** — one conversion normalises an event (drops what the
///   encoding cannot carry, reorders detail children); a second conversion
///   changes nothing more. That is what makes relaying idempotent.
/// * **wire round trip** — the encoded bytes decode back to the same message.
/// * **documented losses** — everything except `extra_attrs`, `version` and
///   detail child order survives, and is asserted field by field.
mod proto_conversion {
    use proptest::prelude::*;
    use rustak_cot::detail::{Detail, Element, Node};
    use rustak_cot::{CotTime, Event, Point, proto, xml};

    /// Values that exercise escaping and Unicode without tripping a
    /// documented XML normalisation.
    const VALUES: &[&str] = &[
        "",
        "ALPHA",
        "a & b",
        "\"quoted\"",
        "<angle>",
        "CHARLIE 🛰️",
        "*:-1:stcp",
        "1.5",
        "87",
        "full",
        "  padded  ",
    ];

    /// Deliberately a mix of the six promotable names and unmodelled ones, so
    /// that most generated details contain both promoted and leftover nodes.
    const ELEMENT_NAMES: &[&str] = &[
        "contact",
        "__group",
        "takv",
        "track",
        "status",
        "precisionlocation",
        "uid",
        "_flow-tags_",
        "remarks",
    ];

    /// The union of every promotable attribute set plus two that belong to no
    /// sub-message, so the strict attribute-set rule is hit from both sides.
    const ATTR_NAMES: &[&str] = &[
        "callsign",
        "endpoint",
        "name",
        "role",
        "geopointsrc",
        "altsrc",
        "battery",
        "speed",
        "course",
        "device",
        "platform",
        "os",
        "version",
        "Droid",
    ];

    /// Character data that survives verbatim: never whitespace-only (such a
    /// run is insignificant), never containing `--` or `]]>`.
    const TEXT: &[&str] = &["content", "ALPHA", "a & b", "🛰️"];

    fn value() -> impl Strategy<Value = String> {
        proptest::sample::select(VALUES).prop_map(str::to_owned)
    }

    fn childless_element() -> impl Strategy<Value = Element> {
        (
            proptest::sample::select(ELEMENT_NAMES),
            proptest::collection::vec((proptest::sample::select(ATTR_NAMES), value()), 0..5),
        )
            .prop_map(|(name, attrs)| {
                let mut element = Element::new(name);
                for (key, value) in attrs {
                    // `set` replaces rather than duplicating, so the element
                    // never claims the same attribute name twice.
                    element.set(key, value);
                }
                element
            })
    }

    fn element() -> impl Strategy<Value = Element> {
        (
            childless_element(),
            proptest::collection::vec(childless_element(), 0..2),
        )
            .prop_map(|(mut parent, children)| {
                for child in children {
                    parent.push(child);
                }
                parent
            })
    }

    fn node() -> impl Strategy<Value = Node> {
        let text = proptest::sample::select(TEXT).prop_map(str::to_owned);
        prop_oneof![
            6 => element().prop_map(Node::Element),
            1 => text.clone().prop_map(Node::Text),
            1 => text.clone().prop_map(Node::CData),
            1 => text.prop_map(|text| Node::Comment(format!(" {text} "))),
        ]
    }

    fn detail() -> impl Strategy<Value = Detail> {
        proptest::collection::vec(node(), 0..5).prop_map(|nodes| {
            // Two adjacent text nodes come back as one, which is a parser
            // normalisation rather than anything the converter does.
            let mut kept: Vec<Node> = Vec::new();
            for node in nodes {
                let both_text =
                    matches!(kept.last(), Some(Node::Text(_))) && matches!(node, Node::Text(_));
                if !both_text {
                    kept.push(node);
                }
            }
            Detail { nodes: kept }
        })
    }

    fn point() -> impl Strategy<Value = Point> {
        (
            -90.0f64..90.0,
            -180.0f64..180.0,
            prop_oneof![Just(0.0), Just(35.5), Just(Point::UNKNOWN_HAE)],
            prop_oneof![Just(0.0), Just(9.5), Just(Point::UNKNOWN_CE)],
            prop_oneof![Just(0.0), Just(1.0), Just(Point::UNKNOWN_LE)],
        )
            .prop_map(|(lat, lon, hae, ce, le)| Point {
                lat,
                lon,
                hae,
                ce,
                le,
            })
    }

    type Markings = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );

    fn markings() -> impl Strategy<Value = Markings> {
        (
            proptest::option::of(value()),
            proptest::option::of(value()),
            proptest::option::of(value()),
            proptest::option::of(value()),
            proptest::option::of(value()),
        )
    }

    fn event() -> impl Strategy<Value = Event> {
        (
            proptest::sample::select(VALUES),
            proptest::sample::select(VALUES),
            0i64..2_000_000_000_000,
            0i64..600_000,
            markings(),
            point(),
            detail(),
            proptest::collection::vec(
                (any::<u32>(), proptest::collection::vec(any::<u8>(), 0..8)),
                0..2,
            ),
            proptest::collection::vec((value(), value()), 0..2),
        )
            .prop_map(
                |(r#type, uid, time, stale, markings, point, detail, extensions, extras)| {
                    let (how, access, qos, opex, caveat) = markings;
                    let time = CotTime::from_millis(time);
                    Event {
                        version: "9.9".to_owned(),
                        uid: uid.to_owned(),
                        r#type: r#type.to_owned(),
                        how,
                        time,
                        start: time,
                        stale: CotTime::from_millis(time.millis() + stale),
                        access,
                        qos,
                        opex,
                        caveat,
                        releasable_to: None,
                        extra_attrs: extras
                            .into_iter()
                            .enumerate()
                            .map(|(index, (_, value))| (format!("extra{index}"), value))
                            .collect(),
                        point,
                        detail,
                        proto_extensions: extensions
                            .into_iter()
                            .map(|(id, data)| (id, bytes::Bytes::from(data)))
                            .collect(),
                    }
                },
            )
    }

    /// Every top-level element name and how many times it appears.
    fn element_names(detail: &Detail) -> Vec<String> {
        let mut names: Vec<String> = detail
            .elements()
            .map(|element| element.name.clone())
            .collect();
        names.sort();
        names
    }

    proptest! {
        /// One conversion normalises; a second changes nothing more.
        #[test]
        fn the_conversion_reaches_a_fixed_point(event in event()) {
            let first = proto::event_to_message(&event);
            let normalised = proto::message_to_event(first.clone())
                .expect("our own message always carries a cotEvent");
            let second = proto::event_to_message(&normalised);

            prop_assert_eq!(&first, &second, "a second encode changed the message");
            prop_assert_eq!(
                proto::message_to_event(second).expect("cotEvent"),
                normalised,
                "a second decode changed the event"
            );
        }

        /// The encoded frame payload decodes back to the same message.
        #[test]
        fn the_wire_bytes_round_trip(event in event()) {
            let message = proto::event_to_message(&event);
            let decoded = proto::decode(&proto::encode(&message))
                .expect("our own bytes must decode");
            prop_assert_eq!(decoded, message);
        }

        /// The losses are exactly the two the module documents: `extra_attrs`
        /// and `version` go, detail children are reordered. Nothing else.
        #[test]
        fn only_the_documented_losses_happen(event in event()) {
            let round_tripped = proto::message_to_event(proto::event_to_message(&event))
                .expect("cotEvent");

            prop_assert_eq!(&round_tripped.uid, &event.uid);
            prop_assert_eq!(&round_tripped.r#type, &event.r#type);
            prop_assert_eq!(round_tripped.time, event.time);
            prop_assert_eq!(round_tripped.start, event.start);
            prop_assert_eq!(round_tripped.stale, event.stale);
            prop_assert_eq!(round_tripped.point, event.point);
            prop_assert_eq!(&round_tripped.proto_extensions, &event.proto_extensions);

            // An empty string and an absent marking are the same thing on the
            // wire, so both decode as absent.
            let blank_is_absent =
                |value: &Option<String>| value.clone().filter(|value| !value.is_empty());
            prop_assert_eq!(round_tripped.how, blank_is_absent(&event.how));
            prop_assert_eq!(round_tripped.access, blank_is_absent(&event.access));
            prop_assert_eq!(round_tripped.qos, blank_is_absent(&event.qos));
            prop_assert_eq!(round_tripped.opex, blank_is_absent(&event.opex));
            prop_assert_eq!(round_tripped.caveat, blank_is_absent(&event.caveat));

            prop_assert_eq!(
                element_names(&round_tripped.detail),
                element_names(&event.detail),
                "detail order may change, but no element may appear or vanish"
            );

            prop_assert_eq!(round_tripped.version, "2.0");
            prop_assert!(round_tripped.extra_attrs.is_empty());
        }

        /// A hostile peer must never be able to panic the decoder.
        #[test]
        fn decoding_arbitrary_bytes_never_panics(
            bytes in proptest::collection::vec(any::<u8>(), 0..96)
        ) {
            if let Ok(message) = proto::decode(&bytes) {
                let _ = proto::message_to_event(message);
            }
        }
    }

    /// The exact bytes the situational-awareness fixture encodes to.
    ///
    /// Field numbers and the strict promotion rules are the two things a
    /// refactor can silently change without breaking a single behavioural
    /// test, so they are pinned here as hex. A deliberate change means
    /// re-deriving this constant *and* re-checking it against a real client.
    const SA_FULL_HEX: &str = concat!(
        "12b6020a09612d662d472d552d432a14414e44524f49442d72757374616b2d61",
        "6c7068613080d4f3f98a343880d4f3f98a3440e0a8f7f98a344a036d2d6751c5",
        "feb27bf2c0494059ebe2361ac05bc0bf61000000000080414069000000e0cf12",
        "634171000000e0cf1263417acb010a543c7569642044726f69643d22414c5048",
        "41222f3e3c5f666c6f772d746167735f2054414b2d5365727665722d72757374",
        "616b2d746573743d22323032362d30392d31375431323a30303a30302e303030",
        "5a222f3e12120a092a3a2d313a737463701205414c5048411a130a044379616e",
        "120b5465616d204d656d626572220a0a0347505312034750532a02085732260a",
        "0c546573742048616e64736574120b72757374616b2d746573741a0233342205",
        "302e312e303a1209000000000000f83f110000000000e07040",
    );

    #[test]
    fn the_situational_awareness_fixture_encodes_to_the_pinned_bytes() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sa-full.xml");
        let event = xml::parse(&std::fs::read(path).expect("read fixture")).expect("parse fixture");

        let encoded = proto::encode(&proto::event_to_message(&event));
        let hex: String = encoded.iter().map(|byte| format!("{byte:02x}")).collect();

        assert_eq!(
            hex, SA_FULL_HEX,
            "the protobuf encoding of sa-full changed; re-derive the constant \
             from the left-hand value only if the change was deliberate"
        );

        // And the pinned bytes still mean what the fixture said.
        let decoded =
            proto::message_to_event(proto::decode(&encoded).expect("decode")).expect("convert");
        assert_eq!(decoded.callsign(), Some("ALPHA"));
        assert_eq!(decoded.uid, "ANDROID-rustak-alpha");
        assert_eq!(
            decoded.detail.count("uid"),
            1,
            "the unmodelled child survives"
        );
        assert_eq!(decoded.detail.count("_flow-tags_"), 1);
    }
}
