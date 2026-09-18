# M2-01 — PKI issuance: CSR parsing, signing, revocation, PKCS#12, client-cert verifier — complete

Brief: `.claude/plan/briefs/M2-01-pki-issuance.md`
Read first: `conventions.md`; `compat/enrollment.md`; `plan.md` → Identity & auth model, "Design
artefacts and reconciled decisions" (TLS row); `design/03-identity-pki-acme-auth.md` §0–§3, §9;
status files `M0-07`, `M0-08`, `M0-10`, `M0-11`.

## What was built

Seven new files under `rustak-server/src/pki/` plus a four-file `tls/` tree, on top of the
`keys`/`ca`/`pem`/`server_cert` that M0-10 and M0-11 landed. Nothing outside `pki/**` and the two
manifests was touched; `db/repos/certificates.rs` already had every query needed and was left alone.

| File | Functional lines | Contents |
|---|---:|---|
| `pki/mod.rs` | 25 | module doc (the table of files; "we choose the subject") + `mod`/`pub use` |
| `pki/csr.rs` | 269 | `CsrEncoding`, `CsrKey`, `ParsedCsr`, `parse_csr`, `CsrPolicy`, `warn_on_subject_mismatch` |
| `pki/issue.rs` | 185 | `IssueRequest`, `IssuedCert`, `issue_client_cert`, `CHANNELS_MARKER_OID` |
| `pki/revoke.rs` | 228 | `CertRejection`, `RevokeReason`, `RevocationHook`, `RevocationCache`, `revoke`, `revoke_for_credential` |
| `pki/p12.rs` | 133 | `P12Options`, `client_keystore`, `truststore`, `legacy_signclient_v1` |
| `pki/facade.rs` | 219 | `Pki`, `Enrollment`, `IssuedVia` |
| `pki/testing.rs` | 104 | `TestAuthority`, `TestClient` (feature/`cfg(test)` gated) |
| `pki/tls/mod.rs` | 92 | `ListenerKind`, `public_/marti_/stream_server_config`, `server_config`, `install_crypto_provider` |
| `pki/tls/resolver.rs` | 69 | `HotSwapCertResolver`, `TlsAlpnChallenge`, `ACME_TLS_ALPN` |
| `pki/tls/client_verifier.rs` | 101 | `RustakClientVerifier` |
| `pki/tls/peer.rs` | 72 | `PeerCertificate`, `on_connect_capture`, `from_tokio_rustls`, `common_name` |

**146 unit tests** under `pki::` (145 run, 1 `#[ignore]`d OpenSSL compat check), each file with its
single trailing column-0 `#[cfg(test)] mod tests`. The whole server crate is 788 tests, green.

### Manifest changes

One dependency added, `actix-tls = { version = "3.6.0", default-features = false, features =
["accept", "rustls-0_23"] }` in `[workspace.dependencies]` and `actix-tls = { workspace = true }` in
`rustak-server`. It was **already in the lock file** — `actix-web`'s `rustls-0_23` feature pulls it
in — but `on_connect` hands a handler `&dyn Any`, and downcasting that to
`actix_tls::accept::rustls_0_23::TlsStream<TcpStream>` needs the crate named. Nothing else was
needed: `p12-keystore`, `x509-parser` and `rustls-pki-types` were all already declared.

`config.example.toml` was **not** touched — `[pki]` already documents every key this brief reads
(`name_entries`, `client_cert_validity`, `require_known_cert`, `csr_min_rsa_bits`,
`csr_allow_ecdsa`, `p12_password`, `p12_legacy`, `channels_marker_eku`).

## The `Pki` facade API (for the enrollment-endpoints brief)

`pki::Pki` is the handle `marti/tls.rs` and `web/api/certificates.rs` should hold, behind an `Arc`.

```rust
// Construction — start-up, after the database and the secret store are open.
Pki::load(
    db: &Database,
    secrets: &SecretStore,
    config: &PkiConfig,
    data_dir: &Path,
    names: &[String],          // web::tls::server_names(config, stored)
    addresses: &[IpAddr],      // config.pki.server_ips
) -> Result<Arc<Pki>, Error>
// loads/creates the root CA, loads/issues the internal server certificate, installs it in a
// HotSwapCertResolver, and reloads the revocation cache from `certificates`.

// Enrolment — one call; do not assemble it from the pieces.
pub struct Enrollment<'a> {
    pub username: &'a Username,        // the authenticated user; becomes CN
    pub csr_body: &'a [u8],            // the request body, exactly as the client sent it
    pub client_uid: Option<&'a str>,   // the `clientUid` parameter, recorded, never validated
    pub user_id: Option<UserId>,
    pub device_id: Option<DeviceId>,
    pub credential_id: Option<CredentialId>,   // revoking it revokes this certificate
    pub issued_via: IssuedVia,         // EnrollV2Json | EnrollV2Xml | EnrollV1P12 | AdminPackage
    pub channels_capable: bool,        // the `version` query parameter was present
}
pki.enroll(db, enrollment) -> Result<IssuedCert, Error>
// parse → policy → sign → `certificates` row → revocation cache → audit, in that order.
// Kind::User on a malformed request, a CN that is not the authenticated user, or a refused key.

pub struct IssuedCert {
    pub der: CertificateDer<'static>,
    pub serial_hex: String,
    pub fingerprint: String,      // lowercase sha256 hex — rustak's identifier everywhere
    pub subject: String,          // "CN=alice,O=rustak,OU=EUD"
    pub common_name: String,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
}

// Revocation
pki.revoke(db, fingerprint: &str, RevokeReason, actor: Option<&Username>) -> Result<bool, Error>
// false = it was already revoked, which is not an error. Kind::User if no such fingerprint.
pki.revocations() -> &Arc<RevocationCache>
//   .on_revoked(Arc<dyn Fn(&str) + Send + Sync>)  ← the stream module registers its disconnect here
//   .reload(db).await, .note_issued(fp), .note_revoked(fp), .is_acceptable(fp), .counts()

// Listeners
pki.marti_server_config(required: bool) -> Result<rustls::ServerConfig, Error>
pki.stream_server_config()              -> Result<rustls::ServerConfig, Error>   // always mandatory
pki.client_verifier(mandatory: bool)    -> Result<Arc<RustakClientVerifier>, Error>
pki.resolver()                          -> &Arc<HotSwapCertResolver>   // ACME/rotation install here

// Everything the routes need to render a response
pki.ca()            -> &CaMaterial
pki.chain()         -> Vec<CertificateDer<'static>>   // the `ca0`/`<ca>` elements
pki.name_entries()  -> Vec<(&str, &str)>              // the `tls/config` XML's nameEntry list
pki.csr_policy()    -> CsrPolicy
pki.p12_options(friendly_name: &str) -> P12Options<'_>
pki.config()        -> &PkiConfig
```

Response bodies come from `pki::pem`: `bare_base64_64col(&issued.der)` for `signedCert` and each
`caN` (64-column wrapping, trailing newline, no armour — both clients add or strip the armour
themselves), `pem_certificate` where a full PEM is wanted.

Bundles come from `pki::p12`: `client_keystore(key, cert, chain, &options)`,
`truststore(chain, &options)`, `legacy_signclient_v1(cert, chain, &options)` — the last is the body
of the pre-v2 `signClient` endpoint, aliases `signedCert`/`ca0`/`ca1`…

Actix wiring, when the Marti listener is built:

```rust
HttpServer::new(app)
    .on_connect(rustak_server::pki::tls::on_connect_capture)
    .bind_rustls_0_23(addr, pki.marti_server_config(required)?)
// handlers: req.conn_data::<PeerCertificate>()  →  { der, fingerprint, common_name, serial_hex }
// the stream listener uses pki::tls::from_tokio_rustls(&stream) on the accepted TlsStream.
```

## Wire-contract decisions, against `compat/enrollment.md`

- **CSR encodings.** `parse_csr` accepts PEM (`-----BEGIN CERTIFICATE REQUEST-----`, and the
  `BEGIN NEW CERTIFICATE REQUEST` banner Windows tooling writes), bare base64 with or without
  newlines, and raw DER. Base64 is read in either alphabet with padding optional, because a body
  that has been through a URL parameter arrives with `-`/`_`. Bare base64 is tried before raw DER
  and the result must start with a DER `SEQUENCE` tag, so the two cannot be confused: a request long
  enough to matter cannot be all-base64-alphabet bytes by accident.
- **The signature is verified** (`x509-parser` `verify_signature`). A request whose signature does
  not check out is one whose sender may not hold the key, and signing it would hand somebody a
  certificate for a key they do not own. A single flipped bit in the signature is refused (tested).
- **CN must equal the authenticated username, case-insensitively** (`Username::eq_ignore_case`).
  `Alice` enrolling as `alice` is fine; `bob` is `Kind::User` and nothing is issued or recorded.
- **`O`/`OU` mismatch is a `debug!`, not a refusal.** TAK Server's `validateCSR` insists the RDN set
  matches its `nameEntries` exactly. We overwrite the subject anyway, so that check can only refuse
  an enrolment that would otherwise have worked — `warn_on_subject_mismatch` says so once and carries
  on. (This is design 03 §0's "downgraded to a warn"; `compat/enrollment.md` §2's stricter wording is
  TAK's behaviour, not a client requirement.)
- **The issued certificate is entirely ours.** `issue.rs` replaces `distinguished_name`, clears
  `subject_alt_names`, `custom_extensions`, `crl_distribution_points` and `name_constraints`, and
  sets `is_ca = ExplicitNoCa`. The only thing taken from the request is its public key — asserted by
  a test comparing the issued `SubjectPublicKeyInfo` bytes against the request's. A CSR asking for
  `evil.example.com` and `O=somebody else` produces a certificate with neither.
  **This deliberately contradicts `compat/enrollment.md` §4's "Subject: CSR subject verbatim"**,
  per the brief and `plan.md`'s TLS row; the clients do not inspect the subject they get back.
- **Key usage follows the key type**: `digitalSignature` + `nonRepudiation` always, plus
  `keyEncipherment` for RSA (the TLS 1.2 RSA key exchange needs it) or `keyAgreement` for a curve.
  Asserting `keyEncipherment` on an EC key is a contradiction some verifiers refuse, which is why
  the flat `digitalSignature, keyAgreement, nonRepudiation` list of `compat/enrollment.md` §4 is
  split by type instead.
- **EKU** is `clientAuth`, plus `1.2.840.113549.1.9.7` when the enrolment carried a `version`
  parameter *and* `[pki] channels_marker_eku` is on.
- **Serial** is 128 random bits with the top bit cleared, so the DER integer stays positive without a
  pad byte. SKI and AKI are both present and the AKI is byte-equal to the CA's SKI (tested).
- **Validity** is `[pki] client_cert_validity` from now, `notBefore` backdated 5 minutes for clock
  skew, and `notAfter` clamped to one day inside the CA's own expiry. An expired CA is a
  `Kind::User` refusal naming what the administrator has to do, not a certificate nobody can verify.

## Security decisions

- **Revocation is enforced at the handshake.** `RustakClientVerifier` runs `WebPkiClientVerifier`
  first (chain, validity, `clientAuth` EKU) and only then hashes the certificate and asks the cache.
  That order matters: hashing something that has not been proven to chain to us would let anyone
  probe which fingerprints we have heard of. `Revoked` maps to `CertificateError::Revoked` and
  `Unknown` to `CertificateError::UnknownIssuer` — deliberately not both `Revoked`, because a
  certificate we have no record of was never taken back and saying so sends an operator looking for
  the wrong thing.
- **`require_known_cert` defaults on**, so a certificate that chains perfectly but has no row — a
  restored backup, a deleted row — is refused. An empty cache therefore refuses everything, which is
  the right failure: `Pki::load` reloads before any listener is bound.
- **The cache fails closed.** It can only ever refuse something the database would have accepted
  (nothing is un-revoked), so a cache that has fallen behind is safe.
- **`revoke()` marks the row, updates the cache and fires the hooks** — in that order, and
  `note_revoked` runs even when the row was already marked, so a certificate revoked by a second
  process is still refused here and its live connections still dropped. Hooks are cloned out of the
  lock before being called, so a hook that revokes something else does not deadlock (tested).
- **`Pki::enroll` writes the row before priming the cache.** A fingerprint the cache accepts but the
  database has no record of is one nothing can revoke.
- **Debug impls redact.** `CaMaterial`, `IssuedCert`, `PeerCertificate`, `TlsAlpnChallenge`,
  `P12Options`, `RevocationCache`, `TestClient` and `TestAuthority` all have hand-written `Debug`
  impls; tests assert the passphrase, the DER and the private key never appear in one.
- **PKCS#12 defaults to the legacy algorithms** (`PbeWithShaAnd3KeyTripleDesCbc`, `HmacSha1`, 2048
  iterations) because that is what ATAK's BouncyCastle keystore reads; a test finds the 3DES and
  SHA-1 OIDs in the output bytes and asserts PBES2 is absent, and the inverse for `p12_legacy =
  false`. The `local_key_id` is the first 20 bytes of `sha256(certificate)` rather than `sha1(…)`
  from design 03 — the value only has to match between the key bag and its certificate bag, and this
  avoids adding a `sha1` dependency for a non-cryptographic identifier.

## TLS listeners

`base()` is `ServerConfig::builder_with_protocol_versions(&[TLS13, TLS12])` over aws-lc-rs, for all
three listeners. A test drives a real handshake with a client pinned to each version and asserts the
negotiated version is the one it pinned — rustls exposes no accessor for a built configuration's
version list, and the behaviour is what matters anyway.

| Builder | Client certificate | ALPN |
|---|---|---|
| `public_server_config(resolver)` | never asked for | `h2`, `http/1.1`, `acme-tls/1` |
| `marti_server_config(resolver, verifier)` | offered; `mandatory` is the verifier's | `h2`, `http/1.1` |
| `stream_server_config(resolver, verifier)` | required | **none** |

The stream advertises no ALPN because commoncommo offers none, and rustls refuses a handshake whose
client offers nothing when the server has a list. `stream_server_config` `warn!`s if it is handed a
non-mandatory verifier rather than silently accepting anonymous stream connections.

`HotSwapCertResolver` answers an ordinary handshake from a `RwLock<Option<Arc<CertifiedKey>>>` —
**without requiring SNI**, because ATAK's streaming client sends none — and an `acme-tls/1`
handshake from a separate challenge slot, only when the SNI matches the name being validated. An
`acme-tls/1` handshake with no matching challenge returns `None` rather than falling back to the
real certificate, which would produce a confusing validation failure. ACME (a later brief) installs
renewed certificates through `install()` and drives the slot with `set_challenge`/`clear_challenge`.

### The handshake tests the brief asked for

`pki::tls::tests` runs a real rustls client against a real `ServerConfig` over a loopback socket:

| Test | Listener | Client | Outcome |
|---|---|---|---|
| `an_enrolled_client_completes_the_stream_handshake` | stream | valid, known | connects |
| `a_revoked_client_is_dropped_at_the_stream_handshake` | stream | revoked | alert |
| `a_client_we_have_no_record_of_is_dropped_…` | stream | valid, unknown | alert |
| `a_client_from_another_authority_is_dropped_…` | stream | foreign CA (and noted as known, so only the chain check can refuse it) | alert |
| `a_client_with_no_certificate_is_dropped_…` | stream | none | alert |
| `the_marti_listener_lets_a_device_without_a_certificate_enrol` | marti | none | connects |
| `the_marti_listener_still_refuses_a_revoked_certificate` | marti | revoked | alert |
| `the_public_listener_never_asks_for_a_certificate` | public | none | connects |

The certificates come from `pki::testing::TestAuthority`, which is built from
`load_or_create_root_ca` → `parse_csr` → `issue_client_cert` rather than from a hand-rolled rcgen
certificate, so the test proves that what rustak *issues* is what rustls *accepts*.

The server side of every one of these runs on its own `std::thread::spawn`ed OS thread — not a
tokio task — because the whole exchange is blocking socket I/O and the tests that drive it are a
mix of `#[test]` and `#[tokio::test]`. Both ends set a **10-second socket read and write timeout**,
so a handshake the server refuses can never leave either side waiting for a flight that will not be
sent: a regression fails with the error rustls reported rather than hanging.

This was not hypothetical. The first cut of these tests used `read_to_string` with no timeouts and
hung on every case where the server rejects the client certificate; the orchestrator saw that
intermediate state and reported it. Fixed in three ways: the timeouts above, `read_exact` of a
fixed-length greeting instead of reading to end-of-stream (what is under test is whether the
handshake completed, and waiting for the close turned a missing `close_notify` into a failure of its
own), and the server sending `close_notify` before it drops the socket. `pki::tls` now runs in 10.2
seconds wall clock.

## Deviations from design 03 §2/§3, and why

1. **`Pki` lives in `pki/facade.rs`, not `pki/mod.rs`.** The design puts the facade in `mod.rs`; it
   is 219 functional lines, and `mod.rs` is better as the module's table of contents. `pki::Pki` is
   re-exported, so the path the design names still resolves.
2. **`human_errors::Error` throughout, no `PkiError`/`CsrError`.** Following M0-10's deviation 2 and
   `conventions.md`; nothing else in the tree has a typed error to be consistent with.
   `CertRejection` *is* a real enum, because the TLS verifier maps its two variants to different
   rustls errors.
3. **Fingerprints are `String`, not a `Sha256Fingerprint` newtype.** Following M0-10's deviation 4:
   `db/repos/certificates.rs` stores a `String` and `pem::sha256_fingerprint` returns one.
   Introducing the newtype is still a worthwhile single change across both, later.
4. **`RevocationHook` is `Arc<dyn Fn(&str) + Send + Sync>`, held in a `RwLock<Vec<_>>`.** The design
   offered "a `Vec<Box<dyn Fn>>` or a channel"; hooks have to be registrable *after* construction
   (the stream module does not exist when `Pki::load` runs), which needs interior mutability, and
   `Arc` lets them be cloned out of the lock before being called.
5. **`revoke()` takes `&Database` and `&RevocationCache`, not `&S: Services` and `&Hub`.** There is
   no `Hub` yet; the hook is the seam, and `Pki::revoke` is the convenience wrapper.
6. **`revoke_for_credential` was added** beyond the design. `credentials.rs` already had
   `revoke_for_credential` on the repository and design 03 §1.8 wants revoking a credential to
   revoke what it bought; without this the cache would not learn about those rows until a reload.
7. **No `time` dependency.** rcgen dates a certificate with `time::OffsetDateTime`, which we need at
   minute granularity for the 5-minute backdating (`ca.rs` and `server_cert.rs` use day granularity
   via `date_time_ymd` and do not). `x509_parser::time::ASN1Time::from_timestamp(..).to_datetime()`
   hands one over and resolves to the same `time` 0.3, so no second date library was added.
8. **`pki/testing.rs` rather than `testing/pki.rs`.** `src/testing/` is another brief's file and the
   brief scoped this one to `pki/**`. It is gated `#[cfg(any(test, feature = "testing"))]` exactly as
   `src/testing/` is, so `--features testing` exposes `rustak_server::pki::testing::TestAuthority`
   to the integration tests under `tests/`.
9. **`RustakClientVerifier::new` installs the crypto provider itself.** `WebPkiClientVerifier`
   reaches for the process-wide provider and *panics* without one; start-up installs it, and this
   makes a test that builds a verifier on its own behave the same way rather than panicking with a
   message about crate features.

## Notes for the briefs that follow

- **M2-02 (`marti/tls.rs`, enrollment endpoints)**: use `Pki::enroll` — do not call `parse_csr` /
  `issue_client_cert` separately, or the row and the cache can disagree. `IssuedVia` decides both
  the `issued_via` and the `source` column. `pki.name_entries()` is what the `tls/config` XML must
  render as `<nameEntry>` elements (remember: **at least two**, or CloudTAK's `xml-js` compaction
  breaks). Response bodies are `pem::bare_base64_64col`. `200` on success — never `201`.
- **M2 (`web/mod.rs`, listeners)**: `pki::tls::on_connect_capture` goes on the actix `HttpServer`
  builder; `pki.marti_server_config(config.web.marti.client_cert == Required)?` is the TLS
  configuration. `web::tls::resolve` is untouched and still builds the public listener's
  `ServerConfig` directly — when ACME lands it should switch to `public_server_config` over its own
  `HotSwapCertResolver`, which is why that builder exists already.
- **M2 (stream listener)**: `pki.stream_server_config()?`, then
  `pki::tls::from_tokio_rustls(&stream)` on each accepted connection, and
  `pki.revocations().on_revoked(Arc::new(move |fp| hub.disconnect_by_fingerprint(fp)))` at start-up.
- **M2 (ACME)**: `HotSwapCertResolver::{install, set_challenge, clear_challenge}` and
  `ACME_TLS_ALPN` are in place; `TlsAlpnChallenge { sni, certified }` is the shape `tls_alpn01.rs`
  should build. `public_server_config` already advertises `acme-tls/1`.
- **`db/repos/certificates.rs` was not changed.** `create`, `get_by_fingerprint`, `fingerprints`,
  `revoke`, `revoke_for_credential`, `list_for_user`, `list_of_kind`, `expiring_before` and
  `touch_last_seen` covered everything. `touch_last_seen` is *not* called yet — the Marti middleware
  that resolves a `PeerCertificate` to a principal is the right place for it.
- **`p12::openssl_reads_a_legacy_bundle` is `#[ignore]`d** and runs
  `openssl pkcs12 -legacy -info -noout` against a generated bundle. It needs OpenSSL 3 with the
  legacy provider; worth wiring into the nightly interop job rather than the PR gate.

## Exit checks

```
$ cargo test -p rustak-server pki::
     Running unittests src/lib.rs (target/debug/deps/rustak_server-…)
running 146 tests
test result: ok. 146 passed; 0 failed; 1 ignored; 0 measured; 643 filtered out; finished in 10.75s
     Running unittests src/main.rs
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running tests/bootstrap.rs
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Per module: `ca` 12, `csr` 18, `facade` 8, `issue` 13, `keys` 6, `p12` 12 (1 ignored), `pem` 9,
`revoke` 17, `server_cert` 7, `testing` 3, `tls` 32 — of which `tls::tests` 12 (the real handshakes),
`tls::client_verifier` 8, `tls::resolver` 7, `tls::peer` 5.

```
$ cargo clippy -p rustak-server --all-targets --all-features -- -D warnings
    Checking rustak-client v0.1.0 (…/rustak-client)
    Checking rustak-server v0.1.0 (…/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 17.07s

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-server --no-deps
 Documenting rustak-server v0.1.0 (…/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.56s
   Generated …/target/doc/rustak_server/index.html and 1 other file

$ cargo fmt -p rustak-server --check
(no output; exit 0)

$ ./scripts/check-file-length.sh
(no output; exit 0)
```

`check-file-length.sh` enumerates with `git ls-files`, so it does not yet see these files — they are
untracked and this brief makes no `git`/`but` writes. Every new file was measured by hand with the
script's own awk rule; the table at the top of this document is that measurement. The largest is
`pki/csr.rs` at 269, against a limit of 300.

Whole-crate regression check, and the `testing` feature:

```
$ cargo test -p rustak-server
test result: ok. 788 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 15.86s
   Doc-tests rustak_server
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test -p rustak-server --all-features pki::
test result: ok. 146 passed; 0 failed; 1 ignored; 0 measured; 643 filtered out; finished in 10.79s

$ cargo test -p rustak-server --features testing
     Running unittests src/lib.rs
test result: ok. 788 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 14.78s
     Running tests/bootstrap.rs
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.58s
   Doc-tests rustak_server
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
(18 s wall clock for the whole invocation — nothing hangs)

$ cargo test -p rustak-server --lib pki::tls
test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; 758 filtered out; finished in 10.23s
```

788 lib tests = the 642 the rest of the crate had before this brief, plus these 146.

## Concurrency note

Two other agents were editing `rustak-cot/proto/**`, `rustak-client/src/stream/**`,
`rustak-server/src/identity/**` and `rustak-server/src/web/api/**` throughout. Several intermediate
builds failed on *their* half-landed files (a renamed protobuf package, a `mint` signature mid-change,
a `TakStream::queued` that had not landed yet); each resolved on its own and nothing of theirs was
edited. All the numbers above were taken after the tree settled.

The orchestrator relayed three findings from an agent running the suite mid-brief: hanging
`pki::tls::*` handshakes, `clippy::cloned_ref_to_slice_refs` in `pki/tls/resolver.rs`, and the
ambiguous ``[`revoke`]`` intra-doc link in `pki/mod.rs`. All three were of this brief's own making
and all three are fixed above — the hangs by the timeouts and `read_exact` described under the
handshake table, the lint by `std::slice::from_ref`, and the link by `[`mod@revoke`]` (the module
and the function share a name). Every exit check in this document was re-run against the final tree
afterwards.
