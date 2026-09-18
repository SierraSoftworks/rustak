# M1-06 — `rustak-client::stream`: TAK stream client — complete

Brief: `.claude/plan/briefs/M1-06-client-stream.md`
Read first: `.claude/plan/conventions.md`; `compat/streaming.md` §1–§7;
`design/02-protocol-streaming.md` §3–§4; `status/M1-01-cot-model-xml.md`,
`status/M1-02-cot-protobuf.md`, `status/M1-03-cot-codec-negotiation.md` (the exact `rustak_cot`
API, including `TakCodec`'s drop counters); `status/M0-15-client-sidecar-example.md` (the existing
`rustak-client` layout); `status/M0-04-rustak-core.md`.

## What was built

Everything under `rustak-client/src/stream/`, plus `pub mod stream;` in `lib.rs`. **No file under
`rustak-client/src/sidecar/` was touched** — `SidecarEvent::Cot` still carries a
`Box<rustak_cot::proto::TakMessage>`, and rewiring the harness onto this client is a later brief's
job (see "For the orchestrator").

| File | Functional lines (limit 300) | Unit tests | Contents |
|---|---:|---:|---|
| `stream/mod.rs` | 105 | 4 | `StreamConfig` + builders, `connect()`, the transport split (TLS / plaintext behind a feature), re-exports |
| `stream/connect_string.rs` | 134 | 9 | `Endpoint` — ATAK `host:port:ssl` and CloudTAK `ssl://host:port`, IPv6, `Display`, `connect_string()` |
| `stream/tls.rs` | 125 | 5 | `TlsIdentity` (PEM truststore + chain + key, redacting `Debug`), `provider()`, `client_config()`, `connect()`, `dial()` |
| `stream/negotiation.rs` | 76 | 10 | `Negotiation` (public state) and `ClientNeg` — the pure client-side `t-x-takp-*` transition table |
| `stream/keepalive.rs` | 83 | 6 | `Keepalive` (`ATAK`, `OFF`) and `KeepaliveState`/`Tick` — ping at 15 s, repeat 4.5 s, dead at 25 s |
| `stream/connection.rs` | 275 | — (12 in `tests/stream_client.rs`) | `AsyncIo`, `TakStream`: `Stream<Item = Result<Event, StreamError>>` + `Sink<Event>`, negotiation, keepalive, `settle()` |
| `stream/reconnect.rs` | 194 | 5 | `Reconnecting`, `ConnectHook`, `MIN_BACKOFF`/`MAX_BACKOFF` |
| `stream/testing.rs` (feature `testing`) | 124 | 6 | `Eud` — `sa`, `send`, `send_sa`, `expect`, `expect_uid`, `expect_none` |
| `stream/error.rs` | 93 | 4 | `StreamError`, the three advice slices, `From<StreamError> for human_errors::Error` |
| `tests/stream_client.rs` | (exempt) | 12 | Duplex pairing: the whole negotiation exchange, held writes, refusal, 60 s expiry, ping/pong timing, `pass_control`, parse drops, EOF, `settle` |
| `tests/stream_reconnect.rs` (feature `insecure-tcp`) | (exempt) | 2 | A real listener that hangs up twice: reconnect + the on-connect hook; a port nothing listens on |
| `tests/stream_tls.rs` | (exempt) | 2 | A throwaway rcgen CA: mutual-TLS handshake carrying CoT both ways, and an untrusted CA being refused |

71 lib unit tests (49 of them this brief's, 22 pre-existing), 16 integration tests, 6 doctests.

`rustak-client/Cargo.toml` gained `[features] default = []`, `testing = ["insecure-tcp"]`,
`insecure-tcp = []`, and the dev-dependencies `tokio` (+`test-util`, for `tokio::time::pause`),
`rcgen` and `rustls` (the TLS test's throwaway CA and listener). **No new runtime dependency and no
edit to the workspace `Cargo.toml`** — see "Deviations" for `rustls-pemfile` and `tokio-stream`.

## `TakStream` / `Eud` API — for the server-side integration-test brief (M1-05)

```rust
// rustak_client::stream
pub struct Endpoint { pub host: String, pub port: u16, pub tls: bool }
impl Endpoint {
    const DEFAULT_PORT: u16 = 8089;
    fn tls(host: impl Into<String>, port: u16) -> Self;      // the only kind a default build dials
    fn new(host: impl Into<String>, port: u16, tls: bool) -> Self;
    fn authority(&self) -> String;       // "host:port", IPv6 bracketed
    fn scheme(&self) -> &'static str;    // "ssl" | "tcp"
    fn connect_string(&self) -> String;  // "host:port:ssl" (ATAK)
}
impl FromStr for Endpoint { type Err = StreamError; }   // "h:p:ssl", "ssl://h:p", "tls://", "tcp://"*
impl Display for Endpoint;                              // "ssl://host:port" (CloudTAK)

pub struct TlsIdentity;                                  // Debug redacts the key
impl TlsIdentity {
    fn from_pem_files(truststore, certificate, key) -> Result<Self, StreamError>;
    fn from_identity(&rustak_core::service::ServiceIdentity) -> Result<Self, StreamError>;
    fn roots(&self) -> usize;
    fn client_config(&self) -> Result<Arc<rustls::ClientConfig>, StreamError>;
}
pub fn tls::provider() -> Arc<rustls::crypto::CryptoProvider>;   // see "Findings"
pub async fn tls::connect(&Endpoint, Arc<ClientConfig>) -> Result<TlsStream<TcpStream>, StreamError>;

pub struct Keepalive { pub idle: Duration, pub repeat: Duration, pub dead: Duration }
impl Keepalive { const ATAK: Self /* 15 s, 4.5 s, 25 s */; const OFF: Self; fn is_off(&self) -> bool }

pub struct StreamConfig {
    pub endpoint: Endpoint, pub tls: Option<TlsIdentity>, pub uid: String,
    pub callsign: Option<String>, pub negotiate: bool, pub keepalive: Keepalive,
    pub connect_timeout: Duration, pub pass_control: bool,
}
impl StreamConfig {
    const DEFAULT_CONNECT_TIMEOUT: Duration = 20s;
    fn new(endpoint: Endpoint, uid: impl Into<String>) -> Self;   // negotiate on, ATAK keepalive
    fn with_tls / with_callsign / with_negotiation(bool) / with_keepalive
      / with_connect_timeout / with_pass_control(bool) -> Self;
}

pub async fn connect(&StreamConfig) -> Result<TakStream, StreamError>;

pub trait AsyncIo: AsyncRead + AsyncWrite + Send + Unpin {}      // blanket impl
pub struct TakStream;
impl TakStream {
    fn new(io: Box<dyn AsyncIo>, config: &StreamConfig) -> Self;
    fn over(io: impl AsyncIo + 'static, uid: impl Into<String>) -> Self;   // defaults; duplex tests
    fn uid(&self) -> &str;
    fn mode(&self) -> Mode;                       // rustak_cot::codec::Mode, re-exported here
    fn negotiation(&self) -> Negotiation;
    fn server_version(&self) -> Option<&str>;     // from the offer's TakServerVersionInfo
    fn set_pass_control(&mut self, bool);
    fn dropped(&self) -> u64;                     // unparseable + unframeable inbound messages
    fn skipped(&self) -> u64;                     // bytes skipped resynchronising protobuf
    fn queued(&self) -> usize;                    // outbound events not yet on the wire
    async fn settle(&mut self, budget: Duration) -> Result<(), StreamError>;
}
impl Stream for TakStream { type Item = Result<Event, StreamError>; }
impl Sink<Event> for TakStream { type Error = StreamError; }

pub enum Negotiation { Waiting, Requested, Proto, Xml }   // is_quiet(), is_settled()

pub type ConnectHook = Arc<dyn Fn(TakStream) -> BoxFuture<'static, Result<TakStream, StreamError>>
                          + Send + Sync>;
pub struct Reconnecting;                                   // Stream<Item = Event> + Sink<Event>
impl Reconnecting {
    fn new(StreamConfig) -> Self;  fn with_hook(self, ConnectHook) -> Self;
    fn stream(&self) -> Option<&TakStream>;  fn is_connected(&self) -> bool;
    fn attempts(&self) -> u64;  fn backoff(&self) -> Duration;
}
pub const MIN_BACKOFF: Duration = 1s;   pub const MAX_BACKOFF: Duration = 30s;

pub enum StreamError {   // non_exhaustive; Display in rustak's voice; Into<human_errors::Error>
    Endpoint(String), Identity(String), Io(std::io::Error), Codec(rustak_cot::error::CodecError),
    RxTimeout, Timeout(String), Unexpected(String),
}
```

```rust
// rustak_client::stream::testing  (feature `testing`)
pub const SA_VALIDITY: Duration = 120s;   pub const SA_TYPE: &str = "a-f-G-U-C";
pub const SETTLE_BUDGET: Duration = 250ms;

pub struct Eud;
impl Eud {
    async fn connect(&StreamConfig, callsign: impl Into<String>) -> Result<Self, StreamError>;
    fn new(TakStream, callsign) -> Self;
    fn over(io: impl AsyncIo + 'static, uid, callsign) -> Self;
    fn with_team(self, name, role) -> Self;                  // default "Cyan" / "Team Member"
    fn uid(&self) -> &str;  fn callsign(&self) -> &str;
    fn stream(&self) -> &TakStream;  fn stream_mut(&mut self) -> &mut TakStream;
    fn sa(&self, lat: f64, lon: f64) -> Event;               // uid + contact(endpoint) + __group + takv
    async fn send(&mut self, Event) -> Result<(), StreamError>;        // sends, then settles
    async fn send_sa(&mut self, lat, lon) -> Result<(), StreamError>;
    async fn expect(&mut self, impl FnMut(&Event) -> bool, Duration) -> Result<Event, StreamError>;
    async fn expect_uid(&mut self, uid: &str, Duration) -> Result<Event, StreamError>;
    async fn expect_none(&mut self, Duration) -> Result<(), StreamError>;
}
```

`rustak-server` takes it as `rustak-client = { workspace = true, features = ["testing"] }` in
`[dev-dependencies]`; `testing` implies `insecure-tcp`, so a test harness may also point an `Eud` at
a plain-TCP listener if one is ever wanted.

## Behaviour M1-05 must not re-litigate

* **The client acts only when it is polled.** The negotiation request, the mode switch, the
  keepalive ping and the death clock all live in `poll_next`. A sidecar's
  `while let Some(event) = stream.next().await` supplies that for free; a test that sends and then
  asserts on *another* EUD must not leave the sender unpolled. `Eud::send` calls
  `TakStream::settle(SETTLE_BUDGET)` for exactly this reason, and `settle` costs nothing when
  `queued() == 0` (the normal case).
* **An event written between `t-x-takp-q` and `t-x-takp-r` is held, not sent** — the encoding the
  server will read the next byte in is undecided, and ATAK sends nothing in that window either.
  `queued()` is how many are waiting; they are released, in the settled encoding, when the response
  arrives or the offer's 60 s run out.
* **One unreadable message never ends the connection.** `TakCodec` already consumes framing damage
  (M1-03); this client does the same for parse failures, counting them in `dropped()`. The only
  `Err` items a caller sees are `RxTimeout` and transport failures, and the stream ends (`None`)
  after any of them.
* **Control traffic is consumed by default.** `t-x-takp-v/q/r` and `t-x-c-t-r` are acted on and not
  delivered; `pass_control` delivers them *as well*, which is what a conformance test wants. The
  client does **not** answer a server-sent `t-x-c-t` — servers do not ping, and inventing a reply
  would be a behaviour no reference client has.
* **Keepalive constants are ATAK's** (`compat/streaming.md` §6): ping after 15 s of inbound silence
  (`t-x-c-t`, `how="m-g"`, stale +10 s, uid `{client-uid}-ping`), repeat every 4.5 s, `RxTimeout`
  at 25 s. "Silence" means nothing *received*, including a message we could not parse. Pings are
  muted while a negotiation request is outstanding, but the 25 s clock keeps running — a server
  that answers nothing at all is a connection to replace, which is also what ATAK does.
* **The negotiation timeout is 60 s** (`negotiate::VALIDITY`) from the request, and its outcome is
  XML forever, not a disconnect. A server that keeps the connection busy but never answers
  therefore ends up on XML; one that goes silent hits the keepalive first.
* **`Reconnecting` never ends.** Errors are logged and retried with 1 s → 30 s doubling backoff,
  reset on every successful connect; the hook runs after each connect, before any event is
  delivered. A caller stops it by dropping it or racing it against `Shutdown::cancelled`.
* **`Endpoint` refuses `tcp://` at parse time** in a build without `insecure-tcp`, and `connect`
  refuses a plaintext endpoint built by hand. TLS requires a truststore, a certificate and a key —
  there is no platform-roots fallback and no anonymous path.

## Findings worth carrying elsewhere

1. **`rustls::ClientConfig::builder()` panics in this workspace.** rustak's dependency graph enables
   *both* `aws-lc-rs` and `ring` (transitively — `rustls-platform-verifier` and friends), so
   rustls cannot pick a process-level `CryptoProvider` from crate features and panics at the first
   connection rather than failing to compile. It was found by the TLS handshake test, which is the
   only thing here that builds a real `ClientConfig`. `tls::provider()` fixes it for this crate:
   prefer whatever the process installed, else `aws-lc-rs`. **`rustak-server`'s listener code is
   almost certainly exposed to the same panic** — anything calling `ServerConfig::builder()`,
   `WebPkiClientVerifier::builder()` or `ClientConfig::builder()` without naming a provider should
   be checked, or the binary should call `CryptoProvider::install_default` once at start-up.
2. **A `Stream` + `Sink` pair over one socket has a "who polls it" hazard.** It cost two rounds of
   deadlocked tests here; `TakStream::settle` and the `Eud::send` call to it are the mitigation, and
   the two are worth knowing about before writing `rustak-server/tests/stream_*.rs`.
3. `rustls-pki-types` can read PEM by itself (`PemObject::{from_pem_file, pem_file_iter}`), so
   `rustls-pemfile` is not needed anywhere in rustak.

## Deviations from `design/02` §3, and why

1. **No `auth.rs` / `Credentials`.** The plan delta removed the TCP `<auth>` path
   (`compat/streaming.md` §1) and M1-03 dropped `codec/auth.rs` with it. `StreamConfig` has no
   `credentials` field.
2. **Two extra files**: `negotiation.rs` (the state machine, split out so it is testable without a
   socket and so `connection.rs` stays inside the 300-line rule — it is at 275) and `error.rs`
   (`StreamError` plus its advice, which `connect_string.rs` and `tls.rs` both need).
3. **The on-connect hook takes the stream and gives it back** (`Fn(TakStream) -> BoxFuture<'static,
   Result<TakStream, StreamError>>`) rather than the design's `FnMut(&mut TakStream) ->
   BoxFuture<()>`. A borrowing hook cannot produce a `'static` future to hold across a reconnect
   without self-referential gymnastics; ownership-passing is the same thing without them, and it
   lets the hook report a failure as a failed connect (retried) rather than swallowing it.
4. **No `rustls-pemfile` and no `tokio-stream` dependency.** The brief lists both; the first is
   unnecessary (finding 3) and the second unused (`futures::StreamExt` covers everything here).
   Adding either would have meant editing the workspace `Cargo.toml`, which this brief may not
   touch. `rustls` and `rcgen` were added as *dev*-dependencies for the TLS test.
5. **The truststore is required** rather than defaulting to the platform roots: trusting every
   public CA to impersonate a TAK server is worse than refusing to start, no crate in the workspace
   supplies platform roots, and enrolment hands the device its CA anyway (M2).
6. **Extra API beyond the design**, all of it used by the tests or by M1-05: `TakStream::{over,
   settle, queued, dropped, skipped, uid, set_pass_control}`, `Negotiation::{is_quiet, is_settled}`,
   `Keepalive::{ATAK, OFF, is_off}`, `Endpoint::{authority, scheme, connect_string, DEFAULT_PORT}`,
   `TlsIdentity::{from_identity, roots}`, `tls::provider`, `Reconnecting::{stream, is_connected,
   attempts, backoff}`, `Eud::{over, with_team, send_sa, expect_uid, stream, stream_mut}`.
7. **`TakStream::queued`, not `buffered`** — `futures::StreamExt::buffered` shadows an inherent
   `buffered()` at every call site that has `StreamExt` in scope, which is all of them.

## Exit checks

```
$ cargo test -p rustak-client --all-features
     Running unittests src/lib.rs
running 71 tests
test result: ok. 71 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
     Running tests/stream_client.rs
running 12 tests
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s
     Running tests/stream_reconnect.rs
running 2 tests
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.01s
     Running tests/stream_tls.rs
running 2 tests
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
   Doc-tests rustak_client
running 6 tests
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

$ cargo clippy -p rustak-client --all-targets --all-features -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.19s

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-client --no-deps
 Documenting rustak-client v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-client)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.64s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_client/index.html

$ cargo fmt -p rustak-client --check
(no output: clean)

$ ./scripts/check-file-length.sh
(no output: clean)

$ cargo test -p rustak-client          # default features: no `testing`, no `insecure-tcp`
running 71 tests ... ok; stream_client 10 ok; stream_reconnect 0 (feature-gated); stream_tls 2 ok
```

`check-file-length.sh` walks `git ls-files` and these sources are still untracked, so the same `awk`
was run over `rustak-client/src/stream/*.rs` directly: `connection.rs` 275, `reconnect.rs` 194,
`connect_string.rs` 134, `tls.rs` 125, `testing.rs` 124, `mod.rs` 105, `error.rs` 93,
`keepalive.rs` 83, `negotiation.rs` 76 — all inside the limit, each with its single trailing
column-0 `#[cfg(test)] mod tests` except `connection.rs`, whose tests are the duplex ones in
`tests/stream_client.rs`. **`connection.rs` at 275 is the one file here with little headroom**; the
natural split if it needs one is the `Sink` impl into `stream/sink.rs`.

`cargo clippy --workspace` and `cargo fmt --all --check` were not run: `rustak-server/**` is another
session's in-flight work and does not currently build.

## For the orchestrator

* **The sidecar harness is not yet wired to this client.** `SidecarEvent::Cot` still carries
  `Box<rustak_cot::proto::TakMessage>`; the M1 shape is `Box<rustak_cot::Event>` plus a
  `Reconnecting` owned by `sidecar::run`, with `SidecarEvent::{Connected, Disconnected}` driven from
  the reconnect hook and the drop path. That is an additive change (`SidecarEvent` is
  `#[non_exhaustive]`) and belongs in whichever brief also gives the harness its `[server] stream`
  connect logic.
* **Finding 1 (the rustls provider panic) is a live risk in `rustak-server`.** It is a runtime
  panic, not a compile error, and only a real handshake finds it.
* `docs/plugins.md` still says the stream client "arrives in M1" as a future tense; it is here now,
  and the guide could use a short "connecting to the stream" section. Out of scope for this brief.
