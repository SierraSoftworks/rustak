//! `rustak-plugin-remoteid`: drones from Remote ID (ASTM F3411, ASD-STAN EN
//! 4709-002) on a rustak channel.
//!
//! A Remote ID receiver is a Wi-Fi card in monitor mode or a Bluetooth sniffer
//! writing what it hears to a capture file (`tcpdump -w`, Wireshark, the nRF
//! Sniffer, or a RIDF recorder). Every tick this sidecar reads the frames that
//! were appended since the last one, feeds them through a `remoteid::Tracker`
//! that it keeps for the life of the process, and publishes one aircraft and
//! one operator marker for each drone it heard. The aircraft's remarks carry the
//! tracker's trust verdict (spoofed, suspicious, plausible, authenticated) and
//! its reasons, and a spoofed drone is drawn as a suspect rather than as an
//! unknown, because Remote ID is unauthenticated and trivially faked.
//!
//! The decoding, tracking, spoof detection and CoT mapping are `uas-cot`'s.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rustak_client::sidecar::{Sidecar, SidecarContext, async_trait, run};
use rustak_core::prelude::*;
use rustak_cot::Event;
use uas_cot::remoteid::capture::{self, CapturedFrame};
use uas_cot::remoteid::{Tracker, TrackerConfig, Transmitter, Verdict};
use uas_cot::rid;

fn default_true() -> bool {
    true
}

fn default_stale_secs() -> u64 {
    30
}

/// `[settings]`: this plugin's own configuration.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Settings {
    /// The pcap, pcapng or RIDF file the receiver is writing. Required.
    capture: Option<PathBuf>,

    /// Draw a drone whose broadcast the tracker judges spoofed as a suspect
    /// (`a-s-…`) rather than as an unknown. Default: true.
    #[serde(default = "default_true")]
    spoofed_as_suspect: bool,

    /// Seconds a marker stays fresh after the last frame heard. Default: 30.
    #[serde(default = "default_stale_secs")]
    stale_secs: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            capture: None,
            spoofed_as_suspect: true,
            stale_secs: default_stale_secs(),
        }
    }
}

#[derive(Default)]
struct RemoteIdSidecar {
    context: Option<SidecarContext<Settings>>,
    tracker: Tracker,
    /// How many frames of the capture have been fed to the tracker already.
    consumed: usize,
}

impl RemoteIdSidecar {
    /// Feeds the frames after the ones already consumed to the tracker and
    /// returns the markers of every transmitter that was heard in them.
    fn ingest(&mut self, frames: &[CapturedFrame], settings: &Settings, now_us: u64) -> Vec<Event> {
        if frames.len() < self.consumed {
            // The receiver rotated or truncated its file: start again.
            self.tracker = Tracker::new(TrackerConfig::default());
            self.consumed = 0;
        }

        let mut heard = BTreeSet::new();
        for frame in &frames[self.consumed..] {
            self.tracker
                .observe(frame.transmitter, &frame.observation(frame.utc_clock));
            heard.insert(frame.transmitter);
        }
        self.consumed = frames.len();

        heard
            .iter()
            .filter_map(|tx| self.tracker.get(tx).map(|uas| (tx, uas)))
            .flat_map(|(tx, uas)| {
                let affiliation =
                    if settings.spoofed_as_suspect && uas.assessment.verdict == Verdict::Spoofed {
                        's'
                    } else {
                        'u'
                    };
                let options = rid::Options {
                    affiliation,
                    stale_secs: settings.stale_secs,
                    ..rid::Options::default()
                };
                rid::tracked(uas, Some(&transmitter_id(tx)), now_us, &options)
            })
            .filter_map(|event| match rustak_cot::xml::parse_str(&event.to_xml()) {
                Ok(event) => Some(event),
                Err(error) => {
                    warn!(uid = %event.uid, %error, "Dropping a Remote ID event that did not parse as CoT.");
                    None
                }
            })
            .collect()
    }
}

/// The radio address, for a drone that has not sent its Basic ID yet.
fn transmitter_id(tx: &Transmitter) -> String {
    match tx {
        Transmitter::Ble(a) | Transmitter::Wifi(a) => {
            a.iter().map(|b| format!("{b:02X}")).collect()
        }
        Transmitter::Other(n) => format!("{n:X}"),
    }
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

#[async_trait]
impl Sidecar for RemoteIdSidecar {
    const NAME: &'static str = env!("CARGO_PKG_NAME");
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    type Settings = Settings;

    async fn start(&mut self, ctx: SidecarContext<Self::Settings>) -> Result<(), Error> {
        info!(capture = ?ctx.settings().capture, "The Remote ID sidecar is starting.");
        self.context = Some(ctx);
        Ok(())
    }

    async fn tick(&mut self) -> Result<Vec<Event>, Error> {
        let Some(context) = self.context.clone() else {
            return Ok(Vec::new());
        };
        let settings = context.settings();
        let Some(path) = &settings.capture else {
            warn!("No [settings] capture is configured, so there is nothing to read.");
            return Ok(Vec::new());
        };

        let frames = match tokio::fs::read(path).await {
            Ok(bytes) => match capture::frames(&bytes) {
                Ok(frames) => frames,
                Err(error) => {
                    warn!(path = %path.display(), %error, "The capture could not be read yet.");
                    return Ok(Vec::new());
                }
            },
            Err(error) => {
                warn!(path = %path.display(), %error, "The capture could not be opened.");
                return Ok(Vec::new());
            }
        };

        Ok(self.ingest(&frames, settings, now_us()))
    }
}

#[tokio::main]
async fn main() {
    run::<RemoteIdSidecar>().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_client::sidecar::SidecarConfig;
    use uas_cot::remoteid::{
        BasicId, IdType, Location, Message, OperatorLocationType, System, Transport, UaType,
    };

    const EXAMPLE: &str = include_str!("../config.example.toml");

    const BLE: [u8; 6] = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];

    fn frame(counter: u8, ms: u64, message: Message) -> CapturedFrame {
        CapturedFrame {
            time_us: 1_763_700_000_000_000 + ms * 1000,
            transmitter: Transmitter::Ble(BLE),
            transport: Transport::Bluetooth4,
            counter: Some(counter),
            payload: message.encode().unwrap().to_vec(),
            beacon: None,
            rssi_dbm: None,
            utc_clock: true,
        }
    }

    fn drone() -> Vec<CapturedFrame> {
        let id = BasicId::new(
            IdType::SerialNumber,
            UaType::HelicopterOrMultirotor,
            "1596F35074529ABCDEFG",
        )
        .unwrap();
        let location = Location {
            latitude: 51.4700,
            longitude: -0.4543,
            altitude_geo: Some(120.0),
            ..Location::default()
        };
        let system = System {
            operator_location_type: OperatorLocationType::LiveGnss,
            operator_latitude: 51.4690,
            operator_longitude: -0.4550,
            ..System::default()
        };
        vec![
            frame(1, 0, Message::BasicId(id)),
            frame(2, 100, Message::Location(location)),
            frame(3, 200, Message::System(system)),
        ]
    }

    #[test]
    fn the_example_configuration_file_is_one_this_plugin_can_load() {
        let config: SidecarConfig<Settings> = rustak_core::config::load_str(EXAMPLE).unwrap();
        assert_eq!(config.service.name.as_str(), "remoteid");
        assert!(config.settings.capture.is_some());
        assert!(config.settings.spoofed_as_suspect);
    }

    #[test]
    fn a_ridf_capture_becomes_an_aircraft_and_its_operator_with_a_trust_verdict() {
        let bytes = capture::to_ridf(&drone()).unwrap();
        let frames = capture::frames(&bytes).unwrap();

        let mut sidecar = RemoteIdSidecar::default();
        let events = sidecar.ingest(&frames, &Settings::default(), 1_763_700_001_000_000);

        assert_eq!(events.len(), 2, "{events:?}");
        let aircraft = &events[0];
        assert_eq!(aircraft.uid, "rid-1596F35074529ABCDEFG");
        assert_eq!(aircraft.r#type, "a-u-A-M-F-Q");
        assert_eq!(aircraft.callsign(), Some("1596F35074529ABCDEFG"));
        let remarks = rustak_cot::detail::chat::remarks(&aircraft.detail).unwrap();
        assert!(
            remarks.text.ends_with("Trust: plausible."),
            "{}",
            remarks.text
        );
        assert_eq!(events[1].uid, "rid-op-1596F35074529ABCDEFG");
        assert_eq!(events[1].r#type, "b-m-p-s-m");

        // Nothing new appended: nothing to publish again.
        assert!(
            sidecar
                .ingest(&frames, &Settings::default(), 1_763_700_002_000_000)
                .is_empty()
        );
    }
}
