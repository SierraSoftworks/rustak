//! The clients that have enrolled with this server.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{CertificateId, DeviceId, DeviceUid, Username};

/// One enrolled client, as described to the admin UI.
///
/// A device is not an identity: it belongs to a [`Device::username`], and one
/// person may carry several. Everything below the identity is what the client
/// told us about itself — in its `takv` CoT detail, or in the `version`
/// parameter of an enrolment request — so it is descriptive rather than
/// authoritative, and every field of it is optional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: DeviceId,

    /// The identifier the client calls itself by, which is what TAK sends as
    /// `clientUid` and puts in the `uid` of every event it emits.
    pub uid: DeviceUid,

    /// Who this device belongs to.
    pub username: Username,

    /// The name shown on other people's maps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callsign: Option<String>,

    /// The client software, from the `takv` detail: `Android`, `iOS`,
    /// `Windows`, or whatever a bridge calls itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,

    /// The client software's version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// The hardware, from the `takv` detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_model: Option<String>,

    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,

    /// Where we last saw it connect from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ip: Option<IpAddr>,

    /// The certificate this device last presented, so an administrator can go
    /// from a device to the certificate to revoke.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_certificate_id: Option<CertificateId>,
}

impl Device {
    /// What to call this device in the UI.
    pub fn display(&self) -> &str {
        self.callsign
            .as_deref()
            .filter(|callsign| !callsign.trim().is_empty())
            .unwrap_or_else(|| self.uid.as_str())
    }

    /// The client software and version, where both are known.
    pub fn platform_version(&self) -> Option<String> {
        match (&self.platform, &self.version) {
            (Some(platform), Some(version)) => Some(format!("{platform} {version}")),
            (Some(platform), None) => Some(platform.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_round_trips_through_serde() {
        let device = Device {
            id: DeviceId::new(1),
            uid: DeviceUid::parse("ANDROID-358240051111110").unwrap(),
            username: Username::parse("alice").unwrap(),
            callsign: Some("ALPHA-1".into()),
            platform: Some("ATAK-CIV".into()),
            version: Some("5.6.0".into()),
            device_model: Some("Samsung SM-G998B".into()),
            first_seen_at: "2026-09-18T12:00:00.500Z".parse().unwrap(),
            last_seen_at: "2026-09-18T13:00:00.250Z".parse().unwrap(),
            last_ip: Some("198.51.100.7".parse().unwrap()),
            last_certificate_id: Some(CertificateId::new(3)),
        };

        let json = serde_json::to_string(&device).unwrap();
        assert_eq!(serde_json::from_str::<Device>(&json).unwrap(), device);
        assert_eq!(device.display(), "ALPHA-1");
        assert_eq!(device.platform_version().as_deref(), Some("ATAK-CIV 5.6.0"));
    }

    #[test]
    fn a_device_that_has_told_us_nothing_about_itself_still_deserialises() {
        // A client that has enrolled but never connected is exactly this, and
        // CloudTAK never sends a `takv` detail at all.
        let device: Device = serde_json::from_value(serde_json::json!({
            "id": 2,
            "uid": "alice (ETL)",
            "username": "alice",
            "first_seen_at": "2026-09-18T12:00:00.500Z",
            "last_seen_at": "2026-09-18T12:00:00.500Z",
        }))
        .unwrap();

        assert_eq!(device.display(), "alice (ETL)");
        assert_eq!(device.platform_version(), None);
        assert_eq!(
            serde_json::to_value(&device)
                .unwrap()
                .as_object()
                .unwrap()
                .keys()
                .count(),
            5,
            "what the client never said should stay absent rather than serialise as null"
        );
    }

    #[test]
    fn an_ipv6_client_address_survives_the_round_trip() {
        let device: Device = serde_json::from_value(serde_json::json!({
            "id": 3,
            "uid": "ANDROID-1",
            "username": "bob",
            "first_seen_at": "2026-09-18T12:00:00.500Z",
            "last_seen_at": "2026-09-18T12:00:00.500Z",
            "last_ip": "2001:db8::1",
        }))
        .unwrap();

        assert_eq!(device.last_ip, Some("2001:db8::1".parse().unwrap()));
        let json = serde_json::to_string(&device).unwrap();
        assert_eq!(serde_json::from_str::<Device>(&json).unwrap(), device);
    }
}
