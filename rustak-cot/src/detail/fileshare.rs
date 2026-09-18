//! `<fileshare>`, `<ackrequest>` and `<ackresponse>` — data package pointers.
//!
//! A `b-f-t-r` event tells a peer that a file is available and where to fetch
//! it. rustak also emits one in place of an outbound protobuf message that
//! would exceed a client's 64 KiB receive buffer, pointing at
//! `/Marti/api/cot/xml/{uid}`.

use std::time::Duration;

use super::{Element, TypedDetail, apply_extras, extra_attrs};
use crate::event::{Event, Point};
use crate::time::CotTime;
use crate::types::{cot_type, how};

/// How long a file-share pointer stays valid.
pub const FILESHARE_VALIDITY: Duration = Duration::from_secs(10);

/// `<fileshare>` — where to fetch a shared file and how to verify it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileShare {
    /// The file name on disk.
    pub filename: String,
    /// Absolute URL the recipient should fetch.
    pub sender_url: String,
    /// Size in bytes, as a decimal string on the wire.
    pub size_in_bytes: u64,
    /// SHA-256 of the content, lower-case hex.
    pub sha256: String,
    /// Uid of the sending device.
    pub sender_uid: String,
    /// Callsign of the sending device.
    pub sender_callsign: String,
    /// Display name of the package.
    pub name: String,
    /// Whether the sender serves the file itself rather than the server.
    pub peer_hosted: Option<bool>,
    /// Port the sender serves the file on, when peer hosted.
    pub https_port: Option<u16>,
    /// Attributes we do not model, preserved in document order.
    pub extra: Vec<(String, String)>,
}

impl FileShare {
    const KNOWN: &'static [&'static str] = &[
        "filename",
        "senderUrl",
        "sizeInBytes",
        "sha256",
        "senderUid",
        "senderCallsign",
        "name",
        "peerHosted",
        "httpsPort",
    ];
}

impl TypedDetail for FileShare {
    const NAME: &'static str = "fileshare";

    fn from_element(element: &Element) -> Option<Self> {
        let attr = |name: &str| element.get(name).unwrap_or_default().to_owned();
        Some(Self {
            filename: attr("filename"),
            sender_url: attr("senderUrl"),
            size_in_bytes: element
                .get("sizeInBytes")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            sha256: attr("sha256"),
            sender_uid: attr("senderUid"),
            sender_callsign: attr("senderCallsign"),
            name: attr("name"),
            peer_hosted: element
                .get("peerHosted")
                .map(|value| value.eq_ignore_ascii_case("true")),
            https_port: element
                .get("httpsPort")
                .and_then(|value| value.parse().ok()),
            extra: extra_attrs(element, Self::KNOWN),
        })
    }

    fn to_element(&self) -> Element {
        let mut element = Element::new(Self::NAME)
            .attr("filename", self.filename.clone())
            .attr("senderUrl", self.sender_url.clone())
            .attr("sizeInBytes", self.size_in_bytes.to_string())
            .attr("sha256", self.sha256.clone())
            .attr("senderUid", self.sender_uid.clone())
            .attr("senderCallsign", self.sender_callsign.clone())
            .attr("name", self.name.clone())
            .attr_opt("peerHosted", self.peer_hosted.map(|v| v.to_string()))
            .attr_opt("httpsPort", self.https_port.map(|v| v.to_string()));
        apply_extras(&mut element, &self.extra);
        element
    }
}

/// `<ackrequest>` — ask the recipient to confirm receipt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AckRequest {
    /// Correlation id, echoed back in the [`AckResponse`].
    pub uid: String,
    /// Whether an acknowledgement is wanted at all.
    pub ack_requested: bool,
    /// Free-text label shown to the user.
    pub tag: String,
}

impl TypedDetail for AckRequest {
    const NAME: &'static str = "ackrequest";

    fn from_element(element: &Element) -> Option<Self> {
        Some(Self {
            uid: element.get("uid").unwrap_or_default().to_owned(),
            ack_requested: element
                .get("ackrequested")
                .is_some_and(|value| value.eq_ignore_ascii_case("true")),
            tag: element.get("tag").unwrap_or_default().to_owned(),
        })
    }

    fn to_element(&self) -> Element {
        Element::new(Self::NAME)
            .attr("uid", self.uid.clone())
            .attr("ackrequested", self.ack_requested.to_string())
            .attr("tag", self.tag.clone())
    }
}

/// `<ackresponse>` — the reply to an [`AckRequest`], carried by `b-f-t-a`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AckResponse {
    /// The correlation id from the request.
    pub uid: String,
    /// Uid of the device that sent the file.
    pub sender_uid: String,
    /// Whether the transfer succeeded.
    pub success: bool,
    /// The label from the request.
    pub tag: String,
    /// Failure explanation, when `success` is false.
    pub reason: String,
    /// SHA-256 of what was received.
    pub sha256: String,
    /// Size of what was received.
    pub size_in_bytes: u64,
}

impl TypedDetail for AckResponse {
    const NAME: &'static str = "ackresponse";

    fn from_element(element: &Element) -> Option<Self> {
        let attr = |name: &str| element.get(name).unwrap_or_default().to_owned();
        Some(Self {
            uid: attr("uid"),
            sender_uid: attr("senderUid"),
            success: element
                .get("success")
                .is_some_and(|value| value.eq_ignore_ascii_case("true")),
            tag: attr("tag"),
            reason: attr("reason"),
            sha256: attr("sha256"),
            size_in_bytes: element
                .get("sizeInBytes")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
        })
    }

    fn to_element(&self) -> Element {
        Element::new(Self::NAME)
            .attr("uid", self.uid.clone())
            .attr("senderUid", self.sender_uid.clone())
            .attr("success", self.success.to_string())
            .attr("tag", self.tag.clone())
            .attr("reason", self.reason.clone())
            .attr("sha256", self.sha256.clone())
            .attr("sizeInBytes", self.size_in_bytes.to_string())
    }
}

/// Builds the `b-f-t-r` event that points a peer at a file.
///
/// `how` is `h-e` and the pointer goes stale after
/// [`FILESHARE_VALIDITY`], matching what ATAK emits for its own transfers.
#[must_use]
pub fn fileshare_pointer(uid: &str, share: &FileShare, now: CotTime) -> Event {
    Event::builder(cot_type::FILESHARE, uid)
        .how(how::H_E)
        .point_full(Point::zero())
        .time(now)
        .stale_after(FILESHARE_VALIDITY)
        .typed(share)
        .typed(&AckRequest {
            uid: uid.to_owned(),
            ack_requested: true,
            tag: share.name.clone(),
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detail::TypedDetail;

    fn share() -> FileShare {
        FileShare {
            filename: "oversize.xml".into(),
            sender_url: "https://tak.example:8443/Marti/api/cot/xml/UID-A".into(),
            size_in_bytes: 71_680,
            sha256: "0f".repeat(32),
            sender_uid: "UID-A".into(),
            sender_callsign: "ALPHA".into(),
            name: "oversize.xml".into(),
            ..FileShare::default()
        }
    }

    #[test]
    fn fileshare_round_trips_through_its_element() {
        let element = share().to_element();
        assert_eq!(element.get("sizeInBytes"), Some("71680"));
        assert_eq!(element.get("peerHosted"), None);
        assert_eq!(FileShare::from_element(&element), Some(share()));
    }

    #[test]
    fn optional_peer_hosting_attributes_round_trip() {
        let mut hosted = share();
        hosted.peer_hosted = Some(true);
        hosted.https_port = Some(8_080);
        let element = hosted.to_element();
        assert_eq!(element.get("peerHosted"), Some("true"));
        assert_eq!(element.get("httpsPort"), Some("8080"));
        assert_eq!(FileShare::from_element(&element), Some(hosted));
    }

    #[test]
    fn a_bad_size_reads_as_zero_rather_than_failing() {
        let element = Element::new("fileshare").attr("sizeInBytes", "lots");
        assert_eq!(FileShare::from_element(&element).unwrap().size_in_bytes, 0);
    }

    #[test]
    fn the_pointer_event_matches_the_atak_shape() {
        let now = CotTime::from_millis(1_789_646_400_000);
        let event = fileshare_pointer("UID-A", &share(), now);
        assert_eq!(event.r#type, "b-f-t-r");
        assert_eq!(event.how.as_deref(), Some("h-e"));
        assert_eq!(event.stale - event.start, 10_000);
        assert_eq!(event.point, Point::zero());

        let ack = event.detail.get::<AckRequest>().unwrap();
        assert!(ack.ack_requested);
        assert_eq!(ack.uid, "UID-A");
        assert_eq!(event.detail.get::<FileShare>(), Some(share()));
    }

    #[test]
    fn ack_response_round_trips() {
        let response = AckResponse {
            uid: "UID-A".into(),
            sender_uid: "UID-B".into(),
            success: true,
            tag: "oversize.xml".into(),
            sha256: "0f".repeat(32),
            size_in_bytes: 71_680,
            ..AckResponse::default()
        };
        let element = response.to_element();
        assert_eq!(element.get("success"), Some("true"));
        assert_eq!(AckResponse::from_element(&element), Some(response));
    }
}
