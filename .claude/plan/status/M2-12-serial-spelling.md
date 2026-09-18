# M2-12 — One spelling for a certificate serial — complete

Defect: `.claude/plan/status/CI-01-2026-09-18-serial-hex-der-minimal.md`.
Read first: `conventions.md`; `rustak-server/src/pki/{serial.rs,issue.rs,ca.rs,server_cert.rs,tls/peer.rs}`,
`rustak-server/src/auth/cert.rs`.

CI-01 caught `pki::tls::peer::tests::a_certificate_yields_the_identity_the_handshake_proved`
failing with `left: 30, right: 32` — a serial rendered two characters short. The test was
correctly red: rustak spelled the same serial two ways, and roughly 1 certificate in 128 was
spelled differently by each.

## What the defect was

`random_serial` clears the top bit of a 128-bit serial so the DER integer stays positive, which
leaves the leading byte uniform over `0x00..=0x7f` — zero once in 128. DER integers are minimal
and `yasna::write_bigint_bytes` strips **every** leading zero, so such a certificate carries a
15-byte (or shorter) serial. Issuance stored `hex::encode` of its own 16-byte array (32 chars);
`pki::tls::peer` hex-encoded `x509_parser`'s `raw_serial()`, which is the content octets as
carried (30 chars). Authentication and revocation never noticed, because both key off the SHA-256
fingerprint — but `auth/cert.rs` records the handshake's serial in
`AuthMethod::ClientCert { fingerprint, serial }`, so for those certificates every audit entry
named a serial that matched nothing in `GET /api/v1/certificates`.

## What was built

| File | Functional lines (limit 300) | What changed |
|---|---:|---|
| `rustak-server/src/pki/serial.rs` | 20 | **New.** The one definition: `SERIAL_BYTES`, `random_serial()` and `serial_hex()`, which renders the *value* as fixed-width lowercase hex, left-padded to 16 bytes. 6 unit tests |
| `rustak-server/src/pki/mod.rs` | 29 | `pub mod serial;`, the module table row, and `pub use serial::{SERIAL_BYTES, random_serial, serial_hex}` |
| `rustak-server/src/pki/tls/peer.rs` | 73 | `serial_hex(certificate.raw_serial())` instead of `hex::encode(..)`; 3 new tests, 2 of them deterministic |
| `rustak-server/src/pki/issue.rs` | 178 | Its private `SERIAL_BYTES`/`random_serial` are gone; the stored serial and the `Issued a client certificate.` log line both go through `serial_hex`. 1 new assertion |
| `rustak-server/src/pki/ca.rs` | 278 | Same: the duplicate constant and generator are gone, the CA's own `serial_hex` goes through the helper |
| `rustak-server/src/pki/server_cert.rs` | 242 | Its third copy of `random_serial` (with a bare `16` rather than the constant) is gone |
| `rustak-server/src/pki/testing.rs` | — | `TestClient` carries `serial_hex`, so a test can hold the handshake's reading against what issuance recorded |

`auth/cert.rs` is **unchanged**: it clones `peer.serial_hex`, which is now the padded spelling, so
the audit trail is fixed without touching it. No migration: for every serial rustak has ever
issued, `serial_hex` of the 16-byte array is byte-for-byte what `hex::encode` produced, so
existing `certificates.serial_hex` rows are already correct and it is only the reader that moves.

## Decisions

### The helper spells the value, not the octets

The write-up's sketch padded anything shorter than 16 bytes and returned anything longer whole.
`serial_hex` strips leading zeros first and then pads, which additionally handles the mirror
case: a foreign authority that does *not* clear the top bit has its serial encoded with a `0x00`
sign pad, so `raw_serial()` hands back **17** bytes for a 128-bit value. Both directions now
render 32 characters for the same certificate. A serial whose value really is wider than ours is
still rendered whole rather than truncated, because a truncated serial names a different
certificate.

### Generation moved too, rather than only the rendering

`random_serial` existed three times (`issue.rs`, `ca.rs`, `server_cert.rs` — the third with a
literal `16` instead of the constant), and it is the top-bit clearing that makes the zero leading
byte reachable at all. Keeping the generator and the renderer in one file puts the invariant and
its consequence next to each other, and removes the room for a fourth divergent copy. The
generation itself is unchanged, byte for byte.

Rerolling a zero leading byte was rejected for the reason the write-up gives: it would fix new
certificates and leave every existing one able to mismatch, and it would make the invariant
depend on generation rather than on rendering.

### The tests

The existing assertion in `a_certificate_yields_the_identity_the_handshake_proved` is untouched
— it is the probabilistic detector, and it stays. Added alongside it:

- `pki::tls::peer::a_serial_der_shortened_by_a_leading_zero_is_read_back_at_full_width` —
  deterministic: builds a certificate carrying serial `00 11 22 … ff`, asserts that it really
  does carry only 15 octets, and that the extractor still answers
  `00112233445566778899aabbccddeeff`.
- `…_shortened_by_several_leading_zeros_…` — three zero bytes, because the encoder drops all of
  them and padding one byte back would not be enough.
- `pki::tls::peer::the_serial_read_off_a_certificate_is_the_one_issuance_recorded` — the round
  trip: what `issue_client_cert` stored equals what `PeerCertificate::from_der` reads.
- `pki::issue::the_serial_is_a_hundred_and_twenty_eight_random_bits` gained the same round-trip
  assertion from the issuance side.
- 6 unit tests in `pki::serial` covering full width, one zero, several zeros, the all-zero serial,
  an empty slice, a sign-padded foreign serial and one wider than ours.

## Checks

```
$ cargo test -p rustak-server --features testing -- pki:: auth::cert
test pki::serial::tests::a_full_width_serial_is_rendered_as_it_is ... ok
test pki::serial::tests::a_der_minimal_serial_is_padded_back_to_the_width_it_was_issued_at ... ok
test pki::serial::tests::a_padded_serial_and_the_array_it_came_from_agree ... ok
test pki::serial::tests::a_foreign_serial_wider_than_ours_is_kept_whole ... ok
test pki::serial::tests::a_foreign_serial_carrying_a_sign_pad_byte_is_still_a_hundred_and_twenty_eight_bits ... ok
test pki::serial::tests::a_generated_serial_is_a_hundred_and_twenty_eight_positive_bits ... ok
test pki::tls::peer::tests::a_serial_der_shortened_by_a_leading_zero_is_read_back_at_full_width ... ok
test pki::tls::peer::tests::a_serial_der_shortened_by_several_leading_zeros_is_read_back_at_full_width ... ok
test pki::tls::peer::tests::the_serial_read_off_a_certificate_is_the_one_issuance_recorded ... ok
test pki::tls::peer::tests::a_certificate_yields_the_identity_the_handshake_proved ... ok
test pki::issue::tests::the_serial_is_a_hundred_and_twenty_eight_random_bits ... ok
test pki::ca::tests::the_serial_is_128_bits_and_positive ... ok
...
test result: ok. 203 passed; 0 failed; 1 ignored; 0 measured; 1379 filtered out; finished in 11.59s

$ cargo clippy -p rustak-server --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 32.11s

$ cargo fmt --all --check
(no output)

$ ./scripts/check-file-length.sh
(no output)
```

## What this does not fix

The 1-in-128 roll is now a rendering that agrees rather than a rendering that differs, but nothing
was done about the generation, so a serial whose leading byte is zero is still issued at the same
rate — by design. `web/api/certificates.rs` and `db/repos/certificates/list.rs` build test
fixtures with `format!("{:032x}", ..)`, which is the same spelling by construction; they are
another brief's files and were left alone.
