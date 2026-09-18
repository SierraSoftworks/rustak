# CI-01 — A certificate's serial is recorded two different ways, and 1 in 128 of them disagree

**Found by:** `rust.yml` run
[35390132588](https://github.com/SierraSoftworks/rustak/actions/runs/35390132588)
(wave A, `36c1411`), `Test` job — `1568 passed; 1 failed`.

```
---- pki::tls::peer::tests::a_certificate_yields_the_identity_the_handshake_proved ----
panicked at rustak-server/src/pki/tls/peer.rs:140:9:
assertion `left == right` failed: a 128-bit serial
  left: 30
 right: 32
```

**Owner:** whoever holds `rustak-server/src/pki/`. CI-01 does not edit non-test
`src/`, and **the test is correctly red** — it asserts a real invariant that the
product violates — so it has been left failing rather than relaxed, per the
brief.

**This is not a wave A regression.** It is a latent flake that wave A happened to
roll. It has been reachable since serials were introduced and will recur at a
rate of about **1 run in 128**.

---

## 1. Why 30 and not 32

`rustak-server/src/pki/issue.rs:317`:

```rust
/// A 128-bit serial with the top bit cleared, so its DER encoding stays
/// positive without a leading pad byte.
fn random_serial() -> [u8; SERIAL_BYTES] {          // SERIAL_BYTES = 16
    let mut serial = [0u8; SERIAL_BYTES];
    rand::rng().fill_bytes(&mut serial);
    serial[0] &= 0x7f;

    serial
}
```

Clearing the top bit leaves the leading byte uniform over `0x00..=0x7f`, so it
is **`0x00` once in 128**. DER integers are minimal-length: a leading zero byte
followed by a byte under `0x80` is not permitted, so the encoder drops it and the
certificate carries a 15-byte serial.

The two sides then disagree:

| Where | Expression | Bytes | Hex length |
|---|---|---|---|
| `pki/issue.rs:207` (what is **stored**) | `hex::encode(serial)` — the array | always 16 | always 32 |
| `pki/tls/peer.rs:62` (what is **read off the handshake**) | `hex::encode(certificate.raw_serial())` — the DER content octets | 15 or 16 | **30 or 32** |

`pki/ca.rs:361` stores the CA's own serial the same way `issue.rs` does, so it is
consistent with the row and inconsistent with `peer.rs` in the same way.

## 2. What it actually costs

Not authentication, and not revocation: both look a certificate up by
**fingerprint**, not serial (`pki::tls::client_verifier`, and the revocation hook
closes by fingerprint — visible in nightly 35388998399, where a revoked EUD was
refused at three successive handshakes).

What it costs is the **audit trail**. `auth/cert.rs:161` puts the handshake's
serial into `AuthMethod::ClientCert { fingerprint, serial }`, which is what a
principal's authentication is recorded as. So for 1 certificate in 128, every
audit entry naming that certificate carries a serial two characters shorter than
the one on the certificate row and in `GET /api/v1/certificates` — and an
administrator correlating the two by serial finds nothing. Silent, rare and
exactly the kind of thing nobody debugs successfully at the time.

## 3. The fix I believe is right

Make `peer.rs` render what was issued, by left-padding the DER-minimal serial
back to `SERIAL_BYTES`:

```rust
// DER integers are minimal, so a serial whose leading byte is zero — one in
// 128, since `random_serial` clears the top bit — comes back a byte shorter
// than the sixteen that were issued. `pki::issue` and `pki::ca` both store the
// full sixteen, so pad to match or the audit trail and the certificate row
// disagree about the same certificate.
fn serial_hex(raw: &[u8]) -> String {
    if raw.len() >= SERIAL_BYTES {
        // A foreign CA's serial may legitimately be longer; keep it whole.
        return hex::encode(raw);
    }

    let mut serial = [0u8; SERIAL_BYTES];
    serial[SERIAL_BYTES - raw.len()..].copy_from_slice(raw);

    hex::encode(serial)
}
```

`SERIAL_BYTES` is private to `pki::issue` today, so it wants promoting to the
`pki` module (or a small `pki::serial` helper both sides call — better, because
then there is one definition of "how rustak spells a serial" rather than two
that agree by inspection).

The alternative — having `random_serial` reroll a zero leading byte — is worse:
it would fix new certificates and leave every existing one still able to
mismatch, and it makes the invariant depend on generation rather than on
rendering.

## 4. The test to add with it

`pki::tls::peer`'s existing case is a probabilistic one: it only catches this on
the 1-in-128 roll, which is why it took until now. Worth a deterministic sibling
that feeds a known DER-minimal serial through the renderer, e.g.
`serial_hex(&[0x01, 0x02])` is `"0000…0102"` (32 characters), alongside a
round-trip case asserting that what `issue` stored and what `peer` reads back
are the same string. The existing assertion at `peer.rs:140` should stay exactly
as it is.

## 5. Meanwhile

The `Test` job will fail roughly one run in 128 until this lands, always on this
one test, always with `left: 30`. A re-run is a legitimate response to seeing it
— but it is a real defect, not a flaky test, and relaxing the assertion would
throw away the only thing currently detecting it.
