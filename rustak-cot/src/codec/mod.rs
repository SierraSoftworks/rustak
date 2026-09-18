//! Framing a TAK stream, in either of the two encodings a connection can be in.
//!
//! A streaming connection starts in [`Mode::Xml`]. If the client answers the
//! server's `t-x-takp-v` offer (see [`crate::negotiate`]) it switches to
//! [`Mode::Proto`] the instant the `t-x-takp-r` response is written, and
//! **never** goes back: the response is the last XML byte on that socket.
//! [`TakCodec`] models exactly that — one codec per connection, whose
//! [`set_mode`](TakCodec::set_mode) flips both directions at once.
//!
//! The decoder yields [`Frame`]s, not events: framing and parsing are separate
//! failures with separate reactions, and a server that only needs to relay a
//! message never has to parse it twice. Turn a frame into an
//! [`Event`](crate::Event) with [`crate::xml::parse`] or
//! [`crate::proto::decode`] + [`crate::proto::message_to_event`].
//!
//! ```
//! use bytes::BytesMut;
//! use rustak_cot::codec::{Frame, Mode, TakCodec};
//! use tokio_util::codec::{Decoder, Encoder};
//!
//! let mut codec = TakCodec::new(Mode::Xml);
//! let mut wire = BytesMut::from(&b"junk<event uid=\"A\"><point lat=\"1\" lon=\"2\"/></event>"[..]);
//! let Some(Frame::Xml(message)) = codec.decode(&mut wire)? else {
//!     panic!("a whole message was in the buffer");
//! };
//! assert!(message.starts_with(b"<event"));
//!
//! // After negotiation both directions are protobuf frames.
//! codec.set_mode(Mode::Proto);
//! let mut out = BytesMut::new();
//! codec.encode(Frame::Proto(bytes::Bytes::from_static(b"\x08\x01")), &mut out)?;
//! assert_eq!(&out[..], b"\xbf\x02\x08\x01");
//! # Ok::<(), rustak_cot::error::CodecError>(())
//! ```

mod encoded;
mod proto_frame;
mod varint;
mod xml_frame;

use bytes::{Bytes, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

use crate::error::{CodecError, FrameError};

pub use encoded::EncodedEvent;
pub use proto_frame::{MAGIC, ProtoScanner};
pub use varint::{MAX_LEN as MAX_VARINT_LEN, put_varint, read_varint};
pub use xml_frame::XmlScanner;

/// The largest single inbound message either framer will assemble.
///
/// TAK Server's own XML framer uses this number and drops anything past it;
/// nothing legitimate comes close.
pub const MAX_MESSAGE: usize = 8 * 1024 * 1024;

/// The largest `TakMessage` payload a peer will read.
///
/// This is ATAK's receive buffer, not an inbound limit: a server whose
/// outbound event would encode larger than this must send a `b-f-t-r` pointer
/// to `/Marti/api/cot/xml/{uid}` instead of the event itself.
pub const MAX_PROTO_PAYLOAD: usize = 65_536;

/// Which encoding a connection is currently speaking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mode {
    /// `<event>…</event>` documents laid head to tail. Every connection starts
    /// here, and a client that never negotiates stays here forever.
    #[default]
    Xml,
    /// TAK Protocol v1: `0xBF`, a length varint, then a `TakMessage`.
    Proto,
}

/// One framed message, still in its wire encoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// The bytes of one `<event>…</event>`, with no declaration and nothing
    /// around it.
    Xml(Bytes),
    /// The payload of one protocol frame: a serialised `TakMessage` with the
    /// magic byte and length prefix already removed.
    Proto(Bytes),
}

impl Frame {
    /// Which encoding this frame is in.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        match self {
            Self::Xml(_) => Mode::Xml,
            Self::Proto(_) => Mode::Proto,
        }
    }

    /// The framed bytes, whichever encoding they are in.
    #[must_use]
    pub const fn payload(&self) -> &Bytes {
        match self {
            Self::Xml(bytes) | Self::Proto(bytes) => bytes,
        }
    }

    /// How many bytes the payload occupies.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.payload().len()
    }

    /// Whether the payload is empty — true only for a zero-length protobuf
    /// frame, which is legal but carries no event.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Unwraps the frame into its bytes.
    #[must_use]
    pub fn into_payload(self) -> Bytes {
        match self {
            Self::Xml(bytes) | Self::Proto(bytes) => bytes,
        }
    }
}

/// The per-connection stream codec: XML or protobuf, switchable in place.
///
/// Both scanners are kept across a mode switch, but only the active one is
/// consulted. That matters at the switchover: the response is written as XML
/// and the very next inbound byte is protobuf, so the same buffer is handed to
/// a different framer with no reset.
///
/// # Framing failures never reach the caller
///
/// [`decode`](Decoder::decode) does **not** report a message it had to throw
/// away — an oversized one, or bytes skipped resynchronising a corrupt
/// protobuf stream. It consumes the damage and returns the next good message
/// instead, and the counters [`dropped`](Self::dropped),
/// [`dropped_bytes`](Self::dropped_bytes) and [`skipped`](Self::skipped) say
/// how much was lost.
///
/// That is not a convenience: a peer sending one message we cannot frame must
/// never cost the connection, and `Framed` ends its stream permanently on the
/// first `Err` a decoder returns. Reporting the drop would therefore
/// disconnect the client, which is precisely the behaviour the wire contract
/// forbids. The only errors `decode` can yield come from the transport.
#[derive(Clone, Debug)]
pub struct TakCodec {
    mode: Mode,
    max_frame: usize,
    xml: XmlScanner,
    proto: ProtoScanner,
    dropped: u64,
    dropped_bytes: u64,
}

impl TakCodec {
    /// A codec for a connection in `mode`, capped at [`MAX_MESSAGE`].
    #[must_use]
    pub const fn new(mode: Mode) -> Self {
        Self::with_limit(mode, MAX_MESSAGE)
    }

    /// A codec with a custom per-message cap, for tests and constrained links.
    #[must_use]
    pub const fn with_limit(mode: Mode, max_frame: usize) -> Self {
        Self {
            mode,
            max_frame,
            xml: XmlScanner::new(),
            proto: ProtoScanner::new(),
            dropped: 0,
            dropped_bytes: 0,
        }
    }

    /// The encoding this connection is currently speaking.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Switches both directions to `mode`.
    ///
    /// Call this **after** writing the `t-x-takp-r` response, never before:
    /// the response itself is XML.
    pub const fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// The per-message cap this codec enforces.
    #[must_use]
    pub const fn max_frame(&self) -> usize {
        self.max_frame
    }

    /// How many bytes have been discarded resynchronising the protobuf stream.
    #[must_use]
    pub const fn skipped(&self) -> u64 {
        self.proto.skipped()
    }

    /// How many inbound messages were thrown away for exceeding
    /// [`max_frame`](Self::max_frame).
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// How many bytes those dropped messages occupied.
    #[must_use]
    pub const fn dropped_bytes(&self) -> u64 {
        self.dropped_bytes
    }
}

impl Default for TakCodec {
    fn default() -> Self {
        Self::new(Mode::Xml)
    }
}

impl Decoder for TakCodec {
    type Item = Frame;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, CodecError> {
        loop {
            let framed = match self.mode {
                Mode::Xml => self
                    .xml
                    .split(src, self.max_frame)
                    .map(|f| f.map(Frame::Xml)),
                Mode::Proto => self
                    .proto
                    .split(src, self.max_frame)
                    .map(|f| f.map(Frame::Proto)),
            };
            match framed {
                Ok(frame) => return Ok(frame),
                // The framer has already consumed the offending bytes, so
                // looping looks at whatever came after them.
                Err(FrameError::Oversized(seen)) => {
                    self.dropped = self.dropped.saturating_add(1);
                    self.dropped_bytes = self.dropped_bytes.saturating_add(seen as u64);
                }
                Err(other) => return Err(other.into()),
            }
        }
    }

    /// At end of stream a half-written message is discarded, not reported.
    ///
    /// The default implementation errors on leftover bytes; a peer that closes
    /// mid-message is ordinary on a TAK stream and must not look like a codec
    /// failure to the connection task.
    fn decode_eof(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, CodecError> {
        let frame = self.decode(src)?;
        if frame.is_none() {
            src.clear();
        }
        Ok(frame)
    }
}

impl Encoder<Frame> for TakCodec {
    type Error = CodecError;

    /// Writes a frame in **its own** encoding, not the codec's mode.
    ///
    /// This is the relay path: a frame that arrived on a peer's connection can
    /// be forwarded without being re-encoded, provided both connections agree
    /// on the mode.
    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), CodecError> {
        match item {
            Frame::Xml(bytes) => dst.extend_from_slice(&bytes),
            Frame::Proto(payload) => proto_frame::encode(&payload, dst),
        }
        Ok(())
    }
}

impl<'a> Encoder<&'a EncodedEvent> for TakCodec {
    type Error = CodecError;

    /// Writes a shared event in whichever encoding this connection speaks,
    /// reusing the cached serialisation.
    fn encode(&mut self, item: &'a EncodedEvent, dst: &mut BytesMut) -> Result<(), CodecError> {
        match self.mode {
            Mode::Xml => dst.extend_from_slice(item.xml()),
            Mode::Proto => proto_frame::encode(item.proto(), dst),
        }
        Ok(())
    }
}

/// Writes one protobuf frame — magic, length, payload — to `dst`.
///
/// Exposed for callers that build a frame outside a codec, such as a test
/// harness or a replay tool.
pub fn encode_proto_frame(payload: &[u8], dst: &mut BytesMut) {
    proto_frame::encode(payload, dst);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::CotTime;
    use crate::{Event, xml};

    fn sample() -> Event {
        Event::builder("a-f-G-U-C", "UID-A")
            .how("m-g")
            .point(1.0, 2.0)
            .time(CotTime::from_millis(1_789_646_400_000))
            .stale_after(std::time::Duration::from_secs(60))
            .build()
    }

    #[test]
    fn a_new_codec_starts_in_xml_at_the_documented_cap() {
        let codec = TakCodec::default();
        assert_eq!(codec.mode(), Mode::Xml);
        assert_eq!(codec.max_frame(), MAX_MESSAGE);
        assert_eq!(MAX_MESSAGE, 8 * 1024 * 1024);
        assert_eq!(MAX_PROTO_PAYLOAD, 65_536);
        assert_eq!(MAGIC, 0xBF);
    }

    #[test]
    fn xml_mode_frames_events_and_drops_the_junk_between_them() {
        let mut codec = TakCodec::new(Mode::Xml);
        let mut wire = BytesMut::new();
        codec
            .encode(&EncodedEvent::new(sample()), &mut wire)
            .expect("infallible");
        let decoded = codec
            .decode(&mut wire)
            .expect("well formed")
            .expect("a whole message");

        assert_eq!(decoded.mode(), Mode::Xml);
        assert!(decoded.payload().starts_with(b"<event"));
        assert_eq!(xml::parse(decoded.payload()).expect("valid"), sample());
    }

    #[test]
    fn proto_mode_frames_the_same_event_as_a_takmessage() {
        let mut codec = TakCodec::new(Mode::Proto);
        let encoded = EncodedEvent::new(sample());
        let mut wire = BytesMut::new();
        codec.encode(&encoded, &mut wire).expect("infallible");

        assert_eq!(wire[0], MAGIC);
        let decoded = codec
            .decode(&mut wire)
            .expect("well formed")
            .expect("a whole frame");
        assert_eq!(decoded, Frame::Proto(encoded.proto().clone()));
        assert!(wire.is_empty());
    }

    #[test]
    fn switching_mode_hands_the_same_buffer_to_the_other_framer() {
        let mut codec = TakCodec::new(Mode::Xml);
        let encoded = EncodedEvent::new(sample());

        let mut wire = BytesMut::new();
        codec.encode(&encoded, &mut wire).expect("infallible");
        // The switchover: everything after the last XML message is protobuf.
        codec.set_mode(Mode::Proto);
        codec.encode(&encoded, &mut wire).expect("infallible");

        // The XML message is now junk to the protobuf framer, which skips it.
        let decoded = codec
            .decode(&mut wire)
            .expect("well formed")
            .expect("the protobuf frame");
        assert_eq!(decoded, Frame::Proto(encoded.proto().clone()));
        assert!(codec.skipped() > 0, "the XML prefix was skipped");
    }

    #[test]
    fn a_frame_is_relayed_in_its_own_encoding_whatever_the_mode() {
        let mut codec = TakCodec::new(Mode::Proto);
        let mut wire = BytesMut::new();
        // An XML frame written by a codec in protobuf mode stays XML: this is
        // the relay path, where the caller has already picked the encoding.
        codec
            .encode(
                Frame::Xml(Bytes::from_static(b"<event/></event>")),
                &mut wire,
            )
            .expect("infallible");
        assert_eq!(&wire[..], b"<event/></event>");
    }

    /// One message a peer got wrong must never cost the connection, so the
    /// oversized one is counted and skipped rather than reported: a reported
    /// error would end a `Framed` stream for good.
    #[test]
    fn an_oversized_xml_message_is_dropped_and_counted_not_reported() {
        let mut codec = TakCodec::with_limit(Mode::Xml, 64);
        let mut wire = BytesMut::from(
            &b"<event uid=\"A\" pad=\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"></event><event uid=\"B\"></event>"[..],
        );
        assert_eq!(
            codec
                .decode(&mut wire)
                .expect("a framing failure is not an error")
                .map(Frame::into_payload),
            Some(Bytes::from_static(b"<event uid=\"B\"></event>"))
        );
        assert_eq!(codec.dropped(), 1);
        assert_eq!(codec.dropped_bytes(), 88);
        assert!(wire.is_empty());
    }

    #[test]
    fn end_of_stream_discards_a_half_written_message() {
        let mut codec = TakCodec::new(Mode::Xml);
        let mut wire = BytesMut::from(&b"<event uid=\"A\"><point lat"[..]);
        assert_eq!(codec.decode_eof(&mut wire).expect("no error"), None);
        assert!(wire.is_empty());
    }

    #[test]
    fn the_free_frame_encoder_matches_the_codec() {
        let mut direct = BytesMut::new();
        encode_proto_frame(b"payload", &mut direct);

        let mut through_codec = BytesMut::new();
        TakCodec::new(Mode::Proto)
            .encode(
                Frame::Proto(Bytes::from_static(b"payload")),
                &mut through_codec,
            )
            .expect("infallible");
        assert_eq!(direct, through_codec);
    }

    #[test]
    fn frame_accessors_describe_the_payload() {
        let empty = Frame::Proto(Bytes::new());
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.mode(), Mode::Proto);

        let xml = Frame::Xml(Bytes::from_static(b"<event></event>"));
        assert!(!xml.is_empty());
        assert_eq!(xml.len(), 15);
        assert_eq!(xml.into_payload(), Bytes::from_static(b"<event></event>"));
    }
}
