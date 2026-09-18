//! Stored Cursor-on-Target, as the administrator's browser reads it.
//!
//! Two shapes, because there are two questions. *What is on the map?* is a page
//! of [`CotSummary`] — one row per uid, the last thing each said, enough to
//! draw a list and filter it. *What exactly did this uid send?* is a
//! [`CotDetail`], which carries the XML the recipients were actually given.
//!
//! # The XML is the relayed form
//!
//! `<marti>` already stripped and this server's flow tag already present, which
//! is what makes a stored message reproducible: an operator comparing what a
//! device claims it received against what we hold is comparing the same bytes
//! rather than the sender's draft of them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The last message one uid sent, without its body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CotSummary {
    /// The event uid, which is the key of the store and of every route here.
    pub uid: String,

    /// The CoT type, `a-f-G-U-C` and its siblings.
    #[serde(rename = "type")]
    pub kind: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,

    /// The team colour from `<__group name>`, when the message carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,

    /// The role from `<__group role>`, when the message carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    /// The event's own time.
    pub time: DateTime<Utc>,

    /// When the event stops being worth drawing.
    pub stale: DateTime<Utc>,

    /// When this server relayed it, which is what a listing is ordered by.
    pub received_at: DateTime<Utc>,

    pub lat: f64,
    pub lon: f64,

    /// The channels the sender was publishing into, resolved to names.
    ///
    /// A bit with no name in the index — a channel deleted since the message
    /// was sent — is left out rather than rendered as a number.
    #[serde(default)]
    pub groups: Vec<String>,
}

/// One stored message, with the bytes its recipients were sent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CotDetail {
    #[serde(flatten)]
    pub summary: CotSummary,

    /// The relayed XML, exactly as stored.
    pub xml: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary() -> CotSummary {
        CotSummary {
            uid: "ANDROID-1".to_string(),
            kind: "a-f-G-U-C".to_string(),
            callsign: Some("ALPHA".to_string()),
            team: Some("Cyan".to_string()),
            role: Some("Team Member".to_string()),
            time: "2026-09-18T12:00:00.500Z".parse().unwrap(),
            stale: "2026-09-18T12:02:00.500Z".parse().unwrap(),
            received_at: "2026-09-18T12:00:00.600Z".parse().unwrap(),
            lat: 51.5,
            lon: -0.12,
            groups: vec!["Blue".to_string()],
        }
    }

    #[test]
    fn a_summary_round_trips_through_serde() {
        let json = serde_json::to_string(&summary()).unwrap();

        assert_eq!(
            serde_json::from_str::<CotSummary>(&json).unwrap(),
            summary()
        );
    }

    #[test]
    fn the_cot_type_is_called_type_on_the_wire() {
        // `type` is a Rust keyword and the only name a reader of CoT will look
        // for, so the rename is written out rather than avoided.
        let json = serde_json::to_value(summary()).unwrap();

        assert_eq!(json["type"], "a-f-G-U-C");
        assert!(json.get("kind").is_none());
    }

    #[test]
    fn a_detail_is_a_summary_with_the_bytes_beside_it() {
        // Flattened rather than nested: a page rendering a row and a page
        // rendering the drawer read the same field names.
        let detail = CotDetail {
            summary: summary(),
            xml: "<event version=\"2.0\"/>".to_string(),
        };

        let json = serde_json::to_value(&detail).unwrap();

        assert_eq!(json["uid"], "ANDROID-1");
        assert_eq!(json["xml"], "<event version=\"2.0\"/>");
        assert_eq!(serde_json::from_value::<CotDetail>(json).unwrap(), detail);
    }

    #[test]
    fn a_message_that_said_nothing_about_its_sender_omits_those_keys() {
        let json = serde_json::to_value(CotSummary {
            callsign: None,
            team: None,
            role: None,
            groups: Vec::new(),
            ..summary()
        })
        .unwrap();

        assert!(json.get("callsign").is_none());
        assert!(json.get("team").is_none());
        assert_eq!(json["groups"], serde_json::json!([]));
    }
}
