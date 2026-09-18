//! Splitting a byte stream into TAK Protocol v1 frames.
//!
//! ```text
//! frame := 0xBF <LEB128 payload length> <payload: serialised TakMessage>
//! ```
//!
//! The mesh/UDP shape (`0xBF <version> 0xBF <payload>`) is a *different*
//! framing and never appears on a stream socket, so this scanner must never be
//! pointed at one.
//!
//! Unlike TAK Server — which logs an error and then quietly stops making
//! progress on a bad magic byte — this scanner **resynchronises**: it skips
//! forward to the next `0xBF` whenever the magic is wrong, the length prefix is
//! malformed, or the length is implausible. ATAK does the same, and it is the
//! only behaviour that survives a peer that gets its own framing wrong once.
//! [`ProtoScanner::skipped`] counts the discarded bytes so a connection can
//! report the damage.

use bytes::{Buf, BufMut, Bytes, BytesMut};

use super::varint::{put_varint, read_varint};
use crate::error::FrameError;

/// The byte that opens every stream frame.
pub const MAGIC: u8 = 0xBF;

/// Incremental frame scanner over a growing buffer.
///
/// One scanner belongs to one connection; it holds only the resync counter,
/// because a frame carries its own length and needs no parse state.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProtoScanner {
    skipped: u64,
}

impl ProtoScanner {
    /// A scanner positioned at the start of a stream.
    #[must_use]
    pub const fn new() -> Self {
        Self { skipped: 0 }
    }

    /// How many bytes this scanner has discarded while resynchronising.
    #[must_use]
    pub const fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Takes the next complete frame's payload from `buf`, if there is one.
    ///
    /// The magic byte and the length prefix are consumed; the returned bytes
    /// are exactly the serialised `TakMessage`.
    ///
    /// # Errors
    ///
    /// Never — a malformed frame resynchronises rather than failing. The
    /// signature keeps [`FrameError`] so that callers can treat both framers
    /// alike, and so a future hard limit does not change it.
    pub fn split(&mut self, buf: &mut BytesMut, max: usize) -> Result<Option<Bytes>, FrameError> {
        loop {
            let Some(at) = memchr::memchr(MAGIC, &buf[..]) else {
                self.skip(buf.len());
                buf.clear();
                return Ok(None);
            };
            if at > 0 {
                self.skip(at);
                buf.advance(at);
            }

            let (payload_len, prefix_len) = match read_varint(&buf[1..]) {
                // The prefix is not a varint we would ever emit: the magic
                // byte was almost certainly payload, so step over it.
                Err(_) => {
                    self.skip(1);
                    buf.advance(1);
                    continue;
                }
                Ok(None) => return Ok(None),
                Ok(Some((claimed, used))) => match usize::try_from(claimed) {
                    Ok(length) if length <= max => (length, used),
                    // Implausible length: same reasoning, same resync.
                    _ => {
                        self.skip(1);
                        buf.advance(1);
                        continue;
                    }
                },
            };

            let header = 1 + prefix_len;
            if buf.len() < header + payload_len {
                return Ok(None);
            }
            buf.advance(header);
            return Ok(Some(buf.split_to(payload_len).freeze()));
        }
    }

    fn skip(&mut self, bytes: usize) {
        self.skipped = self.skipped.saturating_add(bytes as u64);
    }
}

/// Writes one frame — magic, length, payload — to `dst`.
pub fn encode(payload: &[u8], dst: &mut BytesMut) {
    dst.reserve(payload.len() + 1 + super::varint::MAX_LEN);
    dst.put_u8(MAGIC);
    put_varint(payload.len() as u64, dst);
    dst.extend_from_slice(payload);
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 8 * 1024 * 1024;

    fn framed(payload: &[u8]) -> BytesMut {
        let mut buffer = BytesMut::new();
        encode(payload, &mut buffer);
        buffer
    }

    #[test]
    fn a_frame_round_trips_through_its_own_encoder() {
        let mut buffer = framed(b"hello");
        assert_eq!(
            &buffer[..],
            &[MAGIC, 0x05, b'h', b'e', b'l', b'l', b'o'][..]
        );

        let mut scanner = ProtoScanner::new();
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(&b"hello"[..])
        );
        assert!(buffer.is_empty());
        assert_eq!(scanner.skipped(), 0);
    }

    #[test]
    fn a_long_payload_uses_a_multi_byte_length() {
        let payload = vec![0x11_u8; 300];
        let mut buffer = framed(&payload);
        assert_eq!(&buffer[..3], &[MAGIC, 0xAC, 0x02][..]);

        let mut scanner = ProtoScanner::new();
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(&payload[..])
        );
    }

    #[test]
    fn a_zero_length_payload_is_a_real_frame() {
        let mut buffer = framed(b"");
        let mut scanner = ProtoScanner::new();
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(&b""[..])
        );
        assert!(buffer.is_empty());
    }

    #[test]
    fn a_partial_varint_waits_for_the_rest_of_the_stream() {
        let payload = vec![0x22_u8; 300];
        let whole = framed(&payload);
        let mut scanner = ProtoScanner::new();
        let mut buffer = BytesMut::from(&whole[..2]); // magic + first length byte
        assert_eq!(scanner.split(&mut buffer, CAP).unwrap(), None);
        assert_eq!(buffer.len(), 2, "nothing is consumed while waiting");

        buffer.extend_from_slice(&whole[2..]);
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(&payload[..])
        );
    }

    #[test]
    fn a_payload_split_across_reads_is_reassembled() {
        let whole = framed(b"0123456789");
        let mut scanner = ProtoScanner::new();
        let mut buffer = BytesMut::new();
        let mut out = None;
        for byte in &whole[..] {
            buffer.extend_from_slice(&[*byte]);
            if let Some(frame) = scanner.split(&mut buffer, CAP).unwrap() {
                out = Some(frame);
            }
        }
        assert_eq!(out.as_deref(), Some(&b"0123456789"[..]));
        assert_eq!(scanner.skipped(), 0);
    }

    #[test]
    fn garbage_before_the_magic_byte_is_counted_and_skipped() {
        let mut buffer = BytesMut::from(&b"junk"[..]);
        buffer.extend_from_slice(&framed(b"hi"));

        let mut scanner = ProtoScanner::new();
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(&b"hi"[..])
        );
        assert_eq!(scanner.skipped(), 4);
    }

    #[test]
    fn junk_with_no_magic_byte_never_accumulates() {
        let mut scanner = ProtoScanner::new();
        let mut buffer = BytesMut::new();
        for _ in 0..10 {
            buffer.extend_from_slice(&[0x01; 64]);
            assert_eq!(scanner.split(&mut buffer, CAP).unwrap(), None);
            assert!(buffer.is_empty());
        }
        assert_eq!(scanner.skipped(), 640);
    }

    #[test]
    fn an_eleven_byte_length_prefix_resynchronises() {
        let mut buffer = BytesMut::from(&[MAGIC][..]);
        buffer.extend_from_slice(&[0xFF; 11]);
        buffer.extend_from_slice(&framed(b"after"));

        let mut scanner = ProtoScanner::new();
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(&b"after"[..])
        );
        assert_eq!(scanner.skipped(), 12, "the magic and its garbage prefix");
    }

    #[test]
    fn a_length_past_the_cap_resynchronises() {
        let mut buffer = BytesMut::new();
        encode(&[0x33; 200], &mut buffer); // claims 200 bytes
        buffer.truncate(2); // keep only the header, claiming more than the cap
        buffer.extend_from_slice(&framed(b"after"));

        let mut scanner = ProtoScanner::new();
        assert_eq!(
            scanner.split(&mut buffer, 64).unwrap().as_deref(),
            Some(&b"after"[..])
        );
        assert!(scanner.skipped() >= 1, "the bogus header was discarded");
    }

    #[test]
    fn a_magic_byte_inside_a_payload_is_not_a_frame_boundary() {
        let payload = [0x01, MAGIC, MAGIC, 0x02, MAGIC];
        let mut buffer = framed(&payload);
        buffer.extend_from_slice(&framed(b"next"));

        let mut scanner = ProtoScanner::new();
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(&payload[..])
        );
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(&b"next"[..])
        );
        assert_eq!(scanner.skipped(), 0);
    }

    #[test]
    fn two_frames_in_one_chunk_are_both_returned() {
        let mut buffer = framed(b"one");
        buffer.extend_from_slice(&framed(b"two"));

        let mut scanner = ProtoScanner::new();
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(&b"one"[..])
        );
        assert_eq!(
            scanner.split(&mut buffer, CAP).unwrap().as_deref(),
            Some(&b"two"[..])
        );
        assert_eq!(scanner.split(&mut buffer, CAP).unwrap(), None);
    }

    #[test]
    fn a_lone_magic_byte_waits_rather_than_resyncing() {
        let mut scanner = ProtoScanner::new();
        let mut buffer = BytesMut::from(&[MAGIC][..]);
        assert_eq!(scanner.split(&mut buffer, CAP).unwrap(), None);
        assert_eq!(scanner.skipped(), 0);
        assert_eq!(buffer.len(), 1);
    }
}
