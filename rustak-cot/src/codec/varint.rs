//! LEB128 varints — the length prefix of a TAK Protocol v1 frame.
//!
//! This is the same base-128 encoding protobuf uses for its own `uint64`
//! fields: seven payload bits per byte, little-endian, with the top bit set on
//! every byte but the last.
//!
//! The decoder is deliberately stricter than TAK Server's, which has no length
//! cap at all: a prefix that runs past ten bytes, or whose final byte carries
//! bits that would not fit in a `u64`, is rejected as [`FrameError::BadVarint`]
//! so the framer can resynchronise instead of buffering a garbage length.

use bytes::{BufMut, BytesMut};

use crate::error::FrameError;

/// The most bytes a `u64` can occupy: nine full groups of seven bits plus one.
pub const MAX_LEN: usize = 10;

/// Appends `value` to `dst` in LEB128 form.
pub fn put_varint(mut value: u64, dst: &mut BytesMut) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            dst.put_u8(byte);
            return;
        }
        dst.put_u8(byte | 0x80);
    }
}

/// Reads a LEB128 varint from the front of `buf`.
///
/// Returns the value and how many bytes it occupied, or `None` when `buf` ends
/// mid-varint and the caller should wait for more of the stream.
///
/// # Errors
///
/// [`FrameError::BadVarint`] when the prefix is longer than [`MAX_VARINT_LEN`](crate::codec::MAX_VARINT_LEN) bytes
/// or its last byte sets bits above 2^63 — neither can be a length this crate
/// will ever emit, so the only sane reaction is to resynchronise.
pub fn read_varint(buf: &[u8]) -> Result<Option<(u64, usize)>, FrameError> {
    let mut value: u64 = 0;
    for (index, &byte) in buf.iter().take(MAX_LEN).enumerate() {
        // The tenth byte contributes bit 63 and nothing else, so anything
        // above `0x01` either overflows or claims an eleventh byte.
        if index == MAX_LEN - 1 && byte > 0x01 {
            return Err(FrameError::BadVarint);
        }
        value |= u64::from(byte & 0x7F) << (7 * index);
        if byte & 0x80 == 0 {
            return Ok(Some((value, index + 1)));
        }
    }

    if buf.len() >= MAX_LEN {
        Err(FrameError::BadVarint)
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(value: u64) -> Vec<u8> {
        let mut buffer = BytesMut::new();
        put_varint(value, &mut buffer);
        buffer.to_vec()
    }

    #[test]
    fn single_byte_values_are_their_own_encoding() {
        assert_eq!(encoded(0), vec![0x00]);
        assert_eq!(encoded(1), vec![0x01]);
        assert_eq!(encoded(127), vec![0x7F]);
    }

    #[test]
    fn the_boundaries_between_byte_widths_round_trip() {
        for value in [
            0,
            1,
            127,
            128,
            129,
            16_383,
            16_384,
            2_097_151,
            2_097_152,
            65_536,
            u32::MAX as u64,
            u64::MAX / 2,
            u64::MAX,
        ] {
            let bytes = encoded(value);
            assert_eq!(
                read_varint(&bytes),
                Ok(Some((value, bytes.len()))),
                "{value} did not round trip through {bytes:?}"
            );
        }
    }

    #[test]
    fn the_widest_value_uses_exactly_ten_bytes() {
        assert_eq!(encoded(u64::MAX).len(), MAX_LEN);
        assert_eq!(encoded(u64::MAX / 2).len(), 9);
    }

    #[test]
    fn a_truncated_prefix_asks_for_more_bytes() {
        let full = encoded(300_000);
        for take in 0..full.len() {
            assert_eq!(
                read_varint(&full[..take]),
                Ok(None),
                "{take} of {} bytes should be incomplete",
                full.len()
            );
        }
        assert_eq!(read_varint(&full), Ok(Some((300_000, full.len()))));
    }

    #[test]
    fn a_prefix_longer_than_ten_bytes_is_rejected() {
        let eleven = [0xFF_u8; 11];
        assert_eq!(read_varint(&eleven), Err(FrameError::BadVarint));
        // Exactly ten continuation bytes is the same error: there is no
        // eleventh byte a `u64` could use.
        assert_eq!(read_varint(&eleven[..MAX_LEN]), Err(FrameError::BadVarint));
    }

    #[test]
    fn a_tenth_byte_that_would_overflow_is_rejected() {
        let mut overflowing = encoded(u64::MAX);
        overflowing[MAX_LEN - 1] = 0x02;
        assert_eq!(read_varint(&overflowing), Err(FrameError::BadVarint));
    }

    #[test]
    fn trailing_bytes_after_the_varint_are_left_for_the_caller() {
        let mut buffer = encoded(5);
        buffer.extend_from_slice(b"payload");
        assert_eq!(read_varint(&buffer), Ok(Some((5, 1))));
    }

    #[test]
    fn a_non_minimal_encoding_still_decodes() {
        // Padding a zero out to nine bytes is legal LEB128 even though we
        // never emit it; peers that do must not desynchronise the stream.
        let padded = [0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00];
        assert_eq!(read_varint(&padded), Ok(Some((0, 9))));
    }
}
