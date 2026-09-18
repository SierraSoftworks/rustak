//! The CoT event: the single message type that crosses a TAK stream.

use std::time::Duration;

use bytes::Bytes;

use crate::detail::{Contact, Detail, Element, Group, Takv, TypedDetail};
use crate::time::CotTime;
use crate::types::is_control_type;

/// The `version` attribute every client emits and expects.
pub const VERSION: &str = "2.0";

/// The `<point>` of an event: where the thing is, and how well we know.
///
/// CoT has no "unknown" encoding, so a sentinel of `9999999.0` stands in for
/// an unavailable altitude or error estimate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// Latitude in decimal degrees, WGS-84.
    pub lat: f64,
    /// Longitude in decimal degrees, WGS-84.
    pub lon: f64,
    /// Height above the WGS-84 ellipsoid, metres.
    pub hae: f64,
    /// Circular (horizontal) error, metres.
    pub ce: f64,
    /// Linear (vertical) error, metres.
    pub le: f64,
}

impl Point {
    /// The sentinel for an unknown height above ellipsoid.
    pub const UNKNOWN_HAE: f64 = 9_999_999.0;
    /// The sentinel for an unknown circular error.
    pub const UNKNOWN_CE: f64 = 9_999_999.0;
    /// The sentinel for an unknown linear error.
    pub const UNKNOWN_LE: f64 = 9_999_999.0;

    /// A position with unknown altitude and error estimates.
    #[must_use]
    pub const fn new(lat: f64, lon: f64) -> Self {
        Self {
            lat,
            lon,
            hae: Self::UNKNOWN_HAE,
            ce: Self::UNKNOWN_CE,
            le: Self::UNKNOWN_LE,
        }
    }

    /// The null island point server-generated messages carry: `0/0`, zero
    /// altitude, unknown error.
    ///
    /// Control messages must have a point at all — TAK Server drops anything
    /// without `<point lat>` before it reaches a filter — so this is what
    /// pings, pongs and notifications use.
    #[must_use]
    pub const fn zero() -> Self {
        Self {
            lat: 0.0,
            lon: 0.0,
            hae: 0.0,
            ce: Self::UNKNOWN_CE,
            le: Self::UNKNOWN_LE,
        }
    }
}

impl Default for Point {
    fn default() -> Self {
        Self::zero()
    }
}

/// A Cursor-on-Target event.
///
/// Attribute coverage is deliberately complete: the five attributes rustak
/// does not model are kept verbatim in [`extra_attrs`](Event::extra_attrs) so
/// that relaying an event never drops what a client wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// The `version` attribute, normally [`VERSION`].
    pub version: String,
    /// Globally unique id of the object this event describes.
    pub uid: String,
    /// CoT type, e.g. `a-f-G-U-C`.
    pub r#type: String,
    /// How the position was obtained, e.g. `m-g`.
    pub how: Option<String>,
    /// When the event was produced.
    pub time: CotTime,
    /// When the event becomes valid.
    pub start: CotTime,
    /// When the event expires.
    pub stale: CotTime,
    /// Access control marking.
    pub access: Option<String>,
    /// Quality of service marking.
    pub qos: Option<String>,
    /// Exercise / operation / simulation marking.
    pub opex: Option<String>,
    /// Handling caveat.
    pub caveat: Option<String>,
    /// Releasability marking.
    pub releasable_to: Option<String>,
    /// `<event>` attributes rustak does not model, in document order.
    ///
    /// These survive XML relay but are **lost** in the protobuf encoding,
    /// which has no field for them.
    pub extra_attrs: Vec<(String, String)>,
    /// Where the object is.
    pub point: Point,
    /// Everything else the sender attached.
    pub detail: Detail,
    /// Opaque protobuf `extensionDetails`, passed through untouched.
    ///
    /// Only ever populated by the protobuf decoder; the XML encoding has no
    /// representation for them, so they are dropped when an event is written
    /// as XML.
    pub proto_extensions: Vec<(u32, Bytes)>,
}

impl Default for Event {
    fn default() -> Self {
        let now = CotTime::now();
        Self {
            version: VERSION.to_owned(),
            uid: String::new(),
            r#type: String::new(),
            how: None,
            time: now,
            start: now,
            stale: now,
            access: None,
            qos: None,
            opex: None,
            caveat: None,
            releasable_to: None,
            extra_attrs: Vec::new(),
            point: Point::zero(),
            detail: Detail::new(),
            proto_extensions: Vec::new(),
        }
    }
}

impl Event {
    /// Starts building an event of this type and uid, timed to now.
    #[must_use]
    pub fn builder(r#type: impl Into<String>, uid: impl Into<String>) -> EventBuilder {
        EventBuilder {
            event: Event {
                r#type: r#type.into(),
                uid: uid.into(),
                ..Event::default()
            },
        }
    }

    /// The sender's callsign, if they gave a non-empty one.
    #[must_use]
    pub fn callsign(&self) -> Option<&str> {
        self.contact_attr("callsign")
    }

    /// The sender's reply endpoint, if they gave a non-empty one.
    #[must_use]
    pub fn endpoint(&self) -> Option<&str> {
        self.contact_attr("endpoint")
    }

    /// A non-empty attribute of the first `<contact>` element.
    fn contact_attr(&self, name: &str) -> Option<&str> {
        self.detail
            .find(Contact::NAME)
            .and_then(|element| element.get(name))
            .filter(|value| !value.is_empty())
    }

    /// Whether this is a situational-awareness message.
    ///
    /// TAK Server's definition: a non-empty callsign, a non-empty contact
    /// endpoint and a non-empty uid. Only these refresh the latest-SA cache.
    #[must_use]
    pub fn is_sa(&self) -> bool {
        !self.uid.is_empty() && self.callsign().is_some() && self.endpoint().is_some()
    }

    /// Whether the server consumes this event instead of relaying it.
    #[must_use]
    pub fn is_control(&self) -> bool {
        is_control_type(&self.r#type)
    }

    /// The `<contact>` view, if present.
    #[must_use]
    pub fn contact(&self) -> Option<Contact> {
        self.detail.get::<Contact>()
    }

    /// The `<__group>` view, if present.
    #[must_use]
    pub fn group(&self) -> Option<Group> {
        self.detail.get::<Group>()
    }

    /// The `<takv>` view, if present.
    #[must_use]
    pub fn takv(&self) -> Option<Takv> {
        self.detail.get::<Takv>()
    }

    /// Whether the event has expired at `now`.
    #[must_use]
    pub fn is_stale_at(&self, now: CotTime) -> bool {
        self.stale <= now
    }
}

/// Fluent constructor for [`Event`].
///
/// `time` and `start` default to the moment [`Event::builder`] was called and
/// `stale` to the same instant, so a caller that forgets
/// [`stale_after`](EventBuilder::stale_after) produces an already-expired
/// event rather than an immortal one.
#[derive(Clone, Debug)]
pub struct EventBuilder {
    event: Event,
}

impl EventBuilder {
    /// Sets `how`.
    #[must_use]
    pub fn how(mut self, how: impl Into<String>) -> Self {
        self.event.how = Some(how.into());
        self
    }

    /// Sets the position, leaving altitude and error estimates unknown.
    #[must_use]
    pub fn point(mut self, lat: f64, lon: f64) -> Self {
        self.event.point = Point::new(lat, lon);
        self
    }

    /// Sets the whole point.
    #[must_use]
    pub fn point_full(mut self, point: Point) -> Self {
        self.event.point = point;
        self
    }

    /// Sets `time` and `start` together, preserving the staleness interval.
    #[must_use]
    pub fn time(mut self, time: CotTime) -> Self {
        let validity = self.event.stale - self.event.time;
        self.event.time = time;
        self.event.start = time;
        self.event.stale = CotTime::from_millis(time.millis().saturating_add(validity));
        self
    }

    /// Sets `start` alone.
    #[must_use]
    pub fn start(mut self, start: CotTime) -> Self {
        self.event.start = start;
        self
    }

    /// Sets `stale` to an absolute instant.
    #[must_use]
    pub fn stale(mut self, stale: CotTime) -> Self {
        self.event.stale = stale;
        self
    }

    /// Sets `stale` to `start + after`.
    #[must_use]
    pub fn stale_after(mut self, after: Duration) -> Self {
        self.event.stale = self.event.start.stale_after(after);
        self
    }

    /// Sets `access`.
    #[must_use]
    pub fn access(mut self, access: impl Into<String>) -> Self {
        self.event.access = Some(access.into());
        self
    }

    /// Sets `qos`.
    #[must_use]
    pub fn qos(mut self, qos: impl Into<String>) -> Self {
        self.event.qos = Some(qos.into());
        self
    }

    /// Sets `opex`.
    #[must_use]
    pub fn opex(mut self, opex: impl Into<String>) -> Self {
        self.event.opex = Some(opex.into());
        self
    }

    /// Sets `caveat`.
    #[must_use]
    pub fn caveat(mut self, caveat: impl Into<String>) -> Self {
        self.event.caveat = Some(caveat.into());
        self
    }

    /// Sets `releasableTo`.
    #[must_use]
    pub fn releasable_to(mut self, releasable_to: impl Into<String>) -> Self {
        self.event.releasable_to = Some(releasable_to.into());
        self
    }

    /// Replaces the whole detail tree.
    #[must_use]
    pub fn detail(mut self, detail: Detail) -> Self {
        self.event.detail = detail;
        self
    }

    /// Appends one element to the detail tree.
    #[must_use]
    pub fn push(mut self, element: Element) -> Self {
        self.event.detail.push(element);
        self
    }

    /// Appends a typed detail to the detail tree.
    #[must_use]
    pub fn typed<T: TypedDetail>(self, value: &T) -> Self {
        self.push(value.to_element())
    }

    /// Finishes the event.
    #[must_use]
    pub fn build(self) -> Event {
        self.event
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::contact::STREAMING_ENDPOINT;

    #[test]
    fn a_built_event_carries_the_defaults_every_client_expects() {
        let event = Event::builder("a-f-G-U-C", "UID-A")
            .how("m-g")
            .point(51.5074, -0.1278)
            .stale_after(Duration::from_secs(60))
            .build();
        assert_eq!(event.version, VERSION);
        assert_eq!(event.time, event.start);
        assert_eq!(event.stale - event.start, 60_000);
        assert_eq!(event.point.hae, Point::UNKNOWN_HAE);
        assert!(event.detail.is_empty());
    }

    #[test]
    fn setting_the_time_after_the_staleness_keeps_the_interval() {
        let base = CotTime::from_millis(1_000_000);
        let event = Event::builder("t-x-c-t", "UID-A")
            .stale_after(Duration::from_secs(20))
            .time(base)
            .build();
        assert_eq!(event.time, base);
        assert_eq!(event.start, base);
        assert_eq!(event.stale, base.stale_after(Duration::from_secs(20)));
    }

    #[test]
    fn situational_awareness_needs_uid_callsign_and_endpoint() {
        let sa = Event::builder("a-f-G-U-C", "UID-A")
            .typed(&Contact::new("ALPHA").with_endpoint(STREAMING_ENDPOINT))
            .build();
        assert!(sa.is_sa());
        assert_eq!(sa.callsign(), Some("ALPHA"));
        assert_eq!(sa.endpoint(), Some(STREAMING_ENDPOINT));

        let no_endpoint = Event::builder("a-f-G-U-C", "UID-A")
            .typed(&Contact::new("ALPHA"))
            .build();
        assert!(!no_endpoint.is_sa());

        let no_uid = Event::builder("a-f-G-U-C", "")
            .typed(&Contact::new("ALPHA").with_endpoint(STREAMING_ENDPOINT))
            .build();
        assert!(!no_uid.is_sa());
    }

    #[test]
    fn an_empty_callsign_reads_as_absent() {
        let event = Event::builder("a-f-G-U-C", "UID-A")
            .push(Element::new("contact").attr("callsign", ""))
            .build();
        assert_eq!(event.callsign(), None);
        assert!(!event.is_sa());
    }

    #[test]
    fn typed_accessors_read_through_the_detail_tree() {
        let event = Event::builder("a-f-G-U-C", "UID-A")
            .typed(&Group::new("Cyan", "Team Member"))
            .typed(&Takv {
                platform: "ATAK-CIV".into(),
                version: "5.4.0".into(),
                ..Takv::default()
            })
            .build();
        assert_eq!(event.group().unwrap().name, "Cyan");
        assert_eq!(event.takv().unwrap().summary(), "ATAK-CIV:5.4.0");
        assert!(event.contact().is_none());
    }

    #[test]
    fn control_classification_comes_from_the_type() {
        assert!(Event::builder("t-x-c-t", "UID-A").build().is_control());
        assert!(!Event::builder("t-x-d-d", "UID-A").build().is_control());
    }

    #[test]
    fn staleness_is_inclusive_of_the_stale_instant() {
        let now = CotTime::from_millis(5_000);
        let event = Event::builder("a-f", "UID-A").time(now).stale(now).build();
        assert!(event.is_stale_at(now));
        assert!(!event.is_stale_at(CotTime::from_millis(4_999)));
    }

    #[test]
    fn the_zero_point_matches_the_server_generated_template() {
        let point = Point::zero();
        assert_eq!((point.lat, point.lon, point.hae), (0.0, 0.0, 0.0));
        assert_eq!(point.ce, 9_999_999.0);
        assert_eq!(point.le, 9_999_999.0);
    }
}
