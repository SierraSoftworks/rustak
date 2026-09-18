//! Record framing for the append-only segment files.
//!
//! A record is a base-128 varint holding the payload's length, followed by that
//! many opaque bytes. The varint is the same encoding protocol buffers use for
//! a `uint64`, so a segment file is exactly the "varint-delimited protobuf
//! stream" that `protoc --decode_raw` and every protobuf library already know
//! how to walk — which matters because the payloads rustak writes are
//! `TakMessage` frames, and an operator debugging a history file should not
//! need a rustak-specific tool to read one.
//!
//! Nothing here knows what a payload contains. That is deliberate: the same log
//! carries CoT history today and telemetry later, and a log that could parse its
//! own records would have to be taught every schema that is ever written to it.

/// The largest payload a single record may carry.
///
/// A cap is what keeps a corrupt or hostile length prefix from turning into a
/// multi-gigabyte allocation when a segment is scanned. Four mebibytes is far
/// above any CoT message and still small enough that a bad frame costs nothing.
pub(super) const MAX_RECORD_BYTES: u64 = 4 * 1024 * 1024;

/// The most bytes a base-128 varint of a `u64` can occupy.
const MAX_VARINT_BYTES: usize = 10;

/// Appends `value` to `buffer` as a base-128 varint.
fn put_varint(buffer: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        buffer.push((value as u8) | 0x80);
        value >>= 7;
    }

    buffer.push(value as u8);
}

/// Reads a base-128 varint, returning its value and how many bytes it used.
///
/// `None` when the bytes run out mid-varint (a torn write) or when the encoding
/// runs past ten bytes (corruption — no `u64` needs an eleventh).
fn read_varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;

    for (index, byte) in bytes.iter().take(MAX_VARINT_BYTES).enumerate() {
        value |= u64::from(byte & 0x7f) << (index * 7);

        if byte & 0x80 == 0 {
            return Some((value, index + 1));
        }
    }

    None
}

/// Encodes one record: its length as a varint, then the payload itself.
pub(super) fn encode_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + MAX_VARINT_BYTES);

    put_varint(&mut frame, payload.len() as u64);
    frame.extend_from_slice(payload);

    frame
}

/// Walks the complete records at the front of a segment file.
///
/// Iteration stops at the first record that is incomplete or malformed rather
/// than reporting an error, because that is the expected state of the newest
/// segment after an unclean shutdown: the process died partway through a write.
/// [`Frames::valid_len`] then reports where the last complete record ended,
/// which is where the file is truncated back to.
pub(crate) struct Frames<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Frames<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    /// The offset just past the last complete record yielded so far.
    ///
    /// Only meaningful once the iterator has been exhausted, at which point
    /// everything from here to the end of the file is a partial or corrupt
    /// frame that was never acknowledged to a caller.
    pub(super) fn valid_len(&self) -> u64 {
        self.offset as u64
    }
}

impl<'a> Iterator for Frames<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        let remaining = self.bytes.get(self.offset..)?;

        let (length, header) = read_varint(remaining)?;

        if length > MAX_RECORD_BYTES {
            return None;
        }

        let start = self.offset.checked_add(header)?;
        let end = start.checked_add(length as usize)?;
        let payload = self.bytes.get(start..end)?;

        self.offset = end;

        Some(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(payloads: &[&[u8]]) -> Vec<u8> {
        payloads.iter().flat_map(|p| encode_frame(p)).collect()
    }

    #[test]
    fn a_varint_round_trips_at_every_width() {
        for value in [
            0u64,
            1,
            127,
            128,
            300,
            16_383,
            16_384,
            u32::MAX as u64,
            u64::MAX,
        ] {
            let mut buffer = Vec::new();
            put_varint(&mut buffer, value);

            assert_eq!(
                read_varint(&buffer),
                Some((value, buffer.len())),
                "{value} should round trip"
            );
        }
    }

    #[test]
    fn small_lengths_use_a_single_byte_the_way_protobuf_does() {
        // The framing is only useful to other protobuf tooling if it *is* the
        // protobuf varint, so pin the encoding rather than just the round trip.
        let mut buffer = Vec::new();
        put_varint(&mut buffer, 300);

        assert_eq!(buffer, vec![0xac, 0x02]);
        assert_eq!(encode_frame(b"hi"), vec![0x02, b'h', b'i']);
        assert_eq!(encode_frame(b""), vec![0x00]);
    }

    #[test]
    fn every_complete_record_is_yielded_in_order() {
        let bytes = segment(&[b"one", b"", b"three"]);

        let mut frames = Frames::new(&bytes);
        let read: Vec<_> = frames.by_ref().collect();

        assert_eq!(read, vec![&b"one"[..], &b""[..], &b"three"[..]]);
        assert_eq!(frames.valid_len(), bytes.len() as u64);
    }

    #[test]
    fn a_torn_payload_stops_the_scan_at_the_last_whole_record() {
        let mut bytes = segment(&[b"kept", b"lost"]);
        bytes.truncate(bytes.len() - 2);

        let mut frames = Frames::new(&bytes);
        let read: Vec<_> = frames.by_ref().collect();

        assert_eq!(read, vec![&b"kept"[..]]);
        assert_eq!(
            frames.valid_len(),
            5,
            "the length byte plus four payload bytes"
        );
    }

    #[test]
    fn a_torn_length_prefix_stops_the_scan() {
        let mut bytes = segment(&[b"kept"]);
        // A length prefix whose continuation bit promises a byte that is not
        // there: the crash landed between two write syscalls.
        bytes.push(0x80);

        let mut frames = Frames::new(&bytes);
        assert_eq!(frames.by_ref().count(), 1);
        assert_eq!(frames.valid_len(), 5);
    }

    #[test]
    fn an_absurd_length_is_refused_rather_than_allocated() {
        let mut bytes = Vec::new();
        put_varint(&mut bytes, MAX_RECORD_BYTES + 1);
        bytes.extend_from_slice(b"whatever");

        let mut frames = Frames::new(&bytes);

        assert_eq!(frames.by_ref().count(), 0);
        assert_eq!(frames.valid_len(), 0);
    }

    #[test]
    fn a_varint_longer_than_ten_bytes_is_corruption() {
        assert_eq!(read_varint(&[0x80; MAX_VARINT_BYTES + 2]), None);
    }

    #[test]
    fn an_empty_segment_yields_nothing() {
        let mut frames = Frames::new(&[]);

        assert_eq!(frames.by_ref().count(), 0);
        assert_eq!(frames.valid_len(), 0);
    }
}
