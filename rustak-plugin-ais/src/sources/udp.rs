//! A receiver of your own: NMEA 0183 `!AIVDM` sentences over UDP.
//!
//! An AIS receiver — AIS-catcher, `rtl_ais`, a dAISy hat, a commercial
//! transponder's network output — sends the sentences it decodes to a UDP port
//! as fast as it hears them, one or several per datagram. This source binds
//! that port, reassembles the multi-fragment messages, and turns each one into
//! an observation.
//!
//! **Terms: none.** The antenna is the operator's, the receiver is the
//! operator's, and nothing here contacts anybody. It is also the only source in
//! this plugin that works with no internet at all, which is the case a TAK
//! deployment is most often in.
//!
//! # Listening is not connecting
//!
//! A bound socket has no peer to lose, so "connected" here means "the port is
//! bound". What can still fail is the bind itself — a port already taken, an
//! address that no longer exists — and that is what the backoff and the
//! heartbeat's `reconnecting` state are for.

use std::net::SocketAddr;
use std::time::Duration;

use chrono::Utc;
use nmea_parser::ais::{CargoType, ShipType, VesselDynamicData, VesselStaticData};
use nmea_parser::{NmeaParser, ParsedMessage};
use rustak_client::feed::{Feed, Track};
use rustak_client::sidecar::async_trait;
use rustak_core::prelude::*;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::mapping::{Dimensions, Position, StaticData};
use crate::status::{Connection, ConnectionTx};
use crate::vessels::{Observation, Vessels};

use super::notice::{Repeated, Report, humanised};
use super::{Backoff, SourceContext};

/// What this source calls itself in a log line and on a heartbeat.
pub const NAME: &str = "nmea-udp";

/// The largest datagram taken from the socket. NMEA sentences are at most 82
/// characters, and receivers pack several into one datagram; this is more than
/// any of them sends.
const DATAGRAM: usize = 8 * 1024;

/// How many observations are buffered between the socket and the sidecar's
/// tick. A single receiver on a busy coast is tens of sentences a second.
const BUFFER: usize = 8_192;

/// A navigational status of 15 is AIS for "not defined", which every class B
/// report carries because class B has no status field at all.
const NAV_STATUS_UNDEFINED: u8 = 15;

/// A bound UDP port, and what has arrived on it.
#[derive(Debug)]
pub struct UdpFeed {
    /// What the socket task has decoded since the last tick.
    observations: mpsc::Receiver<Observation>,
    /// The per-MMSI memory that turns two message types into one track.
    vessels: Vessels,
}

impl UdpFeed {
    /// Binds the port and starts reading it.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error naming the address when the port
    /// cannot be bound — which is a setting the operator can fix, and the one
    /// thing about this source worth refusing to start over.
    pub fn open(listen: SocketAddr, context: SourceContext) -> Result<Self, Error> {
        let socket = bind(listen)?;
        let (sender, observations) = mpsc::channel(BUFFER);

        context.connection.send_replace(Connection::connected());
        info!(source = NAME, %listen, "Listening for AIS sentences.");

        tokio::spawn(run(
            socket,
            listen,
            sender,
            context.connection,
            context.shutdown,
        ));

        Ok(Self {
            observations,
            vessels: Vessels::new(NAME, context.policy.stale(), context.policy.max_tracks),
        })
    }
}

#[async_trait]
impl Feed for UdpFeed {
    fn name(&self) -> &str {
        NAME
    }

    async fn poll(&mut self) -> Result<Vec<Track>, Error> {
        let mut observations = Vec::new();

        while let Ok(observation) = self.observations.try_recv() {
            observations.push(observation);
        }

        Ok(self.vessels.absorb_all(observations))
    }
}

/// Binds the port, as something an operator can fix when it will not.
fn bind(listen: SocketAddr) -> Result<UdpSocket, Error> {
    let socket = std::net::UdpSocket::bind(listen).map_err(|err| {
        human_errors::user(
            format!("Could not listen for AIS sentences on {listen} ({err})."),
            &[
                "Check that no other process is already bound to that port.",
                "Use 0.0.0.0 to take datagrams from the network, or 127.0.0.1 for this host only.",
            ],
        )
    })?;

    socket.set_nonblocking(true).wrap_system_err(
        "The AIS listener could not be made non-blocking.",
        &["This is a bug in rustak; please report it via GitHub."],
    )?;

    UdpSocket::from_std(socket).wrap_system_err(
        "The AIS listener could not join the async runtime.",
        &["This is a bug in rustak; please report it via GitHub."],
    )
}

/// Reads the port until the sidecar stops, rebinding it if it ever fails.
///
/// A port that is taken by something else stays taken, so the rebind fails
/// every time the backoff falls due: the run of failures is announced once and
/// then counted, which is what stops a misconfigured port being a log line a
/// second for as long as the sidecar runs.
async fn run(
    socket: UdpSocket,
    listen: SocketAddr,
    sender: mpsc::Sender<Observation>,
    connection: ConnectionTx,
    shutdown: Shutdown,
) {
    let mut backoff = Backoff::default();
    let mut socket = Some(socket);
    let mut trouble = Repeated::default();

    while !shutdown.is_cancelled() {
        let bound = match socket.take() {
            Some(bound) => bound,
            None => match bind(listen) {
                Ok(bound) => {
                    listening_again(&mut trouble, listen);
                    connection.send_replace(Connection::connected());
                    backoff.reset();
                    bound
                }
                Err(err) => {
                    cannot_listen(&mut trouble, &err.to_string(), backoff.next());
                    connection.send_replace(Connection::reconnecting(err.to_string()));

                    if !backoff.wait(&shutdown).await {
                        return;
                    }

                    continue;
                }
            },
        };

        match read(&bound, &sender, &shutdown).await {
            Ok(()) => return,
            Err(reason) => {
                cannot_listen(&mut trouble, &reason, backoff.next());
                connection.send_replace(Connection::reconnecting(reason));
            }
        }

        if !backoff.wait(&shutdown).await {
            return;
        }
    }
}

/// One attempt at the port that did not work, said once per run.
fn cannot_listen(trouble: &mut Repeated, reason: &str, retry_in: Duration) {
    match trouble.happened(Utc::now()) {
        Report::First => warn!(source = NAME, retry_in = ?retry_in, "Cannot listen: {reason}"),
        Report::Reminder { count, over } => warn!(
            source = NAME,
            "Still cannot listen: {count} attempts failed in the last {}. {reason}",
            humanised(over),
        ),
        _ => debug!(source = NAME, retry_in = ?retry_in, "Still cannot listen: {reason}"),
    }
}

/// The port answering again, which is the end of that run.
fn listening_again(trouble: &mut Repeated, listen: SocketAddr) {
    match trouble.cleared(Utc::now()) {
        Report::Recovered { count, over } => info!(
            source = NAME,
            %listen,
            "Listening again after {} and {count} failed attempts.",
            humanised(over),
        ),
        _ => debug!(source = NAME, %listen, "Listening again."),
    }
}

/// Reads datagrams and delivers what they decode to, until one of them fails.
async fn read(
    socket: &UdpSocket,
    sender: &mpsc::Sender<Observation>,
    shutdown: &Shutdown,
) -> Result<(), String> {
    // One parser for the life of the socket: a type 5 report is split across
    // two sentences and the state that joins them lives here.
    let mut parser = NmeaParser::new();
    let mut datagram = vec![0u8; DATAGRAM];

    loop {
        let read = tokio::select! {
            biased;

            () = shutdown.cancelled() => return Ok(()),
            read = socket.recv_from(&mut datagram) => read,
        };

        let (length, _from) =
            read.map_err(|err| format!("the socket could not be read ({err})"))?;
        let Ok(text) = std::str::from_utf8(&datagram[..length]) else {
            debug!(source = NAME, "A datagram that is not text.");

            continue;
        };

        for line in text.lines() {
            let Some(observation) = decode(&mut parser, line.trim()) else {
                continue;
            };

            // A full buffer drops the observation: a position report that is
            // several ticks old is not worth holding a socket up for.
            if let Err(mpsc::error::TrySendError::Closed(_)) = sender.try_send(observation) {
                return Ok(());
            }
        }
    }
}

/// Decodes one sentence, answering the observation it completes.
///
/// A fragment that completes nothing, a sentence this plugin has no use for and
/// a line that will not parse are all [`None`]: a receiver emits GNSS sentences
/// and safety broadcasts down the same socket, and one bad line must not stop
/// the next one.
fn decode(parser: &mut NmeaParser, line: &str) -> Option<Observation> {
    if line.is_empty() {
        return None;
    }

    match parser.parse_sentence(line) {
        Ok(ParsedMessage::VesselDynamicData(dynamic)) => {
            position(&dynamic).map(Observation::Position)
        }
        Ok(ParsedMessage::VesselStaticData(statics)) => {
            Some(Observation::Static(static_data(&statics)))
        }
        Ok(_) => None,
        Err(err) => {
            debug!(source = NAME, "A sentence that would not parse: {err}");

            None
        }
    }
}

/// A dynamic report, as this plugin's own position.
fn position(dynamic: &VesselDynamicData) -> Option<Position> {
    Some(Position {
        mmsi: dynamic.mmsi,
        position: (dynamic.latitude?, dynamic.longitude?),
        sog_knots: dynamic.sog_knots,
        cog_deg: dynamic.cog,
        heading_deg: dynamic.heading_true,
        // A class B report has no status field, and the decoder spells that as
        // "not defined" rather than as nothing.
        nav_status: Some(dynamic.nav_status.to_value())
            .filter(|code| *code != NAV_STATUS_UNDEFINED),
        // The sentence carries only the second of the minute it was sent in, so
        // the receiver's clock is the best answer available — and a receiver on
        // the same network is never more than a moment out.
        observed_at: Utc::now(),
    })
}

/// A static report, as this plugin's own static data.
fn static_data(statics: &VesselStaticData) -> StaticData {
    let dimensions = Dimensions {
        to_bow: statics.dimension_to_bow.unwrap_or_default(),
        to_stern: statics.dimension_to_stern.unwrap_or_default(),
        to_port: statics.dimension_to_port.unwrap_or_default(),
        to_starboard: statics.dimension_to_starboard.unwrap_or_default(),
    };

    StaticData {
        mmsi: statics.mmsi,
        name: trimmed(statics.name.as_deref()),
        call_sign: trimmed(statics.call_sign.as_deref()),
        imo: statics.imo_number,
        ship_type: Some(ship_type_code(statics.ship_type, statics.cargo_type)),
        destination: trimmed(statics.destination.as_deref()),
        eta: statics.eta.map(|eta| eta.format("%m-%d %H:%M").to_string()),
        dimensions: dimensions.is_known().then_some(dimensions),
    }
}

/// Puts the combined ship-and-cargo byte back together.
///
/// The decoder splits the field into a ship type and a cargo type, and the
/// mapping wants the code AIS actually sent. For 30–39 and 50–59 the ship type
/// *is* the whole code; everywhere else it is the tens digit and the cargo type
/// carries the units.
fn ship_type_code(ship_type: ShipType, cargo_type: CargoType) -> u8 {
    let tens = ship_type.to_value();

    match tens {
        30..=39 | 50..=59 => tens,
        _ => tens.saturating_add(cargo_type.to_value().saturating_sub(10)),
    }
}

/// AIS pads its text fields with `@` and spaces; neither is part of a name.
fn trimmed(raw: Option<&str>) -> Option<String> {
    let value = raw?.trim_matches(|character: char| character == '@' || character.is_whitespace());

    match value.is_empty() {
        true => None,
        false => Some(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// The AIS six-bit character set, which is what `!AIVDM` payloads are
    /// armoured with and what its text fields are written in.
    const SIX_BIT: &str = concat!(
        "@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_ !\"#$%&'()*+,-./",
        "0123456789:;<=>?",
    );

    /// A payload under construction, one field at a time.
    ///
    /// This is rustak's own encoder, written from the public ITU-R M.1371
    /// field layout, so that every sentence in this suite is a fixture we made
    /// rather than traffic somebody captured.
    #[derive(Default)]
    struct Payload {
        bits: Vec<bool>,
    }

    impl Payload {
        /// Appends `width` bits of `value`, most significant first.
        fn unsigned(mut self, width: usize, value: u64) -> Self {
            for shift in (0..width).rev() {
                self.bits.push((value >> shift) & 1 == 1);
            }

            self
        }

        /// The same for a two's-complement field.
        fn signed(self, width: usize, value: i64) -> Self {
            let mask = (1u64 << width) - 1;

            self.unsigned(width, (value as u64) & mask)
        }

        /// Appends `characters` six-bit characters, padded with `@`.
        fn text(mut self, characters: usize, value: &str) -> Self {
            let mut written = 0;

            for character in value.chars().take(characters) {
                let index = SIX_BIT
                    .chars()
                    .position(|candidate| candidate == character)
                    .expect("a character AIS can carry");
                self = self.unsigned(6, index as u64);
                written += 1;
            }

            for _ in written..characters {
                self = self.unsigned(6, 0);
            }

            self
        }

        /// The armoured payload, padded to a whole number of characters.
        fn armour(mut self) -> String {
            while !self.bits.len().is_multiple_of(6) {
                self.bits.push(false);
            }

            self.bits
                .chunks(6)
                .map(|chunk| {
                    let value = chunk
                        .iter()
                        .fold(0u8, |acc, bit| (acc << 1) | u8::from(*bit));
                    let character = value + 48;

                    char::from(match character > 87 {
                        true => character + 8,
                        false => character,
                    })
                })
                .collect()
        }
    }

    /// Wraps a payload in the `!AIVDM` sentence a receiver would send.
    fn sentence(count: u8, number: u8, message_id: &str, payload: &str, fill: u8) -> String {
        let body = format!("AIVDM,{count},{number},{message_id},A,{payload},{fill}");
        let checksum = body.bytes().fold(0u8, |acc, byte| acc ^ byte);

        format!("!{body}*{checksum:02X}")
    }

    /// A class A position report (type 1).
    fn class_a(mmsi: u32, nav_status: u8, lat: f64, lon: f64) -> String {
        let payload = Payload::default()
            .unsigned(6, 1)
            .unsigned(2, 0)
            .unsigned(30, u64::from(mmsi))
            .unsigned(4, u64::from(nav_status))
            .unsigned(8, 128) // rate of turn: not available
            .unsigned(10, 62) // speed over ground: 6.2 knots
            .unsigned(1, 1) // position accuracy: high
            .signed(28, (lon * 600_000.0).round() as i64)
            .signed(27, (lat * 600_000.0).round() as i64)
            .unsigned(12, 2715) // course over ground: 271.5 degrees
            .unsigned(9, 270) // true heading
            .unsigned(6, 42) // second of the minute
            .unsigned(2, 0)
            .unsigned(3, 0)
            .unsigned(1, 0)
            .unsigned(19, 0)
            .armour();

        sentence(1, 1, "", &payload, 0)
    }

    /// A class B position report (type 18), which carries no status.
    fn class_b(mmsi: u32, lat: f64, lon: f64) -> String {
        let payload = Payload::default()
            .unsigned(6, 18)
            .unsigned(2, 0)
            .unsigned(30, u64::from(mmsi))
            .unsigned(8, 0)
            .unsigned(10, 41) // 4.1 knots
            .unsigned(1, 1)
            .signed(28, (lon * 600_000.0).round() as i64)
            .signed(27, (lat * 600_000.0).round() as i64)
            .unsigned(12, 880) // 88.0 degrees
            .unsigned(9, 511) // heading: not available
            .unsigned(6, 42)
            .unsigned(2, 0)
            .unsigned(1, 1)
            .unsigned(1, 0)
            .unsigned(1, 0)
            .unsigned(1, 1)
            .unsigned(1, 0)
            .unsigned(1, 0)
            .unsigned(1, 0)
            .unsigned(20, 0)
            .armour();

        sentence(1, 1, "", &payload, 0)
    }

    /// A static and voyage report (type 5), which is 424 bits and therefore
    /// always arrives as two sentences.
    fn class_a_static(mmsi: u32, name: &str, call_sign: &str, ship_type: u8) -> [String; 2] {
        let payload = Payload::default()
            .unsigned(6, 5)
            .unsigned(2, 0)
            .unsigned(30, u64::from(mmsi))
            .unsigned(2, 0) // AIS version
            .unsigned(30, 9_312_345) // IMO number
            .text(7, call_sign)
            .text(20, name)
            .unsigned(8, u64::from(ship_type))
            .unsigned(9, 120) // to bow
            .unsigned(9, 30) // to stern
            .unsigned(6, 11) // to port
            .unsigned(6, 11) // to starboard
            .unsigned(4, 1) // position fix: GPS
            .unsigned(4, 9) // ETA month
            .unsigned(5, 21) // ETA day
            .unsigned(5, 6) // ETA hour
            .unsigned(6, 0) // ETA minute
            .unsigned(8, 95) // draught, decimetres
            .text(20, "ROTTERDAM")
            .unsigned(1, 0)
            .unsigned(1, 0)
            .armour();
        let (first, second) = payload.split_at(60);

        [
            sentence(2, 1, "3", first, 0),
            sentence(2, 2, "3", second, 2),
        ]
    }

    fn decoded(lines: &[String]) -> Vec<Observation> {
        let mut parser = NmeaParser::new();

        lines
            .iter()
            .filter_map(|line| decode(&mut parser, line))
            .collect()
    }

    #[test]
    fn a_class_a_position_report_decodes_to_a_position() {
        let observations = decoded(&[class_a(244_660_000, 0, 51.9512, 4.1338)]);

        let [Observation::Position(position)] = observations.as_slice() else {
            panic!("one position, got {observations:?}");
        };

        assert_eq!(position.mmsi, 244_660_000);
        assert!((position.position.0 - 51.9512).abs() < 1e-4, "{position:?}");
        assert!((position.position.1 - 4.1338).abs() < 1e-4, "{position:?}");
        assert_eq!(position.sog_knots, Some(6.2));
        assert_eq!(position.cog_deg, Some(271.5));
        assert_eq!(position.heading_deg, Some(270.0));
        assert_eq!(position.nav_status, Some(0));
    }

    #[test]
    fn a_moored_vessel_keeps_the_status_the_staleness_depends_on() {
        let observations = decoded(&[class_a(244_660_001, 5, 51.95, 4.13)]);

        let [Observation::Position(position)] = observations.as_slice() else {
            panic!("one position, got {observations:?}");
        };

        assert_eq!(position.nav_status, Some(5));
        assert!(crate::mapping::is_stationary_status(5));
    }

    #[test]
    fn a_class_b_position_report_decodes_without_a_navigational_status() {
        let observations = decoded(&[class_b(244_123_456, 51.8977, 4.0104)]);

        let [Observation::Position(position)] = observations.as_slice() else {
            panic!("one position, got {observations:?}");
        };

        assert_eq!(position.mmsi, 244_123_456);
        assert_eq!(
            position.nav_status, None,
            "class B has no status field at all",
        );
        let knots = position.sog_knots.expect("a speed");
        assert!((knots - 4.1).abs() < 1e-6, "{knots}");
        assert_eq!(
            position.heading_deg, None,
            "the decoder applies the 511 sentinel itself",
        );
    }

    #[test]
    fn a_two_fragment_static_report_decodes_once_both_halves_have_arrived() {
        let fragments = class_a_static(244_660_000, "ZEEBRUGGE", "PBZE", 70);

        // The first fragment on its own completes nothing.
        assert!(decoded(&fragments[..1]).is_empty());

        let observations = decoded(&fragments);

        let [Observation::Static(statics)] = observations.as_slice() else {
            panic!("one static report, got {observations:?}");
        };

        assert_eq!(statics.mmsi, 244_660_000);
        assert_eq!(statics.name.as_deref(), Some("ZEEBRUGGE"));
        assert_eq!(statics.call_sign.as_deref(), Some("PBZE"));
        assert_eq!(statics.imo, Some(9_312_345));
        assert_eq!(statics.ship_type, Some(70));
        assert_eq!(statics.destination.as_deref(), Some("ROTTERDAM"));
        assert_eq!(statics.dimensions.expect("dimensions").length_m(), 150);
        assert_eq!(statics.dimensions.expect("dimensions").beam_m(), 22);
    }

    #[test]
    fn a_malformed_line_is_dropped_rather_than_dropping_the_socket() {
        let mut parser = NmeaParser::new();
        let good = class_a(244_660_000, 0, 51.95, 4.13);

        for line in [
            "",
            "not a sentence",
            "!AIVDM,1,1,,A,,0*26",
            // A real sentence with one character of its payload changed, so the
            // checksum no longer matches: a receiver with a noisy antenna.
            &good.replace(",A,1", ",A,2"),
        ] {
            assert!(decode(&mut parser, line).is_none(), "{line:?}");
        }

        // And the parser still works afterwards, which is the point.
        assert!(decode(&mut parser, &good).is_some());
    }

    #[test]
    fn a_sentence_that_is_not_about_a_vessel_is_ignored() {
        let mut parser = NmeaParser::new();

        // A GNSS fix from the same receiver, down the same socket.
        assert!(
            decode(
                &mut parser,
                "$GPGGA,120000.00,5157.0720,N,00408.0280,E,1,08,0.9,5.0,M,46.9,M,,*4F",
            )
            .is_none(),
        );
    }

    #[test]
    fn the_split_ship_and_cargo_field_is_put_back_together() {
        for raw in [0u8, 30, 35, 37, 52, 55, 59, 60, 70, 74, 80, 89, 99] {
            assert_eq!(
                ship_type_code(ShipType::new(raw), CargoType::new(raw)),
                raw,
                "ship and cargo type {raw}",
            );
        }
    }

    #[test]
    fn a_static_report_reaches_the_map_as_the_class_its_type_means() {
        let fragments = class_a_static(244_660_002, "NORDKAP", "OXAB", 30);
        let observations = decoded(&fragments);

        let [Observation::Static(statics)] = observations.as_slice() else {
            panic!("one static report, got {observations:?}");
        };

        assert_eq!(
            crate::mapping::vessel_class(statics.ship_type.expect("a ship type")),
            rustak_client::feed::VesselClass::Fishing,
        );
    }

    #[tokio::test]
    async fn a_port_that_is_already_taken_is_a_start_up_failure_naming_it() {
        let held = std::net::UdpSocket::bind("127.0.0.1:0").expect("an ephemeral port");
        let taken = held.local_addr().expect("the bound address");
        let (connection, _state) = crate::status::connection();

        let refused = UdpFeed::open(
            taken,
            SourceContext {
                area: rustak_client::feed::Area::default(),
                policy: rustak_client::feed::PublishPolicy::default(),
                shutdown: Shutdown::new(),
                connection,
            },
        )
        .expect_err("the port is already bound");

        assert!(
            refused.to_string().contains(&taken.to_string()),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn sentences_sent_to_the_port_arrive_as_tracks_on_the_next_poll() {
        let (connection, state) = crate::status::connection();
        let shutdown = Shutdown::new();
        let listener = std::net::UdpSocket::bind("127.0.0.1:0").expect("an ephemeral port");
        let listen = listener.local_addr().expect("the bound address");
        drop(listener);

        let mut feed = UdpFeed::open(
            listen,
            SourceContext {
                area: rustak_client::feed::Area::default(),
                policy: rustak_client::feed::PublishPolicy::default(),
                shutdown: shutdown.clone(),
                connection,
            },
        )
        .expect("the port binds");

        assert!(matches!(*state.borrow(), Connection::Connected { .. }));

        let sender = UdpSocket::bind("127.0.0.1:0").await.expect("a sender");
        let mut datagram = class_a(244_660_000, 0, 51.9512, 4.1338);
        datagram.push('\n');
        for fragment in class_a_static(244_660_000, "ZEEBRUGGE", "PBZE", 70) {
            datagram.push_str(&fragment);
            datagram.push('\n');
        }
        sender
            .send_to(datagram.as_bytes(), listen)
            .await
            .expect("the datagram is sent");

        let tracks = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let tracks = feed.poll().await.expect("a poll never fails");

                if tracks.len() >= 2 {
                    return tracks;
                }

                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the sentences arrive");

        assert_eq!(tracks[0].id, "AIS-244660000");
        assert_eq!(tracks[0].callsign.as_deref(), Some("MMSI 244660000"));
        assert_eq!(tracks[1].callsign.as_deref(), Some("ZEEBRUGGE"));

        shutdown.cancel();
    }
}
