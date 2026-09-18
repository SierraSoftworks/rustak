//! How rustak spells a certificate's serial number.
//!
//! A serial is a 128-bit integer, and there are two ways to arrive at one: the
//! sixteen bytes we drew when we issued the certificate, and the content octets
//! DER carried them in. They are not the same bytes. DER integers are minimal,
//! so a serial whose leading byte is zero — one in 128, because
//! [`random_serial`] clears the top bit to keep the integer positive — travels
//! as fifteen bytes, and the encoder drops every leading zero, not just one.
//!
//! Rendering each side with `hex::encode` therefore produces two different
//! strings for the same certificate. Nothing about authentication or revocation
//! notices, because both look a certificate up by its SHA-256 fingerprint. The
//! audit trail does: `AuthMethod::ClientCert` records the serial read off the
//! handshake, and an administrator correlates that against the `certificates`
//! row — which holds what issuance stored — by string equality.
//!
//! So there is one definition here and everything that renders a serial calls
//! it. [`serial_hex`] answers fixed-width lowercase hexadecimal, left-padded to
//! [`SERIAL_BYTES`], whether it is handed the array we generated or the octets
//! a peer presented.

use rand::Rng as _;

/// How many bytes of randomness a serial number carries.
pub const SERIAL_BYTES: usize = 16;

/// A 128-bit serial with the top bit cleared, so its DER encoding stays
/// positive without a leading pad byte.
///
/// The clearing is what makes a zero leading byte possible at all, which is why
/// [`serial_hex`] exists; rerolling until the leading byte is non-zero was the
/// alternative, and it would leave every certificate issued before the change
/// still able to disagree with its own audit entries.
pub fn random_serial() -> [u8; SERIAL_BYTES] {
    let mut serial = [0u8; SERIAL_BYTES];
    rand::rng().fill_bytes(&mut serial);
    serial[0] &= 0x7f;

    serial
}

/// Renders a serial as rustak spells it everywhere: lowercase hexadecimal,
/// left-padded to [`SERIAL_BYTES`].
///
/// Takes both the array issuance generated and the DER content octets a
/// certificate carries, and answers the same string for both, because it spells
/// the *value* rather than the octets: DER drops the leading zeros from ours,
/// and adds a pad byte to a foreign serial whose top bit is set, and neither
/// changes which certificate is being named.
///
/// A serial whose value is wider than [`SERIAL_BYTES`] — which a certificate
/// from somebody else's authority may legitimately have — is rendered whole
/// rather than truncated, because a truncated serial would name the wrong
/// certificate.
pub fn serial_hex(raw: &[u8]) -> String {
    let value = match raw.iter().position(|byte| *byte != 0) {
        Some(first) => &raw[first..],
        None => &[][..],
    };

    if value.len() >= SERIAL_BYTES {
        return hex::encode(value);
    }

    let mut serial = [0u8; SERIAL_BYTES];
    serial[SERIAL_BYTES - value.len()..].copy_from_slice(value);

    hex::encode(serial)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_width_serial_is_rendered_as_it_is() {
        let serial = [
            0x7f, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];

        assert_eq!(
            serial_hex(&serial),
            "7f112233445566778899aabbccddeeff",
            "the bytes, lowercase, nothing added"
        );
    }

    #[test]
    fn a_der_minimal_serial_is_padded_back_to_the_width_it_was_issued_at() {
        // What DER carries for a serial whose leading byte was zero.
        assert_eq!(
            serial_hex(&[
                0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
                0xff
            ]),
            "00112233445566778899aabbccddeeff"
        );
        // DER drops every leading zero, not only the first one.
        assert_eq!(
            serial_hex(&[0x01, 0x02]),
            "00000000000000000000000000000102"
        );
        assert_eq!(serial_hex(&[0x00]), "0".repeat(32), "the zero serial");
        assert_eq!(serial_hex(&[]), "0".repeat(32), "nothing at all");
    }

    #[test]
    fn a_padded_serial_and_the_array_it_came_from_agree() {
        let issued = random_serial();
        let minimal: Vec<u8> = issued
            .iter()
            .copied()
            .skip_while(|byte| *byte == 0)
            .collect();

        assert_eq!(serial_hex(&issued), serial_hex(&minimal));
        assert_eq!(serial_hex(&issued).len(), SERIAL_BYTES * 2);
    }

    #[test]
    fn a_foreign_serial_wider_than_ours_is_kept_whole() {
        let wide = [0xab; SERIAL_BYTES + 4];

        assert_eq!(serial_hex(&wide), "ab".repeat(SERIAL_BYTES + 4));
    }

    #[test]
    fn a_foreign_serial_carrying_a_sign_pad_byte_is_still_a_hundred_and_twenty_eight_bits() {
        // rustak clears the top bit, but an authority that does not has its
        // serial encoded with a leading zero so the integer stays positive.
        // That pad byte is DER's, not part of the value.
        let mut padded = vec![0x00];
        padded.extend_from_slice(&[0xff; SERIAL_BYTES]);

        assert_eq!(serial_hex(&padded), "ff".repeat(SERIAL_BYTES));
    }

    #[test]
    fn a_generated_serial_is_a_hundred_and_twenty_eight_positive_bits() {
        let serial = random_serial();

        assert_eq!(serial.len(), SERIAL_BYTES);
        assert!(
            serial[0] < 0x80,
            "a set top bit would encode as a negative integer"
        );
        assert_ne!(serial, random_serial(), "and it is drawn, not fixed");
    }
}
