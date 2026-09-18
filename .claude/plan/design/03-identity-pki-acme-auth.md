I have everything I need; here is the plan.

# rustak — Identity, PKI, TLS/ACME and Auth: implementation plan

## 0. Facts verified from source that drive the design

| Fact | Source | Consequence |
|---|---|---|
| ATAK stream TLS: `SSL_CTX_new(SSLv23_client_method())` (= `TLS_client_method`, max TLS 1.3) and `SSL_CTX_set_cipher_list(sslCtx, "DEFAULT:!ECDH")` ("demo.atakserver.com fails when they are [enabled]") | `refs/atak-civ/commoncommo/core/impl/streamingsocketmanagement.cpp:80,418-421` | `!ECDH` removes every ECDHE TLS 1.2 suite; rustls only offers ECDHE suites in TLS 1.2 (no RSA-kx, no DHE). **The 8089 handshake with ATAK is TLS 1.3 or nothing.** TLS 1.3 suites are unaffected by `set_cipher_list`. Server-cert key type does not rescue TLS 1.2. |
| commoncommo generates RSA-2048, signs CSR with SHA-256, subject = `CN=<user>` first then every `nameEntry` in order | `crypto.cpp:279-306,336-419`, `enroll.cpp:648-660` | We accept RSA≥2048 (and ECDSA), CN must equal the authenticated user. |
| PKCS#12 written/read with `NID_pbe_WithSHA1And3_Key_TripleDES_CBC` (key), `NID_pbe_WithSHA1And40BitRC2_CBC` (cert), SHA-1 MAC, legacy provider loaded "until bouncycastle used by atak and takkernel support newer openssl defaults" | `crypto.cpp:260,495-525` | Manual-package p12 must be legacy PBES1 + HmacSha1. `p12-keystore` 0.3.2 supports exactly this (`EncryptionAlgorithm::PbeWithShaAnd3KeyTripleDesCbc`, `MacAlgorithm::HmacSha1`). |
| TAK `validateCSR`: RDN count == nameEntries+1, `getHttpUser().compareToIgnoreCase(cn)==0`, each nameEntry present case-insensitively | `CertManagerService.java:62-107` | Same check, but we only *require* CN match; O/OU mismatch is a warning (we overwrite the subject anyway). |
| `signClient/v2` Accept dispatch: null/`*/*`/`application/json`/empty → JSON `{"signedCert","ca0","ca1"…}`; contains `application/xml` → `<?xml…?><enrollment><signedCert>…</signedCert><ca>…</ca></enrollment>`; else 400. Bodies are `certToPEM(cert,false)` = base64 with `\n` every 64 chars, no armour. CSR body: armour stripped then base64-decoded (so PEM or bare) | `CertManagerApi.java:344-407`, `Util.java:18-34`, `CertManagerService.java:128-133` | Reproduce exactly, including 64-col wrapping (node-tak wraps into PEM; commoncommo prepends/appends armour). |
| Legacy `POST /tls/signClient` returns PKCS#12 (`application/octet-stream`), password `atakatak`, aliases `signedCert`, `ca0…`, certs only | `CertManagerApi.java:301-337` | Implement for WinTAK/old clients via `p12.rs`. |
| ATAK quick-connect fails with "Server did not return trust configuration" if no `<ca>` returned; commoncommo builds the truststore from all `<ca>` elements and the client keystore from `signedCert`+CAs | `CertificateEnrollmentClient.java:357-361`, `enroll.cpp:770-800` | Always return the full chain (≥1 CA). |
| ATAK enrollment trust: pre-provisioned truststore if present, else "local trust manager with public CAs included … will use hostname verification" | `CertificateEnrollmentClient.java:510-533` | 8446 needs a publicly-trusted cert **or** a pre-provisioned truststore package (`enrollForCertificateWithTrust`). Both flows designed below. |
| Enrollment port default 8446 (`apiCertEnrollmentPort`), API 8443 | `CotMapComponent.java:1301-1313`, `SslNetCotPort.java` | Bind public app on 443 **and** 8446. |
| node-tak: `Buffer.from(jwt,'base64').toString().split('}')`, `JSON.parse(split[1]+'}')`, expects `{sub,aud,nbf,exp,iat}` (`aud` a string); 401/403 or `invalid_grant`+"Bad credentials" = bad creds | `refs/node-tak/lib/api/oauth.ts:25-66` | Header exactly 27 bytes; payload flat; `aud` string; return 401 with an `invalid_grant` body (satisfies both branches). |
| node-tak enrollment: `Accept: application/json`, Basic auth, PEM CSR, `clientUid=<user> (ETL)&version=3`, reads `ns2:certificateConfig.nameEntries.nameEntry[]` | `credentials.ts:42-108` | Root element must literally be `ns2:certificateConfig`; ≥2 `nameEntry` so xml2js yields an array. |
| TAK `/login/auth` sets cookie `state`=random, sends `state=sha256(cookie)`; `/login/redirect` checks `sha256(cookie)==state`, exchanges with `client_secret`, sets HttpOnly `access_token` cookie(s), redirects to `/Marti/login/redirect.html`; `/login/refresh`; `/login/authserver` → `ApiResponse<String>`; `/login/.well-known/openid-configuration` → `{authorization_endpoint,token_endpoint}` of the IdP; `/token/access` → `ApiResponse<String>` gated by `allowAccessTokenRetrieval` | `OAuthApi.java:77-114,207-380` | Mirror shapes; add PKCE + nonce + server-side state on top. |
| Group suffix semantics: `readOnlyGroup` member → no IN for anything; name ends with `readSuffix` → OUT only (suffix stripped); ends with `writeSuffix` → IN only; else IN+OUT | `LdapAuthenticator.java:746-789` | Implement identically in `identity/provisioning.rs`. |
| X509 auth: sha256 fingerprint → `TakCert` → revoked ⇒ reject; username from cert vs authenticated id `compareToIgnoreCase`; TAK adds EKU OID `1.2.840.113549.1.9.7` to client certs when `version` param present (marker for group cache) | `X509Authenticator.java:130-171,201` | Fingerprint-keyed revocation cache; case-insensitive username; optionally add the marker EKU. |
| Crate APIs (local registry): rustls 0.23 `ClientCertVerifier` (sync, `verify_client_cert(ee, intermediates, now)`), `ResolvesServerCert::resolve(ClientHello)` with `alpn()`/`server_name()`, `WebPkiClientVerifier::builder(roots).allow_unauthenticated().build()`; actix-web `on_connect(Fn(&dyn Any,&mut Extensions))`, `bind_rustls_0_23`; actix-tls `TlsStream` derefs to `tokio_rustls::server::TlsStream` → `.get_ref().1.peer_certificates()`; rcgen 0.14 `CertificateSigningRequestParams{params,public_key}::from_der/from_pem` + `.signed_by(&Issuer)`, `Issuer::from_ca_cert_der`, `KeyPair::from_pkcs8_der_and_sign_algo`; instant-acme 0.8 `Account::builder()?.create(..)->(Account, AccountCredentials: Serialize)`, `ChallengeType::{Http01,TlsAlpn01}`, `KeyAuthorization::{as_str,digest}`, `finalize_csr`, `poll_certificate`; jsonwebtoken 11 `crypto::sign(msg,&EncodingKey,Algorithm)`; argon2 0.6 with `password-hash`; x509-parser 0.18 `X509CertificationRequest::from_der` + `verify_signature()` | `~/.cargo/registry/src/*/…` | Signatures below are written against these. |

## 1. Decisions and recommendations

1. **Root CA and internal server certs: RSA-2048/SHA-256 by default**, `ecdsa-p256` selectable. Rationale: not TLS 1.2 (rustls cannot serve `!ECDH` clients on 1.2 regardless), but ecosystem conservatism — every TAK artefact (TAK Server CA, `makeRootCa.sh`, BouncyCastle truststores, legacy PBE p12 in WinTAK/iTAK importers) is RSA; ECDSA gains nothing for a personal deployment and adds an untested variable in ATAK's Java truststore path. Client certs are whatever the CSR carries (RSA-2048 from ATAK/CloudTAK).
2. **TLS 1.3 is mandatory for the stream listener** (offer 1.3+1.2; 1.2 only helps CloudTAK/WinTAK/iTAK). Document; CI test with `openssl s_client -cipher 'DEFAULT:!ECDH'`.
3. **ACME: TLS-ALPN-01 primary on :443, HTTP-01 fallback on :80.** Most home routers already forward 443; TLS-ALPN-01 needs no extra port and rustls supports it through a `ResolvesServerCert` that answers `acme-tls/1` (rustls-acme technique). HTTP-01 needs a plain listener; we bind :80 only when `challenge = "http-01"` or `http_redirect_bind` is set (it also redirects to https).
4. **Public cert source enum: `acme | files | internal`.** `internal` (our CA) is the LAN-only story: ATAK then enrolls via the "trust-bootstrap package" (truststore + `enrollForCertificateWithTrust`), CloudTAK via `webtak` pointed at a host whose CA is in node's `NODE_EXTRA_CA_CERTS` (document).
5. **One bearer format everywhere: our RS256 JWT.** The Yew UI still uses automate's popup code flow, but `/api/v1/auth/token` exchanges the IdP code server-side, validates the ID token, JIT-provisions the user, evaluates `user_acl`/`admin_acl` against the claims **at sign-in/refresh time**, and returns *our* JWT + our rotating refresh token. Per-request checks read the user row (`disabled`, `is_admin`). Access TTL 1h bounds ACL propagation. This removes a second token type from Marti middleware and makes the local-admin bootstrap use the same path (password grant).
6. **OIDC client code: lift automate's discovery/JWKS/validate (split into <300-line files) and add PKCE (S256) + nonce.** `openidconnect` 4 is an alternative, but automate's code is already proven against `TestIdentityProvider` and the additions are ~40 lines; fewer deps, one reqwest.
7. **Active channel state is per device** (`device_group_state`), matching `PUT /groups/active?clientUid=`; default (no rows) = all memberships active; effective bit-vector = memberships ∩ active.
8. **Revocation is enforced at the TLS layer** via an in-memory `RevocationCache` (rustls verifier is sync); revoke also disconnects live stream subscriptions and Marti requests re-check per request. `require_known_cert = true` rejects CA-signed certs missing from the DB (restored backups).
9. **Enrollment tokens are one *enrollment*, not one request:** ATAK performs `tls/config`, `signClient`, `profile/enrollment` with the same Basic credential; `uses` is incremented only on `signClient` success, `max_uses=1`, default TTL 15 min.
10. **Argon2id for every stored secret** (user decision) plus a 5-minute verified-secret cache (`sha256(credential_id || secret)`) so ATAK's 3 Basic calls and CloudTAK's polling do not pay argon2 each time.
11. **First-run bootstrap guarded by a setup token** printed to the log/written to `setup_token_file` (Jenkins-style), so a freshly exposed host cannot have its admin claimed by a stranger.
12. **Wizard-managed settings live in a DB `settings` table; TOML wins when present** (`effective = toml.or(db).or(default)`), limited to: `server.public_hostname`, `pki.name_entries`, `acme.*`, `auth.oidc.*`, ACLs. Containers cannot rewrite their config file.

## 2. File-level layout (every file < 300 functional lines)

```
rustak-server/src/
  pki/
    mod.rs                 Pki facade (Arc), PkiError, re-exports
    keys.rs                KeyType, generate_key (rsa crate → PKCS#8 → rcgen KeyPair), pkcs8 helpers
    ca.rs                  CaMaterial, load_or_create_root_ca (kv + SecretStore), root params, export ca.crt
    csr.rs                 parse_csr (PEM | bare base64 | DER), ParsedCsr, CsrPolicy::validate
    issue.rs               issue_client_cert (rcgen signed_by), ClientSubject, IssuedCert
    server_cert.rs         ServerCertManager (SANs, persistence, rotate_if_needed)
    revoke.rs              RevocationCache (+known set), revoke() orchestration
    p12.rs                 client_keystore, truststore, legacy_signclient_v1
    pem.rs                 bare_base64_64col, pem_block, parse_pem_chain, sha256 fingerprint
    tls/
      mod.rs               public/marti/stream ServerConfig builders, protocol versions, ALPN lists
      resolver.rs          HotSwapCertResolver (ResolvesServerCert + acme-tls/1 challenge)
      client_verifier.rs   RustakClientVerifier (wraps WebPkiClientVerifier + RevocationCache)
      peer.rs              PeerCertificate, on_connect_capture (actix), from_tokio_rustls (stream)
    acme/
      mod.rs               AcmeManager, CertState, startup ensure_certificate
      account.rs           account create/load (sealed AccountCredentials), directory URLs
      order.rs             order → challenges → finalize_csr → poll_certificate → persist/install
      tls_alpn01.rs        challenge cert (rcgen CustomExtension::new_acme_identifier), install/clear
      http01.rs            Http01Responder + actix route /.well-known/acme-challenge/{token}
      byo.rs               ByoCertSource: load files, validate key↔cert, mtime reload
  identity/
    mod.rs                 Identity facade, IdentityError
    username.rs            Username newtype + normalisation
    users.rs               User, UserKind, UserSource, UserRepo
    groups.rs              Group, Direction, GroupKind, bitpos allocation, __ANON__
    members.rs             GroupMembership, MembershipSource, DeviceGroupState, effective groups
    devices.rs             Device, DeviceRepo (uid ↔ user, takv, last seen)
    credentials.rs         Credential, CredentialKind, generate/hash/verify, use accounting
    provisioning.rs        provision_from_identity, map_group_claims (TAK suffix semantics)
    services.rs            (M6) service identities = kind=service users + ServiceToken credentials
  auth/
    mod.rs
    principal.rs           Principal, AuthMethod, GroupSet/bitvecs
    resolve.rs             resolve_principal, marti_auth middleware, ListenerAuthPolicy, extractors
    basic.rs               Basic header parsing + credential verification
    bearer.rs              Bearer extraction + our-JWT verification + jti revocation
    ratelimit.rs           in-memory limiter keyed (ip, username), lockout
    cache.rs               VerifiedSecretCache (TTL)
    acl.rs                 filt-rs Filterable (AuthRequestFilter), json_to_filter_value (lifted)
    mission_token.rs       HS256 mission tokens
    stream_auth.rs         <auth> credentials + cert→Principal for stream/
    audit.rs               audit event names/builders for auth
    oauth_server/
      mod.rs               actix scope: /oauth/*, /login/*, /token/*, /logout
      jwt.rs               JwtIssuer, AccessClaims, fixed header, encode_flat, verify
      keys.rs              signing key generate/load (sealed), rotation, token_key/jwks rendering
      token.rs             POST /oauth/token (password, refresh_token, authorization_code)
      refresh.rs           refresh token table (hashed, rotating), revocation of jti
      authorize.rs         GET /oauth/authorize, GET /login/auth (begin IdP code flow)
      login.rs             /login/redirect, /login/refresh, /login/authserver, /login/.well-known/…, /token/access, /logout
      state.rs             PendingAuth store (kv partition, one-shot claim) lifted from automate integrations/state.rs
    oidc/
      discovery.rs         discovery + cache (lifted)
      jwks.rs              JWKS fetch/cache + kid refetch (lifted)
      validate.rs          verify_token: asymmetric-only, aud/iss/exp/nbf required, nonce (lifted+nonce)
      exchange.rs          exchange_code (+code_verifier), refresh_tokens (lifted)
      pkce.rs              PKCE verifier/challenge, random state/nonce
      claims.rs            username_from_claims (default preferred_username→sub), display, email, groups
  web/api/auth.rs          /api/v1/auth/{metadata,token,refresh,logout,me}, /api/v1/setup/*
  web/api/{users,groups,devices,credentials,certificates,pki,acme}.rs   admin API handlers
  marti/tls.rs             /Marti/api/tls/config, signClient, signClient/v2 (+ profile/enrollment stub until M3)
  db/repos/{users,groups,members,devices,credentials,certificates,refresh_tokens,acme}.rs
  db/migrations/000N_identity.sql, 000N_pki.sql, 000N_auth.sql
  jobs/{acme_renew,cert_reload,server_cert_rotate,credential_expiry,pending_auth_prune}.rs
  testing/{oidc.rs (TestIdentityProvider + code-flow mocks), pki.rs (TestCa, csr helpers), client.rs (reqwest mTLS)}
rustak-api/src/{identity.rs, pki.rs, auth.rs}     serde DTOs shared with the UI
rustak-ui/src/auth.rs                             popup flow + PKCE (lifted from automate)
rustak-ui/src/pages/{setup.rs, devices.rs, credentials.rs (+QR), users.rs, groups.rs, certificates.rs, pki.rs}
```

## 3. `pki/` — types and signatures

### keys.rs / ca.rs
```rust
pub enum KeyType { Rsa2048, Rsa3072, EcdsaP256 }
pub fn generate_key(kind: KeyType) -> Result<rcgen::KeyPair, PkiError>;
//  Rsa*: rsa::RsaPrivateKey::new(&mut OsRng, bits) → pkcs8 DER → KeyPair::from_pkcs8_der_and_sign_algo(&der, &PKCS_RSA_SHA256)
//  Ecdsa: KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)  (pure-Rust RSA keygen keeps us off aws-lc-rs keygen for cross builds)
pub fn key_pair_from_pkcs8(der: &[u8]) -> Result<rcgen::KeyPair, PkiError>;

pub struct CaMaterial {
    pub cert: CertificateDer<'static>,
    pub subject: rcgen::DistinguishedName,
    pub not_after: time::OffsetDateTime,
    pub key_type: KeyType,
    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,   // Issuer::from_ca_cert_der(&cert, key)
}
impl CaMaterial { pub fn issuer(&self) -> &rcgen::Issuer<'static, rcgen::KeyPair>; pub fn chain_der(&self) -> Vec<CertificateDer<'static>>; }
pub async fn load_or_create_root_ca<S: Services>(s: &S, cfg: &PkiConfig) -> Result<CaMaterial, PkiError>;
// storage: kv partition "pki": "root_ca.cert" = DER (plain), "root_ca.key" = Sealed(pkcs8) with SecretContext::PkiKey{ name: "root_ca" }
// params: CN="<ca_name>", O/OU from name_entries; is_ca = Ca(Unconstrained); key_usages = [KeyCertSign, CrlSign, DigitalSignature];
//         serial = 16 random bytes (top bit cleared); validity 10y; key_identifier_method = Sha256; also writes <data>/pki/ca.crt for operators
```

### csr.rs
```rust
pub enum CsrEncoding { Pem, BareBase64, Der }
pub enum CsrKey { Rsa { bits: usize }, EcdsaP256, EcdsaP384, Other(String) }
pub struct ParsedCsr {
    pub der: Vec<u8>,
    pub encoding: CsrEncoding,
    pub common_name: Option<String>,
    pub rdns: Vec<(String, String)>,     // type short name (CN/O/OU/…), value — in CSR order
    pub key: CsrKey,
    pub requested_sans: usize,           // we drop them; counted for audit
}
pub fn parse_csr(body: &[u8]) -> Result<ParsedCsr, CsrError>;
// 1) if contains "-----BEGIN CERTIFICATE REQUEST-----": strip armour lines  2) strip all whitespace; if all chars in base64/base64url alphabet → decode (padding indifferent)
// 3) else if body[0]==0x30 → raw DER. Then x509_parser::certification_request::X509CertificationRequest::from_der + verify_signature() (reject unsigned/forged)

pub struct CsrPolicy { pub min_rsa_bits: u32 /*2048*/, pub allow_ecdsa: bool /*true*/, pub max_der_len: usize /*8 KiB*/ }
impl CsrPolicy {
    pub fn validate(&self, csr: &ParsedCsr, authenticated: &Username) -> Result<(), CsrError>;
    // CN required; Username::parse(cn)? == authenticated (case-insensitive); key type/size; TAK's O/OU check downgraded to a warn! log
}
```

### issue.rs
```rust
pub struct IssueRequest<'a> {
    pub username: &'a Username,
    pub client_uid: Option<&'a str>,
    pub validity: chrono::Duration,             // [pki].client_cert_validity, capped at CA not_after - 1 day
    pub not_before_skew: chrono::Duration,      // 5 min
    pub channels_marker: bool,                  // add EKU OID 1.2.840.113549.1.9.7 when `version` param present (TAK marker; harmless)
}
pub struct IssuedCert { pub der: CertificateDer<'static>, pub serial: Vec<u8>, pub fingerprint: Sha256Fingerprint, pub subject: String, pub not_before: DateTime<Utc>, pub not_after: DateTime<Utc> }
pub fn issue_client_cert(ca: &CaMaterial, csr: &ParsedCsr, name_entries: &[(String, String)], req: &IssueRequest<'_>) -> Result<IssuedCert, PkiError>;
// let mut p = CertificateSigningRequestParams::from_der(&csr.der.as_slice().into())?;   // keeps public key
// p.params.distinguished_name = { CN=username } + name_entries (O, OU …) in config order  ← we never trust CSR RDNs/SANs/extensions
// p.params.subject_alt_names.clear(); p.params.custom_extensions.clear(); p.params.is_ca = IsCa::ExplicitNoCa;
// p.params.key_usages = [DigitalSignature] + [KeyEncipherment if RSA]; p.params.extended_key_usages = [ClientAuth] (+ Other(oid) marker)
// p.params.serial_number = Some(random_128bit()); not_before/not_after; use_authority_key_identifier_extension = true; key_identifier_method = Sha256
// p.signed_by(ca.issuer())
```

### server_cert.rs / revoke.rs / p12.rs / pem.rs
```rust
pub struct ServerCertManager { names: Vec<rcgen::SanType>, key_type: KeyType, validity: Duration, renew_before: Duration, current: RwLock<Arc<CertifiedKey>> }
impl ServerCertManager {
    pub async fn load_or_issue<S: Services>(s: &S, ca: &CaMaterial, cfg: &PkiConfig) -> Result<Self>;   // kv "pki": "server_cert.internal" {cert DER chain, Sealed key}
    pub fn certified_key(&self) -> Arc<rustls::sign::CertifiedKey>;
    pub async fn rotate_if_needed<S: Services>(&self, s: &S, ca: &CaMaterial) -> Result<bool>;             // job: daily
}
// SANs = [pki.server_names…, acme.domains…, server.public_hostname] as DnsName + pki.server_ips as IpAddress (+ "localhost"/127.0.0.1 in tests); EKU serverAuth; CN = first name

pub struct RevocationCache { revoked: RwLock<HashSet<Sha256Fingerprint>>, known: RwLock<HashSet<Sha256Fingerprint>>, require_known: bool }
impl RevocationCache {
    pub async fn reload(&self, repo: &CertificateRepo) -> Result<()>;
    pub fn is_acceptable(&self, fp: &Sha256Fingerprint) -> Result<(), CertRejection /*Revoked|Unknown*/>;
    pub fn note_issued(&self, fp: Sha256Fingerprint); pub fn note_revoked(&self, fp: Sha256Fingerprint);
}
pub enum RevokeReason { UserRequest, DeviceLost, Superseded, AdminAction, CredentialRevoked, UserDisabled }
pub async fn revoke<S: Services>(s: &S, pki: &Pki, hub: &Hub, fp: &Sha256Fingerprint, reason: RevokeReason, actor: &Username) -> Result<()>;
// repo.mark_revoked → cache.note_revoked → hub.disconnect_by_fingerprint(fp) → audit "pki.cert.revoked"

pub struct P12Options<'a> { pub password: &'a str /*"atakatak"*/, pub friendly_name: &'a str, pub legacy: bool /*true → 3DES + HmacSha1, 2048 iters; false → AES-256 + HmacSha256*/ }
pub fn client_keystore(key_pkcs8: &[u8], cert: &[u8], chain: &[&[u8]], o: &P12Options) -> Result<Vec<u8>>;   // KeyStoreEntry::PrivateKeyChain(PrivateKeyChain::new(local_key_id=sha1(cert), PrivateKey::from_der(key), [cert, chain…]))
pub fn truststore(chain: &[&[u8]], o: &P12Options) -> Result<Vec<u8>>;                                        // aliases "ca0","ca1"… KeyStoreEntry::Certificate
pub fn legacy_signclient_v1(cert: &[u8], chain: &[&[u8]]) -> Result<Vec<u8>>;                                 // aliases "signedCert","ca0"…; password "atakatak"

pub fn bare_base64_64col(der: &[u8]) -> String;   // STANDARD alphabet, '\n' after every 64 chars, trailing '\n' (matches TAK toPEM)
pub fn pem_certificate(der: &[u8]) -> String;
pub fn parse_pem_chain(pem: &str) -> Result<Vec<CertificateDer<'static>>>;  // rustls-pemfile
pub fn sha256_fingerprint(der: &[u8]) -> Sha256Fingerprint;                  // hex lowercase, no colons (TAK stores hash; format is ours)
```
Persistence (`db/repos/certificates.rs`):
```sql
CREATE TABLE certificates (
  id INTEGER PRIMARY KEY, fingerprint TEXT NOT NULL UNIQUE, serial_hex TEXT NOT NULL UNIQUE,
  subject TEXT NOT NULL, user_id INTEGER NOT NULL REFERENCES users(id), device_id INTEGER REFERENCES devices(id),
  client_uid TEXT, der BLOB NOT NULL, issued_at TEXT NOT NULL, not_before TEXT NOT NULL, not_after TEXT NOT NULL,
  issued_via TEXT NOT NULL CHECK (issued_via IN ('enroll_v2_json','enroll_v2_xml','enroll_v1_p12','admin_package','acme_internal')),
  credential_id INTEGER REFERENCES credentials(id), revoked_at TEXT, revoke_reason TEXT, revoked_by TEXT, last_seen_at TEXT
);
CREATE INDEX certificates_user ON certificates(user_id); CREATE INDEX certificates_revoked ON certificates(revoked_at) WHERE revoked_at IS NOT NULL;
```

### tls/
```rust
pub struct HotSwapCertResolver { current: RwLock<Option<Arc<CertifiedKey>>>, challenge: Mutex<Option<TlsAlpnChallenge>> }
pub struct TlsAlpnChallenge { pub sni: String, pub cert: Arc<CertifiedKey> }
impl HotSwapCertResolver { pub fn new(initial: Option<Arc<CertifiedKey>>) -> Arc<Self>; pub fn install(&self, ck: Arc<CertifiedKey>); pub fn set_challenge(&self, c: TlsAlpnChallenge); pub fn clear_challenge(&self); }
impl ResolvesServerCert for HotSwapCertResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let is_acme = hello.alpn().map_or(false, |mut a| a.any(|p| p == b"acme-tls/1"));
        if is_acme { /* SNI must equal challenge.sni (case-insensitive) → challenge cert, else None */ } else { self.current.read().clone() }   // no SNI required (commoncommo sends none)
    }
}

pub struct RustakClientVerifier { inner: Arc<dyn ClientCertVerifier>, revocations: Arc<RevocationCache>, mandatory: bool }
impl RustakClientVerifier { pub fn new(root: &CertificateDer<'_>, revocations: Arc<RevocationCache>, mandatory: bool) -> Result<Arc<Self>>; }
//  inner = WebPkiClientVerifier::builder(Arc::new(RootCertStore{root})).allow_unauthenticated().build()?   (chain, validity, EKU clientAuth via webpki)
impl ClientCertVerifier for RustakClientVerifier {
    fn offer_client_auth(&self) -> bool { true }
    fn client_auth_mandatory(&self) -> bool { self.mandatory }
    fn root_hint_subjects(&self) -> &[DistinguishedName] { self.inner.root_hint_subjects() }
    fn verify_client_cert(&self, ee: &CertificateDer<'_>, ints: &[CertificateDer<'_>], now: UnixTime) -> Result<ClientCertVerified, rustls::Error> {
        self.inner.verify_client_cert(ee, ints, now)?;
        match self.revocations.is_acceptable(&sha256_fingerprint(ee)) { Ok(()) => Ok(ClientCertVerified::assertion()),
            Err(CertRejection::Revoked) => Err(rustls::Error::InvalidCertificate(CertificateError::Revoked)),
            Err(CertRejection::Unknown) => Err(rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer /* or ApplicationVerificationFailure */)) }
    }
    fn verify_tls12_signature(..) / verify_tls13_signature(..) / supported_verify_schemes(..) → delegate to inner
}

#[derive(Clone)] pub struct PeerCertificate { pub der: CertificateDer<'static>, pub fingerprint: Sha256Fingerprint, pub common_name: Option<String>, pub serial: Vec<u8> }
pub fn on_connect_capture(conn: &dyn Any, ext: &mut Extensions);   // conn.downcast_ref::<actix_tls::accept::rustls_0_23::TlsStream<actix_web::rt::net::TcpStream>>() → (**s).get_ref().1.peer_certificates()?.first() → ext.insert(PeerCertificate)
pub fn from_tokio_rustls<IO>(s: &tokio_rustls::server::TlsStream<IO>) -> Option<PeerCertificate>;
pub fn common_name(der: &CertificateDer<'_>) -> Option<String>;      // x509-parser iter_common_name().next(); equivalent of TAK's CN=(.*?)(?:,|$)

pub enum ListenerKind { Public, Marti, Stream }
pub fn public_server_config(r: Arc<HotSwapCertResolver>) -> ServerConfig;        // no client auth; alpn = [b"h2", b"http/1.1", b"acme-tls/1"]
pub fn marti_server_config(r: Arc<HotSwapCertResolver>, v: Arc<RustakClientVerifier /*mandatory=false*/>) -> ServerConfig;   // alpn h2, http/1.1
pub fn stream_server_config(r: Arc<HotSwapCertResolver>, v: Arc<RustakClientVerifier /*mandatory=true*/>) -> ServerConfig;   // no ALPN
fn base() -> ConfigBuilder<ServerConfig, WantsVerifier>  // ServerConfig::builder_with_protocol_versions(&[&TLS13, &TLS12]) (aws-lc-rs default provider)
```
actix wiring (`web/mod.rs`): `HttpServer::new(app).on_connect(pki::tls::peer::on_connect_capture).bind_rustls_0_23(addr, marti_server_config(...))`; handlers read `req.conn_data::<PeerCertificate>()`. Cargo: `actix-web = { features = ["rustls-0_23"] }`, `actix-tls` implied.

### acme/
```rust
pub enum ChallengePreference { TlsAlpn01, Http01 }
pub struct AcmeManager { cfg: Arc<AcmeConfig>, resolver: Arc<HotSwapCertResolver>, http01: Arc<Http01Responder>, key_type: KeyType, account: tokio::sync::OnceCell<instant_acme::Account> }
pub enum CertState { Missing, Valid { not_after }, Expiring { not_after }, Failed { last_error, attempts } }
impl AcmeManager {
    pub async fn ensure_account<S: Services>(&self, s: &S) -> Result<&Account>;   // acme_accounts row → SecretStore::open_json::<AccountCredentials>(ctx PkiKey{name:"acme_account"}) → Account::builder()?.from_credentials(c); else create(&NewAccount{contact:&[cfg.contact], terms_of_service_agreed: cfg.accept_tos, only_return_existing:false}, directory_url, None) → seal+store
    pub async fn startup<S: Services>(&self, s: &S) -> Result<CertState>;          // load acme_certificates for domains → install; if none → install temporary self-signed (so :443 binds) and spawn order()
    pub async fn order<S: Services>(&self, s: &S) -> Result<CertState>;            // see order.rs
}
// order.rs: account.new_order(&NewOrder::new(&domains.map(Identifier::Dns)))
//   for each authz in order.authorizations(): if Valid skip; pick challenge(pref) or fallback(other); publish:
//     TlsAlpn01 → tls_alpn01::challenge_cert(domain, key_auth.digest()) (rcgen CertificateParams::new([domain]) + CustomExtension::new_acme_identifier(&digest) self-signed) → resolver.set_challenge
//     Http01   → http01.publish(token = challenge.token, key_auth.as_str())
//   challenge.set_ready(); order.poll_ready(&RetryPolicy::default()) → key = generate_key(key_type); csr = CertificateParams::new(domains).serialize_request(&key); order.finalize_csr(csr.der()); chain = order.poll_certificate(&RetryPolicy::default())
//   persist acme_certificates{domains, chain_pem, key Sealed pkcs8, not_before/not_after from leaf, challenge_type}; resolver.install(CertifiedKey); clear challenge/tokens; audit "acme.issued"
// http01.rs: pub struct Http01Responder { tokens: RwLock<HashMap<String,String>> }  + GET /.well-known/acme-challenge/{token} → text/plain key_auth or 404; mounted on :80 (plain HttpServer, everything else 301 → https) and on the public https app
// byo.rs: pub struct ByoCertSource { cert_path, key_path, last: Mutex<(SystemTime, SystemTime)> }  load() validates chain parses, key parses, leaf pubkey == key pubkey (rcgen SubjectPublicKeyInfo compare) → CertifiedKey; poll() every 60 s by jobs/cert_reload.rs
// jobs/acme_renew.rs: daily at startup+random minute; if not_after - now < renew_before → order(); on Err → attempts+1, backoff 1h/4h/24h, audit "acme.renew.failed" (UI banner reads CertState)
```
```sql
CREATE TABLE acme_accounts (id INTEGER PRIMARY KEY, directory_url TEXT NOT NULL UNIQUE, contact TEXT, credentials TEXT NOT NULL /*Sealed JSON*/, created_at TEXT NOT NULL);
CREATE TABLE acme_certificates (id INTEGER PRIMARY KEY, domains TEXT NOT NULL /*json array, sorted*/ UNIQUE, chain_pem TEXT NOT NULL, key TEXT NOT NULL /*Sealed pkcs8*/, not_before TEXT, not_after TEXT, issued_at TEXT, challenge_type TEXT, attempts INTEGER DEFAULT 0, last_attempt_at TEXT, last_error TEXT);
```

## 4. `identity/` — model, repos, rules

```rust
pub struct Username(String);
impl Username {
    pub fn parse(raw: &str) -> Result<Self, UsernameError>;
    // trim; lowercase (ASCII + Unicode simple fold); len 1..=64; allowed chars [a-z0-9._@+-]; must start alnum;
    // forbidden anywhere: '}' '{' '"' '\\' ',' '=' '/' ';' '<' '>' (JWT flatness, X.500 DN safety, XML attr safety); reserved: "anonymous","__anon__","rustak","takserver", prefix "__"
    pub fn as_str(&self) -> &str; pub fn eq_ignore_case(&self, cn: &str) -> bool;
}
pub enum UserKind { Person, Service }   pub enum UserSource { Local, Oidc }
pub struct User { pub id: UserId, pub username: Username, pub display_name: Option<String>, pub email: Option<String>, pub kind: UserKind, pub source: UserSource,
                  pub is_admin: bool, pub admin_override: Option<bool>, pub disabled: bool, pub oidc_issuer: Option<String>, pub oidc_subject: Option<String>,
                  pub created_at: DateTime<Utc>, pub last_login_at: Option<DateTime<Utc>> }
pub enum Direction { In, Out }           // IN = write (client → server), OUT = read
pub enum GroupKind { System, Manual, Oidc }
pub struct Group { pub id: GroupId, pub name: String, pub description: Option<String>, pub bitpos: u32, pub kind: GroupKind, pub created_at: DateTime<Utc> }
pub struct GroupMembership { pub user_id: UserId, pub group_id: GroupId, pub direction: Direction, pub source: MembershipSource /*Manual|Oidc*/ }
pub struct DeviceGroupState { pub device_id: DeviceId, pub group_id: GroupId, pub direction: Direction, pub active: bool }
pub struct Device { pub id: DeviceId, pub uid: String, pub user_id: UserId, pub callsign: Option<String>, pub takv: Option<TakVersion>, pub first_seen_at, pub last_seen_at, pub last_ip: Option<IpAddr>, pub last_certificate_id: Option<i64> }
pub enum CredentialKind { DevicePassword, EnrollmentToken, LocalPassword, ServiceToken }
pub struct Credential { pub id: CredentialId, pub user_id: UserId, pub kind: CredentialKind, pub label: String, pub hash: String /*PHC argon2id*/, pub hint: String /*first 4 chars*/,
                        pub created_at, pub expires_at: Option<DateTime<Utc>>, pub max_uses: Option<u32>, pub uses: u32, pub disabled: bool, pub last_used_at: Option<..>, pub created_by: Username }
pub struct MintedSecret { pub credential: Credential, pub secret: zeroize::Zeroizing<String> }   // returned once, never stored
pub fn generate_secret(kind: CredentialKind) -> Zeroizing<String>;   // DevicePassword: 5×4 Crockford base32 groups "xxxx-xxxx-xxxx-xxxx-xxxx" (100 bits); EnrollmentToken: 32 url-safe chars; ServiceToken: "rsk_" + 40
pub fn hash_secret(secret: &str) -> Result<String>;                  // Argon2id default params (m=19 MiB, t=2, p=1), SaltString::generate(OsRng)
pub enum Purpose { Enrollment /*DevicePassword|EnrollmentToken*/, OAuthPassword /*DevicePassword|LocalPassword*/, Marti /*DevicePassword*/, StreamAuth /*DevicePassword*/, ServiceApi /*ServiceToken*/ }
pub struct Verified { pub user: Arc<User>, pub credential: Credential }
impl<S: Services> Credentials<S> {
    pub async fn mint(&self, user: &User, kind, label, expires_in: Option<Duration>, max_uses: Option<u32>, actor: &Username) -> Result<MintedSecret>;
    pub async fn verify(&self, username: &Username, secret: &str, purpose: Purpose, cache: &VerifiedSecretCache) -> Result<Verified, VerifyError /*NoSuchUser|Disabled|BadSecret|Expired|Exhausted*/>;
    //   constant-time-ish: always run one argon2 verify even when user unknown (dummy hash) to avoid user enumeration by timing
    pub async fn record_use(&self, id: CredentialId, consumed: bool) -> Result<()>;   // consumed=true only from signClient success (enrollment tokens)
    pub async fn revoke(&self, id: CredentialId, actor: &Username) -> Result<()>;      // also revokes certs with certificates.credential_id = id
}
```
Bitpos allocation: `__ANON__` = bitpos 1 (System, created by migration); next free = lowest unused in 2..=255 (bit-vector width 256 in the hub); deleting a group frees its bitpos only after all subscriptions are refreshed (soft-delete then reuse). Anonymous/TCP-anonymous principals get `__ANON__` IN+OUT; all persons get `__ANON__` IN+OUT by default unless `[auth] anon_group_default = false` (TAK's `x509addAnonymous`).

Effective groups for a subscription: `members(user) ∩ device_active(device)`; `PUT /Marti/api/groups/active?clientUid=` writes `device_group_state` and emits `t-x-g-c`.

```sql
CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL UNIQUE, display_name TEXT, email TEXT, kind TEXT NOT NULL, source TEXT NOT NULL, is_admin INTEGER NOT NULL DEFAULT 0, admin_override INTEGER, disabled INTEGER NOT NULL DEFAULT 0, oidc_issuer TEXT, oidc_subject TEXT, created_at TEXT NOT NULL, last_login_at TEXT, UNIQUE(oidc_issuer, oidc_subject));
CREATE TABLE groups (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, description TEXT, bitpos INTEGER NOT NULL UNIQUE, kind TEXT NOT NULL, created_at TEXT NOT NULL, deleted_at TEXT);
INSERT INTO groups (name, bitpos, kind, created_at) VALUES ('__ANON__', 1, 'system', strftime('%Y-%m-%dT%H:%M:%fZ','now'));
CREATE TABLE group_members (user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, group_id INTEGER NOT NULL REFERENCES groups(id) ON DELETE CASCADE, direction TEXT NOT NULL CHECK(direction IN ('IN','OUT')), source TEXT NOT NULL, PRIMARY KEY (user_id, group_id, direction));
CREATE TABLE devices (id INTEGER PRIMARY KEY, uid TEXT NOT NULL UNIQUE, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, callsign TEXT, takv TEXT /*json*/, first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL, last_ip TEXT, last_certificate_id INTEGER);
CREATE TABLE device_group_state (device_id INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE, group_id INTEGER NOT NULL REFERENCES groups(id) ON DELETE CASCADE, direction TEXT NOT NULL, active INTEGER NOT NULL, PRIMARY KEY (device_id, group_id, direction));
CREATE TABLE credentials (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, kind TEXT NOT NULL, label TEXT NOT NULL, hash TEXT NOT NULL, hint TEXT NOT NULL, created_at TEXT NOT NULL, created_by TEXT NOT NULL, expires_at TEXT, max_uses INTEGER, uses INTEGER NOT NULL DEFAULT 0, disabled INTEGER NOT NULL DEFAULT 0, last_used_at TEXT);
CREATE TABLE refresh_tokens (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE, token_hash TEXT NOT NULL UNIQUE, family TEXT NOT NULL, scope TEXT NOT NULL, created_at TEXT NOT NULL, expires_at TEXT NOT NULL, used_at TEXT, revoked_at TEXT, client TEXT);
CREATE TABLE revoked_jtis (jti TEXT PRIMARY KEY, expires_at TEXT NOT NULL);
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL, updated_by TEXT);
```

### provisioning.rs (JIT from OIDC)
```rust
pub struct VerifiedIdentity { pub issuer: String, pub subject: String, pub username: Username, pub display_name: Option<String>, pub email: Option<String>, pub claims: serde_json::Map<String, Value> }
pub struct GroupMappingConfig { pub groups_claim: String /*"groups"*/, pub group_prefix: Option<String>, pub strip_prefix: bool, pub read_suffix: String /*"_READ"*/, pub write_suffix: String /*"_WRITE"*/, pub read_only_group: Option<String>, pub auto_create: bool }
pub fn map_group_claims(claims: &Map, cfg: &GroupMappingConfig) -> Vec<(String, Direction)>;
// claims[groups_claim] as array of strings (or space-separated string); keep names starting with prefix (case-insensitive), strip if configured;
// read_only = names contains read_only_group (removed from set); for each: ends_with(read_suffix) → OUT only (strip); ends_with(write_suffix) → IN only (strip); else IN+OUT; IN suppressed when read_only. (LdapAuthenticator.groupNamesToGroups semantics)
pub async fn provision_from_identity<S: Services>(s: &S, id: &VerifiedIdentity, acl: &AclOutcome /*allowed, is_admin*/) -> Result<Arc<User>, ProvisionError>;
// lookup by (issuer, subject) → else if cfg.link_by_username lookup by username (source Local → convert to Oidc) → else create(kind Person, source Oidc)
// refuse if !acl.allowed (Forbidden) or user.disabled; update display/email/last_login; is_admin = admin_override.unwrap_or(acl.is_admin)
// replace memberships where source=Oidc with mapped groups (auto-create kind=Oidc groups if allowed; unknown groups otherwise ignored + audit); manual memberships untouched
```

## 5. `auth/` — principal, resolution, OAuth2 server, OIDC federation

### principal.rs / resolve.rs
```rust
pub enum AuthMethod { ClientCert { fingerprint: Sha256Fingerprint, serial: Vec<u8> }, Bearer { jti: String, scope: String }, Basic { credential: CredentialId, kind: CredentialKind }, StreamAuth { credential: CredentialId }, Anonymous, SetupToken }
pub struct GroupGrant { pub id: GroupId, pub name: String, pub bitpos: u32, pub direction: Direction }
pub struct Principal { pub user: Arc<User>, pub device_uid: Option<String>, pub groups: Vec<GroupGrant>, pub is_admin: bool, pub method: AuthMethod, pub client_ip: Option<IpAddr> }
impl Principal { pub fn username(&self) -> &Username; pub fn in_bits(&self) -> u256/BitSet; pub fn out_bits(&self) -> BitSet; pub fn has_group(&self, name, dir) -> bool; pub fn anonymous(anon: GroupGrant×2, ip) -> Self; }

pub enum BasicPolicy { Off, EnrollmentOnly /*/Marti/api/tls/**, /oauth/token*/, All }
pub struct ListenerAuthPolicy { pub cert: bool, pub bearer: bool, pub basic: BasicPolicy, pub anonymous: bool }
//  Public: {cert:false, bearer:true, basic:EnrollmentOnly, anonymous:false}   Marti: {cert:true, bearer:true, basic: cfg.allow_basic? All:Off, anonymous:false}
pub enum AuthFailure { Missing, Invalid(&'static str), Forbidden(&'static str), RateLimited(Duration) }
pub async fn resolve_principal<S: Services>(s: &S, req: &ServiceRequest, policy: &ListenerAuthPolicy) -> Result<Principal, AuthFailure>;
// 1) policy.cert && req.conn_data::<PeerCertificate>() → cn → Username::parse → users.get (not disabled) → certificates.get_by_fingerprint (exists, !revoked, user matches; else Forbidden) → device from certificates.device_id / clientUid query → groups → Principal{ClientCert}
// 2) policy.bearer && Authorization: Bearer → jwt.verify (RS256 only, aud/iss/exp/nbf) → revoked_jtis miss → users.get(sub) enabled → Principal{Bearer}
// 3) basic allowed for path → parse → ratelimit.check(ip, username)? → credentials.verify(.., Purpose by path) → Principal{Basic}; failure → ratelimit.record_failure
// 4) policy.anonymous → Principal::anonymous  else Err(Missing)
pub fn marti_auth(policy: ListenerAuthPolicy) -> impl Transform  // from_fn middleware; inserts Principal; 401 body per path: /oauth/token → {"error":"invalid_grant","error_description":"Bad credentials"}; /Marti/** → text "Unauthorized" + `WWW-Authenticate: Basic realm="rustak"` only when basic allowed and no cert/bearer presented
pub struct Auth(pub Principal); pub struct Admin(pub Principal);   // FromRequest extractors
```

### ratelimit.rs / cache.rs
```rust
pub struct RateLimiter { buckets: Mutex<HashMap<(IpAddr, String), Bucket>>, attempts: u32 /*10*/, window: Duration /*1m*/, lockout: Duration /*15m*/ }
impl RateLimiter { pub fn check(&self, ip, user: &str) -> Result<(), Duration>; pub fn record_failure(&self, ip, user); pub fn record_success(&self, ip, user); pub fn sweep(&self); }
pub struct VerifiedSecretCache { map: Mutex<HashMap<[u8;32] /*sha256(cred_id||secret)*/, Instant>>, ttl: Duration /*5m*/, cap: usize /*4096*/ }
```

### oauth_server/jwt.rs, keys.rs
```rust
pub const JWT_HEADER_JSON: &str = r#"{"alg":"RS256","typ":"JWT"}"#;   // 27 bytes → 36 base64url chars, no partial group
#[derive(Serialize, Deserialize)]                                      // field order is wire order; every value scalar
pub struct AccessClaims { pub sub: String, pub aud: String, pub iss: String, pub iat: i64, pub nbf: i64, pub exp: i64, pub jti: String, pub scope: String,
                          #[serde(default, skip_serializing_if = "Option::is_none")] pub dev: Option<String> /*device uid*/ }
pub struct SigningKey { pub id: String /*sha256(spki)[..8]*/, pub encoding: EncodingKey, pub decoding: DecodingKey, pub public_pem: String, pub jwk: Jwk }
pub struct JwtIssuer { active: SigningKey, previous: Vec<SigningKey>, issuer: String, audience: String, ttl: Duration }
impl JwtIssuer {
    pub async fn load_or_create<S: Services>(s: &S, cfg: &AuthConfig) -> Result<Self>;   // kv "auth": "jwt.rsa2048.<id>" Sealed pkcs8 (SecretContext::AuthKey{name}); "jwt.active" = id; rotation keeps previous for verify
    pub fn issue(&self, user: &User, scope: &str, device_uid: Option<&str>, ttl: Option<Duration>) -> Result<(String, AccessClaims)>;
    //  payload = serde_json::to_vec(&claims) → debug_assert!(payload.iter().filter(|b| **b == b'}').count() == 1);  token = format!("{HEADER_B64}.{payload_b64}.{}", jsonwebtoken::crypto::sign(signing_input, &key.encoding, Algorithm::RS256)?)
    pub fn verify(&self, token: &str) -> Result<AccessClaims, JwtError>;   // decode_header: alg must be RS256 (reject HS*/none); Validation{aud, iss, exp, nbf, leeway 60s, required ["exp","aud","iss","sub"]}; try active then previous
    pub fn token_key_json(&self) -> Value;   // {"alg":"SHA256withRSA","value":"<public PEM>"}  (Spring format TAK clients expect at /oauth/token_key)
    pub fn jwks(&self) -> Value;             // {"keys":[{kty:RSA,use:sig,alg:RS256,kid,n,e}…]}
}
```

### oauth_server/token.rs, refresh.rs
```rust
pub async fn token<S: Services>(s: Data<S>, req: HttpRequest, form: Form<TokenForm>) -> HttpResponse;
// grant_type=password: username (normalised), password → ratelimit → credentials.verify(Purpose::OAuthPassword) → issue(scope "marti" [+ "admin" if is_admin]) → 200 {"access_token","token_type":"bearer","expires_in":3600,"scope","refresh_token"}
//   bad creds → 401 {"error":"invalid_grant","error_description":"Bad credentials"}; missing → 400 invalid_request; unknown grant → 400 unsupported_grant_type; limited → 429 + Retry-After
// grant_type=refresh_token: hash lookup → not used/revoked/expired → rotate (mark used_at, insert new same family) → new access; reuse of a used token revokes the family (replay detection)
// grant_type=authorization_code (our own codes from /oauth/authorize, PKCE) → same issuance
// Content-Type: application/json exactly (HttpResponse::json does this); Cache-Control: no-store
pub struct RefreshTokens<S> { .. } // mint(user, scope, client) -> (opaque 32-byte url-safe, row), redeem(token) -> Result<(User, scope)>, revoke_family, revoke_user
```

### oauth_server/authorize.rs, login.rs, state.rs (TAK-Server-style federation)
```rust
pub struct PendingAuth { pub kind: PendingKind /*BrowserRedirect{return_to}|OauthCode{client_redirect_uri, client_state, client_pkce_challenge}|UiPopup*/, pub pkce_verifier: String, pub nonce: String, pub started_at, pub expires_at /*+10m*/ }
pub struct PendingAuths<S> { .. }  // kv partition "auth-state" keyed by state; begin(state, PendingAuth), claim(state) one-shot (lifted PendingAuthorizations)
```
| Method/Path | Behaviour |
|---|---|
| `GET /login/auth` (and `GET /oauth/authorize?response_type=code&client_id&redirect_uri&state&code_challenge` when `client_id` is a registered public client) | Generate `state` (32 B urlsafe), PKCE verifier/challenge, nonce; store `PendingAuth` keyed by `sha256(state)`; set cookie `state=<state>; HttpOnly; Secure; SameSite=Lax; Path=/login`; 302 to IdP `authorization_endpoint?response_type=code&client_id&redirect_uri=<issuer>/login/redirect&scope=openid …&state=sha256(state)&nonce&code_challenge&code_challenge_method=S256` |
| `GET /login/redirect?code&state` | Require cookie `state`, check `sha256(cookie)==state` (TAK rule) **and** claim `PendingAuth` (one-shot, unexpired); `oidc::exchange_code(code, redirect_uri, pkce_verifier)`; `oidc::validate` (nonce match); `provision_from_identity` + ACLs; issue our JWT (+ refresh); clear state cookie; set `access_token` cookie (HttpOnly, Secure, SameSite=Lax, chunked at 4000 B like TAK) ; then per `PendingKind`: BrowserRedirect → 302 `/login/redirect.html` (served by our UI: reads cookie? no — page calls `/token/access`); OauthCode → 302 `client_redirect_uri?code=<our code>&state=<client_state>`; on failure 302 `/login/error.html?reason=` (never leak IdP errors) |
| `GET /login/refresh` | Uses `refresh_token` cookie (HttpOnly, path `/login`) → rotate → new `access_token` cookie → 302 `/login/redirect.html`; failure → clear cookies, 401 |
| `GET /login/authserver` | `{"version":"3","type":"java.lang.String","data":"<oidc display name>","nodeId":…}` or 404 when OIDC not configured |
| `GET /login/.well-known/openid-configuration` | `{"authorization_endpoint": <IdP>, "token_endpoint": <IdP>}` (TAK shape; IdP values from discovery) |
| `GET /token/access` | Auth via cookie or bearer; returns `ApiResponse<String>` with the access token when `[auth] allow_access_token_retrieval = true` (default true — WebTAK-style JS needs it), else 403 |
| `GET|POST /logout` | Revoke jti + refresh family; clear cookies; 204 |
| `GET /oauth/token_key` | `token_key_json()` |
| `GET /oauth/jwks` , `GET /.well-known/openid-configuration` (ours) | JWKS / minimal discovery for our issuer (future CloudTAK OIDC) |

Cookie-authenticated requests (`access_token` cookie) are accepted **only** on `/login/*`, `/token/access`, `/logout` and the UI's static pages, never on `/Marti/**` or `/api/v1/**` (no CSRF surface there — bearer only).

### oidc/ (IdP client)
```rust
pub struct OidcProvider<S: Services> { cfg: Arc<OidcConfig>, s: S }
pub struct AuthorizeParams { pub state_hash: String, pub nonce: String, pub pkce_challenge: String, pub redirect_uri: String, pub extra_scopes: Vec<String> }
impl<S: Services> OidcProvider<S> {
    pub async fn discovery(&self) -> Result<OidcDiscovery>;                      // lifted (cache 1h)
    pub fn authorize_url(&self, d: &OidcDiscovery, p: &AuthorizeParams) -> Url;
    pub async fn exchange_code(&self, code: &str, redirect_uri: &str, pkce_verifier: Option<&str>) -> Result<TokenSet>;   // lifted + code_verifier
    pub async fn validate_id_token(&self, id_token: &str, expected_nonce: Option<&str>) -> Result<Map<String, Value>>;    // lifted (asymmetric only, kid refetch, required exp/aud/iss) + nonce
    pub fn identity_from_claims(&self, claims: Map) -> Result<VerifiedIdentity>;   // username_claim (default preferred_username → sub), name/email
}
pub fn pkce_pair() -> (String /*verifier 64 urlsafe*/, String /*S256 challenge*/);
```

### acl.rs
```rust
pub struct AuthRequestFilter<'a> { pub method: &'a str, pub path: &'a str, pub client_ip: Option<String>, pub headers: &'a HeaderMap, pub claims: Option<&'a Map<String, Value>>, pub username: &'a str, pub source: &'a str }
impl Filterable for AuthRequestFilter<'_> { /* "method","path","client_ip","headers.*","claims.*","username","source" */ }
pub struct AclOutcome { pub allowed: bool, pub is_admin: bool }
pub fn evaluate(cfg: &AuthConfig, f: &AuthRequestFilter<'_>) -> AclOutcome;   // user_acl default "true", admin_acl default "false"
```

### mission_token.rs
```rust
pub enum MissionTokenKind { Subscription, Invitation, Access }   // names SUBSCRIPTION|INVITATION|ACCESS
pub struct MissionTokenClaims { pub kind: MissionTokenKind, pub id: String, pub mission_name: String, pub mission_guid: Uuid, pub exp: Option<i64> }
pub struct MissionTokens { key: Zeroizing<[u8; 32]> }   // kv "auth": "mission_hs256" Sealed; rotation = invalidates outstanding tokens (documented)
impl MissionTokens {
    pub fn issue(&self, c: &MissionTokenClaims) -> Result<String>;   // Map: {"sub": KIND, KIND: id, "MISSION_NAME": name, "MISSION_GUID": guid, "iat", ["exp"]} via jsonwebtoken HS256
    pub fn verify(&self, token: &str) -> Result<MissionTokenClaims>; // Validation{HS256, validate_exp only if present, no aud}
    pub fn from_headers(h: &HeaderMap) -> Option<&str>;               // MissionAuthorization first, then Authorization; strip "Bearer " (case-insensitive)
}
```

### stream_auth.rs (hook for `stream/`)
```rust
pub struct StreamAuth<S: Services> { .. }
impl<S: Services> StreamAuth<S> {
    pub async fn from_peer_cert(&self, cert: &PeerCertificate, peer: SocketAddr) -> Result<Principal, AuthFailure>;   // same rules as resolve step 1; updates devices.last_seen on first CoT
    pub async fn from_auth_message(&self, username: &str, password: &str, uid: &str, callsign: Option<&str>, peer: SocketAddr) -> Result<Principal, AuthFailure>;  // Purpose::StreamAuth; ratelimit; upsert device(uid,user)
    pub async fn anonymous(&self, peer: SocketAddr) -> Result<Principal, AuthFailure>;   // only if [stream.tcp] auth = "anonymous"
}
```

### Audit events (`auth/audit.rs` constants)
`auth.login.success|failure` (method, username, ip), `auth.ratelimit.lockout`, `auth.token.refresh|revoke`, `auth.oidc.provisioned|denied`, `pki.csr.rejected` (reason), `pki.cert.issued|revoked`, `pki.server_cert.rotated`, `acme.issued|renew.failed|account.created`, `credential.minted|used|revoked|expired`, `setup.completed`.

### Threat model notes (what each control is for)
- Brute force on Basic and password grant: `RateLimiter` (10/min per ip+user, 15 min lockout), argon2id, constant-time dummy verify for unknown users, audit + UI lockout indicator. Device passwords are 100-bit random so offline cracking of a leaked DB is moot; `LocalPassword` (user-chosen) relies on argon2.
- Token replay: access TTL 1h; `jti` revocation on logout/user disable/credential revoke; refresh rotation with family revocation on reuse; refresh cookie scoped to `/login`.
- CSRF on `/login/redirect`: TAK's `sha256(cookie)==state` plus server-side one-shot `PendingAuth` (10 min) plus PKCE (code cannot be redeemed by anyone without the verifier) plus nonce (ID token bound to this flow). `SameSite=Lax` cookies; bearer-only on APIs.
- JWT algorithm confusion: our verifier accepts RS256 only; IdP verifier rejects HS*/none and requires `kid` in the published JWKS (lifted tests).
- CN spoofing via CSR: CN must equal the authenticated username; the entire subject, SANs and extensions are **replaced** by ours (we never copy RDNs); CSR signature verified; key size policy.
- Certificate revocation latency: revocation cache updated synchronously before `revoke()` returns; live subscriptions torn down; Marti requests re-check the DB row per request (cheap indexed lookup) so even a stale cache cannot extend access beyond one request.
- Enumeration: `/oauth/token` and Basic failures are indistinguishable for unknown user vs wrong password; `/login/redirect` failures are generic.
- Key material: CA key, JWT key, ACME account key/cert key sealed with `SecretStore` (AAD bound to key name), key file beside DB (automate pattern); `rustak pki export-ca` CLI for backup; never logged (`Debug` impls redact).

## 6. Endpoint tables

### TAK-compatible (Marti/OAuth) — mounted on public (443/8446) and Marti (8443) listeners unless noted

| Method/Path | Listener | Auth | Request | Response |
|---|---|---|---|---|
| `GET /Marti/api/tls/config` | both | Basic (Enrollment) or Bearer or cert | — | `200 text/xml`: `<?xml version="1.0" encoding="UTF-8" standalone="yes"?><ns2:certificateConfig xmlns:ns2="com.bbn.marti.config"><nameEntries><nameEntry name="O" value="…"/><nameEntry name="OU" value="…"/></nameEntries></ns2:certificateConfig>` (always ≥2 entries; defaults O=rustak, OU=EUD) |
| `POST /Marti/api/tls/signClient/v2?clientUid=&version=` | both | Basic (Enrollment) or Bearer | body PEM or bare base64 or DER CSR (`Content-Type` ignored) | Accept dispatch per TAK: JSON `{"signedCert":"<b64 64-col>","ca0":"…"[,"ca1"]}` (`application/json`), or `application/xml` `<enrollment>`; 400 for other Accept; 400 `{"error":"csr_invalid","reason":…}` on policy failure; 403 when CN≠user. Side effects: certificates row, device upsert (uid=clientUid), credential use consumed, audit |
| `POST /Marti/api/tls/signClient?clientUid=&version=` | both | same | same | `200 application/octet-stream` PKCS#12 (legacy PBE, password `atakatak`, aliases `signedCert`,`ca0…`) |
| `GET /Marti/api/tls/profile/enrollment?clientUid=` | both | same | — | 204 until M3 (then zip) |
| `GET /Marti/api/version` , `GET /Marti/api/version/config` | both | any incl. optional (version: none) | — | `text/plain` `rustak-<ver>`; config JSON |
| `POST /oauth/token` | both | none (grant carries creds) | form `grant_type=password|refresh_token|authorization_code` | see token.rs |
| `GET /oauth/token_key`, `GET /oauth/jwks` | both | none | — | Spring `token_key` JSON / JWKS |
| `GET /oauth/authorize` | public | none | OAuth2 code request from a registered public client (PKCE required) | 302 to IdP (federated) or to our login page for local users |
| `GET /login/auth`, `/login/redirect`, `/login/refresh`, `/login/authserver`, `/login/.well-known/openid-configuration`, `GET /token/access`, `/logout` | public | see table above | | |
| `GET /.well-known/acme-challenge/{token}` | :80 plain + public | none | — | key authorization or 404 |
| `GET /Marti/api/groups/all`, `PUT /Marti/api/groups/active?clientUid=` | Marti (+public for CloudTAK) | Principal | — | per plan A.2 (uses `Principal.groups`, `device_group_state`) |

### Admin API `/api/v1` (bearer = our JWT with `scope` containing `admin` for admin routes; `me`/credentials self-service for any user)

| Method/Path | Who | Purpose |
|---|---|---|
| `GET /api/v1/auth/metadata` | public | `{authorization_endpoint, client_id, scopes, pkce:true, local_login:bool}` |
| `POST /api/v1/auth/token` | public | `{code, redirect_uri, code_verifier}` → exchange+validate+provision+ACL → `{token, refresh_token, expires_in}` (our JWT) |
| `POST /api/v1/auth/login` | public | `{username, password}` local login (LocalPassword/DevicePassword) → same response; rate limited |
| `POST /api/v1/auth/refresh` | public | `{refresh_token}` → rotated pair |
| `POST /api/v1/auth/logout` | user | revoke jti + family |
| `GET /api/v1/me` | user | user, groups, is_admin, method |
| `GET /api/v1/setup/status` | public | `{needs_setup, steps_done}` |
| `POST /api/v1/setup/admin` | setup token | `{setup_token, username, password}` → admin user + LocalPassword; only while no admin exists |
| `PUT /api/v1/setup/settings` | admin (or setup token during wizard) | wizard-managed settings (hostname, name_entries, acme, oidc, acls) |
| `GET/POST /api/v1/users`, `GET/PATCH/DELETE /api/v1/users/{username}` | admin | list/create local/service users; patch display, admin_override, disabled (disable ⇒ revoke certs+tokens) |
| `GET /api/v1/users/{username}/groups`, `PUT …/groups` | admin | replace manual memberships `[{group, direction}]` |
| `GET/POST /api/v1/groups`, `PATCH/DELETE /api/v1/groups/{name}` | admin | CRUD (bitpos auto) |
| `GET /api/v1/devices`, `GET/DELETE /api/v1/devices/{uid}`, `PUT /api/v1/devices/{uid}/active-groups` | admin (self for own) | device list with last seen/cert; active channel state |
| `GET /api/v1/credentials`, `POST /api/v1/credentials` | user (self) / admin (any user) | mint `{kind, label, expires_in, max_uses}` → `{credential, secret}` once |
| `GET /api/v1/credentials/{id}/qr` | self/admin | for EnrollmentToken: `{url: "tak://com.atakmap.app/enroll?host=…&username=…&token=…", host}` (UI renders QR with `qrcode` in wasm; the secret is returned only at mint time, so this endpoint accepts the secret in the request or the UI renders from the mint response — decide: UI-side only, endpoint returns host/username template) |
| `DELETE /api/v1/credentials/{id}` | self/admin | revoke (cascades certs issued with it) |
| `GET /api/v1/certificates?user=&revoked=`, `POST /api/v1/certificates/{fingerprint}/revoke` | admin (self list) | list/revoke |
| `POST /api/v1/certificates/issue-package` | admin | `{username, client_uid, callsign, validity}` → server-side key+CSR+cert → `{p12_b64, truststore_b64, password}` for the profiles module |
| `GET /api/v1/pki/ca` (`.crt` download), `GET /api/v1/pki/truststore.p12`, `GET /api/v1/pki/status`, `POST /api/v1/pki/server-cert/rotate` | ca download public; rest admin | CA distribution; server cert info/rotate |
| `GET /api/v1/acme/status`, `POST /api/v1/acme/renew` | admin | `CertState`, last error, next check; force order |
| `GET /api/v1/audit?category=auth|pki` | admin | audit feed |

Yew pages: `setup` (wizard), `devices` (list, active channels), `credentials` (mint dialog shows secret once + QR), `users`, `groups`, `certificates` (revoke), `pki` (CA download, ACME status/banner).

## 7. Enrollment flows

**ATAK via QR (`tak://com.atakmap.app/enroll?host=H&username=U&token=T`)**
1. User (UI `credentials` page) mints `EnrollmentToken` (label = device name, TTL 15 min, max_uses 1) → UI renders QR with `host = server.public_hostname`, `username`, `token` (URL-encoded).
2. ATAK: TLS to `H:8446` using system CAs + hostname verification (public cert required) → `GET /Marti/api/tls/config` Basic `U:T`, `Accept: application/xml` → `marti/tls.rs::config` → `resolve_principal` (Basic, Purpose::Enrollment) → XML.
3. ATAK (commoncommo) generates RSA-2048, CSR `CN=U, O, OU` → `POST /Marti/api/tls/signClient/v2?clientUid=<ANDROID-…>&version=<atak ver>`, `Content-Type: application/octet-stream`, `Accept: application/xml`, body = base64 (no armour) → `parse_csr` → `CsrPolicy::validate(cn==U)` → `issue_client_cert` → certificates row (device upsert uid=clientUid, credential_id) → `credentials.record_use(consumed=true)` → `<enrollment><signedCert>…</signedCert><ca>root</ca></enrollment>`.
4. ATAK builds client p12 + truststore (from `<ca>`), stores per server:port; `GET /Marti/api/tls/profile/enrollment?clientUid=` (204 until M3).
5. ATAK connects `H:8089:ssl` with the client cert → `stream_server_config` (client auth mandatory) → `RustakClientVerifier` (chain + not revoked + known) → `stream_auth.from_peer_cert` → `Principal` → subscription groups; Marti calls to `H:8443` carry the cert → `on_connect_capture` → `resolve_principal` step 1.

**ATAK manual (Network Preferences → add server, "Enroll for certificate", username + device password)**: identical to steps 2-5 with `DevicePassword` (multi-use, optional expiry). Alternative when 8446 is not publicly trusted: user imports the **trust-bootstrap data package** (profiles module builds it from `pki::p12::truststore(chain, {password: p12_password})` + `.pref` with `cot_streams`: `connectString0=H:8089:ssl`, `useAuth0=true`, `enrollForCertificateWithTrust0=true`, `caLocation0=cert/truststore-rustak.p12`, `caPassword0=<p12_password>`, `cacheCreds0=Cache credentials`); ATAK then prompts for username/device password and enrolls against 8446 trusting our CA (`CertificateEnrollmentClient.java:512-528` path). PKI provides: `truststore()` bytes + password + CA fingerprint for the manifest.

**CloudTAK (Server setup → login)**: `POST {webtak}/oauth/token` password grant (username lower-cased by CloudTAK; we normalise) → JWT (`sub` = normalised username) → node-tak `parse()` → `GET {webtak}/Marti/api/tls/config` Bearer → `POST {webtak}/Marti/api/tls/signClient/v2?clientUid=U (ETL)&version=3` **Basic** `U:devicepassword`, `Accept: application/json`, PEM CSR → JSON with 64-col bare base64 → CloudTAK stores cert/key → every later call mTLS to `{api}` (8443): `GET /Marti/api/version` etc. via `resolve_principal` step 1; stream `ssl://H:8089` with the cert. Bad password → 401 `invalid_grant`.

**Manual "config data package" for devices that cannot enroll (profiles module owns the zip)** — PKI provides, via `POST /api/v1/certificates/issue-package`: `generate_key(Rsa2048)` → internal CSR (`CertificateParams::serialize_request`) → `issue_client_cert` (issued_via `admin_package`) → `client_keystore(key, cert, chain, {password, friendly_name: username, legacy:true})` and `truststore(chain, …)`; structs: `pki::p12::P12Options`, `IssuedCert`, `pki::Pki::issue_package(username, client_uid, validity) -> IssuedPackage { client_p12: Vec<u8>, truststore_p12: Vec<u8>, password: String, cert: IssuedCert }`. The `.pref` (profiles) sets `certificateLocation0`, `clientPassword0`, `caLocation0`, `caPassword0`, `useAuth0=false`.

## 8. Config schema sketch and bootstrap

```toml
[server]
public_hostname = "tak.example.com"          # issuer, SANs, QR host, cookies; required unless setup wizard sets it
data_dir = "/data"

[web.public]
bind = ["0.0.0.0:443", "0.0.0.0:8446"]
cert_source = "acme"                         # "acme" | "files" | "internal"
cert_file = "/data/certs/public.crt"         # cert_source = "files": full chain PEM
key_file  = "/data/certs/public.key"
plain_bind = "0.0.0.0:80"                    # optional: ACME http-01 + 301 redirect; default unset
trust_proxy = false
allow_access_token_retrieval = true          # /token/access

[web.marti]
bind = "0.0.0.0:8443"
client_cert = "optional"                     # "optional" | "required"   (required breaks /oauth/token on 8443)
allow_basic = false
allow_bearer = true

[stream.tls]
bind = "0.0.0.0:8089"                        # client cert always required
min_tls = "1.2"                              # informational; ATAK needs 1.3 anyway
[stream.tcp]
enabled = false
bind = "0.0.0.0:8087"
auth = "credentials"                         # "credentials" | "anonymous"

[pki]
ca_name = "rustak Root CA"
key_type = "rsa2048"                         # "rsa2048" | "rsa3072" | "ecdsa-p256"
name_entries = [["O", "rustak"], ["OU", "EUD"]]
client_cert_validity = "365d"
server_cert_validity = "365d"
server_cert_renew_before = "30d"
server_names = ["tak.example.com", "tak.lan"]   # + server.public_hostname + acme.domains automatically
server_ips = ["192.168.1.10"]
require_known_cert = true
csr_min_rsa_bits = 2048
csr_allow_ecdsa = true
p12_password = "atakatak"
p12_legacy = true
channels_marker_eku = true

[acme]
enabled = true
directory = "letsencrypt"                    # "letsencrypt" | "letsencrypt-staging" | "https://…/directory"
contact = "mailto:me@example.com"
accept_tos = true
domains = ["tak.example.com"]
challenge = "tls-alpn-01"                    # "tls-alpn-01" | "http-01" (fallback to the other if unavailable)
renew_before = "30d"

[auth]
issuer = "https://tak.example.com"           # default: https://{public_hostname}
audience = "rustak"
access_token_ttl = "1h"
refresh_token_ttl = "30d"
local_login = true
user_acl = "true"
admin_acl = 'claims.groups contains "tak-admins"'
anon_group_default = true
enrollment_token_ttl = "15m"
device_password_ttl = ""                     # "" = no default expiry
rate_limit = { attempts = 10, window = "1m", lockout = "15m" }
setup_token_file = "/data/setup-token"       # first run only
# secret_key / previous_secret_keys as in automate (SecretStore)

[auth.oidc]
endpoint = "https://login.example.com/realms/home"
client_id = "rustak"
client_secret = "${{ env.RUSTAK_OIDC_CLIENT_SECRET }}"
scopes = ["profile", "email", "groups"]
username_claim = "preferred_username"        # fallback sub
groups_claim = "groups"
group_prefix = "tak-"
strip_group_prefix = true
read_suffix = "_READ"
write_suffix = "_WRITE"
read_only_group = ""
auto_create_groups = true
link_by_username = false
display_name = "Home SSO"                    # /login/authserver
```
All structs `#[serde(deny_unknown_fields)]`; example-config test as in automate. Durations via a `serde_duration` helper (lift).

**First-run bootstrap sequence**
1. `run()`: open DB, migrations; `SecretStore::load`; `load_or_create_root_ca`; `ServerCertManager::load_or_issue`; `JwtIssuer::load_or_create`; `MissionTokens::load_or_create`; `RevocationCache::reload`.
2. Public cert: `acme` → `AcmeManager::startup` (temporary self-signed installed first; order in background); `files` → `ByoCertSource::load`; `internal` → internal server cert.
3. If `users` has no admin: generate 32-byte setup token → write `setup_token_file` (0600) and log `Setup required: open https://<host>/setup and enter the token from <file>`; `/api/v1/setup/*` enabled.
4. Wizard (Yew `/setup`): step 1 token + admin username/password → `POST /setup/admin`; step 2 hostname + cert source (ACME email/ToS | upload paths | internal) → `PUT /setup/settings` (triggers ACME order; shows live `CertState`); step 3 OIDC (optional; "test" button hits discovery); step 4 O/OU; step 5 mint first device password + QR. `setup.completed` audit; setup token file deleted; setup routes return 410 afterwards.
5. Steady state: jobs — `acme_renew` (daily), `cert_reload` (60 s, files mode), `server_cert_rotate` (daily), `credential_expiry` (hourly: disable expired, prune revoked_jtis), `pending_auth_prune` (10 min), `ratelimit sweep`.

## 9. Test plan

**Unit (in-file `#[cfg(test)]`)**
- `pki/csr.rs`: PEM, bare base64 with/without newlines, base64url, raw DER, garbage, unsigned/forged signature (flip a bit → `verify_signature` fails), RSA-1024 rejected, ECDSA accepted, CN missing, CN case-mismatch accepted (`Alice` vs `alice`), CN mismatch rejected, CSR with SANs → parsed but `requested_sans` counted.
- `pki/issue.rs`: issued cert parses (x509-parser): subject exactly `CN=<user>,O=…,OU=…` in order, no SANs, EKU contains clientAuth (+ marker when flagged), KU digitalSignature|keyEncipherment (RSA) / digitalSignature (ECDSA), serial 16 bytes positive, SKI/AKI present and AKI == CA SKI, not_before ≈ now-5m, not_after capped by CA, chain verifies with webpki against the root; determinism of subject order across JSON/XML paths.
- `pki/ca.rs`: create → persist → reload gives same fingerprint; key sealed (kv value never contains PKCS#8 bytes); ECDSA variant.
- `pki/p12.rs`: round-trip through `p12-keystore` reader; algorithm OIDs in output = 3DES PBE and SHA-1 MAC when legacy; alias names `signedCert`/`ca0`; **compat check** with `openssl pkcs12 -legacy -info -noout` in an `#[ignore]` test when `openssl` is on PATH.
- `pki/pem.rs`: 64-col wrapping equals TAK `toPEM(false)` (golden: known DER → expected string); trailing newline; node-tak reconstruction (`BEGIN\n`+body+`END`) parses with rustls-pemfile.
- `pki/tls/resolver.rs`: resolve without SNI returns current; ALPN `acme-tls/1` with matching SNI returns challenge cert; mismatched SNI → None; challenge cleared → None for acme, current otherwise.
- `pki/tls/client_verifier.rs`: valid cert ok; revoked → `CertificateError::Revoked`; unknown-but-valid → rejected when `require_known_cert`; expired; foreign CA; mandatory flag propagates.
- `auth/oauth_server/jwt.rs`: header JSON is byte-identical to `{"alg":"RS256","typ":"JWT"}` and `len % 3 == 0`; payload contains exactly one `}`; `aud` is a string; **CloudTAK-style parse simulation**: implement Node's lenient decoder (drop every char outside `A-Za-z0-9+/-_=`, decode without padding, discard trailing partial bits), `split('}')`, `JSON.parse(split[1] + '}')` → `sub == username`, `aud/nbf/exp/iat` present; property test over 200 random usernames/lengths; verify rejects HS256/`none`/wrong aud/expired/previous-key-after-removal; `token_key_json` PEM parses; JWKS `n`/`e` match.
- `auth/mission_token.rs`: claim key equals kind name; `MissionAuthorization` precedence over `Authorization`; tampered → error.
- `identity/username.rs`: normalisation table (case, whitespace, `Alice@Example.COM` → lowercase, rejects `}`/`"`/`,`/`=`, reserved names).
- `identity/credentials.rs`: mint → verify ok; wrong secret; expiry; max_uses consumed only via `record_use(consumed=true)`; disabled; cache hit skips argon2 (count via test hook); unknown user still costs one argon2 (timing shape).
- `identity/provisioning.rs`: TAK suffix table (`ops_READ` → OUT, `ops_WRITE` → IN, `ops` → both, read_only_group suppresses IN, prefix filter/strip, non-array claim); admin via `admin_acl`; `admin_override` wins; disabled user refused; manual memberships preserved across re-provision.
- `auth/ratelimit.rs`: window/lockout arithmetic; per-(ip,user) isolation.
- `auth/oauth_server/state.rs`: one-shot, expiry, unrecognised indistinguishable (lifted tests).
- `auth/oidc/*`: lifted automate tests (alg confusion, kid refetch counts, missing aud) + nonce mismatch rejected + PKCE verifier sent in exchange (wiremock body matcher).

**Integration (`rustak-server/tests/`, in-process `run()` with `TestCa`, `TestIdentityProvider`, ephemeral ports)**
- `enroll_json.rs`: reqwest (system-trust off, trust test public cert) → `POST /oauth/token` password → JWT → `GET tls/config` Bearer (assert root `ns2:certificateConfig`, 2 entries) → `POST signClient/v2` Basic + PEM CSR + `Accept: application/json` → reconstruct PEM → reqwest identity (rcgen key + cert) → mTLS `GET /Marti/api/version` on Marti port → 200 text; assert `certificates` row, device uid `alice (ETL)`.
- `enroll_xml.rs`: ATAK shape: Basic with EnrollmentToken, `Accept: application/xml`, `Content-Type: application/octet-stream`, bare base64 body → `<enrollment>` with ≥1 `<ca>`; second enrollment with same token → 401 (consumed); token expiry → 401.
- `enroll_v1_p12.rs`: legacy endpoint → parse p12 with `p12-keystore`, aliases.
- `csr_cn_spoof.rs`: CSR `CN=bob` with alice's creds → 403; CSR with SAN/extra RDNs → issued cert has none.
- `mtls_revoke.rs`: enroll → mTLS ok → `POST /api/v1/certificates/{fp}/revoke` (admin JWT) → new connection handshake fails (`rustls` alert, reqwest error, no HTTP status) and a live stream TLS connection is closed; unknown-cert (signed by TestCa directly, not via DB) refused when `require_known_cert`.
- `stream_tls_principal.rs`: tokio-rustls client with enrolled cert → `<auth>`-less connect → subscription groups = memberships ∩ active; TCP `<auth>` with device password → principal; wrong password → connection closed after error event.
- `oidc_popup_flow.rs`: `TestIdentityProvider` extended with `/authorize` (302 back with code+state) and `/token` (validates `code_verifier` S256, returns id_token with nonce) → SPA-style: `GET /api/v1/auth/metadata` → follow authorize → `POST /api/v1/auth/token {code, redirect_uri, code_verifier}` → our JWT; `/api/v1/me` shows provisioned user + mapped groups; `admin_acl` from claims; refresh rotation + reuse → family revoked.
- `login_redirect_flow.rs`: `GET /login/auth` → state cookie + IdP redirect with `state=sha256(cookie)`, PKCE params → callback `/login/redirect` → `access_token` cookie set, 302 `/login/redirect.html`; replay of the same callback → 4xx; tampered state → 4xx; `GET /token/access` with cookie → token; `/login/.well-known/openid-configuration` shape; `/login/authserver` ApiResponse.
- `oauth_errors.rs`: bad password → 401 body `invalid_grant`/`Bad credentials`; `Content-Type: application/json` exactly (no charset); 11th attempt → 429; unsupported grant → 400.
- `acme_tls_alpn.rs` (unit-ish): drive `order()` against a mock ACME directory? Too heavy — instead test the pieces: challenge cert has `id-pe-acmeIdentifier` (1.3.6.1.5.5.7.1.31) critical with SHA-256(key_auth); resolver serves it for ALPN `acme-tls/1`+SNI via a real rustls client handshake with `alpn_protocols=[acme-tls/1]`; HTTP-01 route serves the token. Full ACME: **optional Pebble job** (`ghcr.io/letsencrypt/pebble`, `PEBBLE_VA_NOSLEEP=1`, config `httpPort`/`tlsPort` pointed at the test server; `RUSTAK_TEST_PEBBLE_DIR` env enables the test; document in `docs/deployment.md`) plus a manual staging checklist (`directory = "letsencrypt-staging"`).
- `tls_versions.rs`: rustls client restricted to TLS 1.2 with ECDHE succeeds (CloudTAK path); document/`#[ignore]` shell test `openssl s_client -connect :8089 -cipher 'DEFAULT:!ECDH' -cert … -key …` negotiates TLSv1.3 (CI job with openssl 3 installed on the runner).
- `setup_bootstrap.rs`: fresh DB → `/api/v1/setup/status` needs_setup → wrong token 403 → correct → admin created → `/api/v1/auth/login` works → setup routes 410; `/api/v1/users` requires admin scope.

**e2e (Playwright)**: setup wizard happy path; mint device password shows secret once and QR image; revoke certificate row disappears from device page.

## 10. Ordered implementation steps (M2 then M5) with verification

| # | Step | Files | Verify |
|---|---|---|---|
| M2.1 | Migrations + repos for users/groups/members/devices/credentials/certificates/settings; `Username`; `__ANON__` seed | `db/migrations/0002_identity.sql`, `db/repos/*`, `identity/{username,users,groups,members,devices,credentials}.rs` | migration test at each version; username table tests; credential mint/verify tests |
| M2.2 | `pki/keys.rs`, `ca.rs`, `pem.rs`, `SecretContext::{PkiKey,AuthKey}` additions in `crypto.rs` | as named | CA create/reload test; `ca.crt` written; sealed key |
| M2.3 | `csr.rs`, `issue.rs`, `revoke.rs` (cache), `certificates` repo | | unit tests in §9; golden CSRs generated with `openssl req` checked in as fixtures (our own) |
| M2.4 | `server_cert.rs`, `tls/{mod,resolver,client_verifier,peer}.rs`; web listeners: public (temporary self-signed/internal), Marti (`bind_rustls_0_23` + `on_connect`), stream TLS acceptor uses `stream_server_config` | `web/mod.rs`, `stream/listener_tls.rs` | in-process handshake tests (client cert optional vs required; revoked rejected); `PeerCertificate` visible in a probe handler |
| M2.5 | `auth/{principal,resolve,basic,bearer,ratelimit,cache,acl,audit}.rs`; `marti_auth` middleware; `marti/version.rs` | | `oauth_errors.rs` partially (Basic paths), `resolve` unit tests with fake `PeerCertificate` |
| M2.6 | `oauth_server/{jwt,keys,token,refresh}.rs` password + refresh grants, `token_key`, `jwks` | | JWT alignment + CloudTAK parse simulation; `enroll_json.rs` token half |
| M2.7 | `marti/tls.rs`: config XML, signClient v2 (JSON/XML), v1 p12; device upsert; enrollment token consumption; `p12.rs` | | `enroll_json.rs`, `enroll_xml.rs`, `enroll_v1_p12.rs`, `csr_cn_spoof.rs`, `mtls_revoke.rs` |
| M2.8 | `stream_auth.rs` hook; hub uses `Principal.groups`; `PUT groups/active` per device | `stream/connection.rs` integration | `stream_tls_principal.rs` |
| M2.9 | `acme/*`, `jobs/{acme_renew,cert_reload,server_cert_rotate}.rs`, `:80` plain listener, `cert_source` switch, `ByoCertSource` | | resolver/challenge unit tests; Pebble optional; manual staging issuance on a real host (**M2 exit gate: ATAK QR enrol over public cert; CloudTAK setup+login+cert**) |
| M2.10 | Admin API: setup/bootstrap, credentials(+QR), devices, certificates, pki, acme status; Yew pages `setup`, `credentials`, `devices`, `certificates`, `pki` | `web/api/*`, `rustak-api/src/*`, `rustak-ui/src/pages/*` | `setup_bootstrap.rs`; e2e wizard + QR |
| M5.1 | `auth/oidc/*` (lift + PKCE + nonce), `testing/oidc.rs` extended with code-flow mocks | | lifted tests green; nonce/PKCE tests |
| M5.2 | `identity/provisioning.rs` + `acl.rs` outcome; `web/api/auth.rs` popup exchange → our JWT; `rustak-ui/src/auth.rs` with PKCE | | `oidc_popup_flow.rs`; group mapping table |
| M5.3 | `oauth_server/{authorize,login,state}.rs` TAK-style `/login/*`, `/oauth/authorize`, cookies, `/token/access`, `/logout`, our discovery/JWKS | | `login_redirect_flow.rs`; manual WebTAK-style page check |
| M5.4 | Users/groups admin API + Yew pages; disable ⇒ revoke certs/tokens; audit views | | admin API tests; e2e |
| M5.5 | Hardening pass: rate-limit sweeps, `revoked_jtis` pruning, `Debug` redaction audit (grep test that `format!("{:?}")` of key types contains no `BEGIN`/base64 of key), file-length lint, docs `docs/compat/auth.md`, `docs/compat/enrollment.md` | | CI green; **M5 exit gate: CloudTAK login with device password; browser SSO for Yew UI; `/login/*` flow** |

## 11. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| rustls has no TLS 1.2 RSA-kx/DHE; ATAK's `DEFAULT:!ECDH` kills TLS 1.2 ECDHE | ATAK stream fails if TLS 1.3 unavailable on the device's OpenSSL | commoncommo uses OpenSSL 3 (`EVP_PKEY_CTX_new_from_name`, legacy provider) → TLS 1.3 available; always enable TLS 1.3 on 8089; CI `openssl s_client -cipher 'DEFAULT:!ECDH'` check; document that a TLS-terminating proxy in front of 8089 must also offer TLS 1.3. Escape hatch (not planned): an `openssl`-backed stream acceptor feature flag. |
| PKCS#12 legacy algorithms (RC2-40/3DES/SHA-1) rejected or unsupported | Manual packages fail on some clients | Use `p12-keystore` PBES1 3DES + HmacSha1 (what ATAK/iTAK/WinTAK import; iTAK's `SecPKCS12Import` notably rejects PBES2/AES); `p12_legacy=false` toggle for modern-only consumers; `#[ignore]` openssl interop test. |
| `instant-acme` API churn (0.8 → 0.9) | Build breaks on upgrade | Isolate all instant-acme usage in `acme/{account,order}.rs` (< 250 lines total); pin minor version in `[workspace.dependencies]`; Dependabot PRs run the unit tests. |
| Let's Encrypt rate limits during development | Lockout for a week | `letsencrypt-staging` default in the wizard's "test" step and in e2e; Pebble locally; never order on each restart (persisted cert reused). |
| JWT clock skew (CloudTAK reads `nbf`/`exp` but doesn't verify; ATAK never sees it; our verify does) | Spurious 401s | `nbf = iat - 60s`, leeway 60 s; server NTP note in deployment docs. |
| Chunked/oversized `access_token` cookie on `/login/redirect` | WebTAK-style flow breaks | Keep our JWT small (no groups embedded, ~400 B); still implement 4000-byte chunking like TAK for safety. |
| Argon2 CPU cost under polling clients / CPU DoS via Basic spam | Latency, DoS | `VerifiedSecretCache`, rate limiter before hashing, dummy-hash only once per request. |
| Revocation cache vs multi-process | Stale acceptance | Single process by design; cache updated in-line; Marti re-checks DB per request; startup reload. |
| `require_known_cert` + restored older DB | Valid devices locked out | Admin UI shows "unknown certificate rejected" audit entries with fingerprint/CN and a one-click "trust this certificate" that inserts the row (issued_via `recovered`). |
| Username charset restrictions vs IdP usernames containing spaces or `}` | Provisioning refused | `username_claim` configurable (use `email`); clear error in audit and UI; normalisation documented. |
| `rcgen` cannot generate RSA keys without aws-lc-rs; cross builds of aws-lc-rs need cmake/clang | CI breakage on musl/aarch64 | RSA keygen via pure-Rust `rsa` crate; rustls/rcgen stay on the default aws-lc-rs provider (pre-generated bindings exist for the CI targets); fallback plan documented: `ring` features + ECDSA-only CA. |
| CloudTAK lower-cases the email but the IdP `preferred_username` might be mixed case | `sub` ≠ typed username → CloudTAK confusion | We normalise usernames to lowercase everywhere (JIT provisioning included) and compare case-insensitively (TAK behaviour). |

### Critical Files for Implementation
- `/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/pki/issue.rs` (CSR → client cert: subject replacement, EKU/KU, serial, AKI/SKI, validity — the compatibility heart)
- `/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/pki/tls/client_verifier.rs` (rustls `ClientCertVerifier` with revocation/known-cert cache; failures must occur at handshake)
- `/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/auth/resolve.rs` (cert → bearer → basic principal resolution, per-listener policy, 401/403 shapes CloudTAK depends on)
- `/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/auth/oauth_server/jwt.rs` (fixed 27-byte header, flat claims, RS256-only verify, `token_key`/JWKS)
- `/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/marti/tls.rs` (`tls/config` XML, `signClient/v2` Accept dispatch with 64-col bare base64, legacy p12)

Reference sources to lift from: `/Users/bpannell/dev/gh/SierraSoftworks/automate/agent/src/web/helpers/oidc.rs` (split into `auth/oidc/*`), `/Users/bpannell/dev/gh/SierraSoftworks/automate/agent/src/integrations/state.rs` (→ `oauth_server/state.rs`), `/Users/bpannell/dev/gh/SierraSoftworks/automate/agent/src/testing/oidc.rs` (→ `testing/oidc.rs` + code-flow mocks), `/Users/bpannell/dev/gh/SierraSoftworks/automate/ui/src/auth.rs` (→ `rustak-ui/src/auth.rs` + PKCE).
