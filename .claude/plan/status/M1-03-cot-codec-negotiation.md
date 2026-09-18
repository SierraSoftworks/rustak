# M1-03 — `rustak-cot`: frame codecs, negotiation and control messages — complete

Brief: `.claude/plan/briefs/M1-03-cot-codec-negotiation.md`
Read first: `.claude/plan/conventions.md`; `status/M1-01-cot-model-xml.md` (public API);
`compat/streaming.md`; `plan.md` Appendix A.1; `design/02-protocol-streaming.md` §0, §1.2, §4;
`research/05-takserver-streaming-auth-verified.md` §3–§5 (verified templates — facts only, no text
copied).

## What was built

| File | Functional lines | Unit tests | Contents |
|---|---:|---:|---|
| `src/codec/mod.rs` | 163 | 9 | `MAX_MESSAGE`, `MAX_PROTO_PAYLOAD`, `Mode`, `Frame`, `TakCodec` + `Decoder`/`Encoder<Frame>`/`Encoder<&EncodedEvent>`, `encode_proto_frame`, re-exports |
| `src/codec/varint.rs` | 31 | 8 | `MAX_LEN`, `put_varint`, `read_varint` |
| `src/codec/xml_frame.rs` | 73 | 14 | `XmlScanner` — `<event`-aligned, `</event>`-terminated incremental framer |
| `src/codec/proto_frame.rs` | 62 | 12 | `MAGIC`, `ProtoScanner` (skip/resync + `skipped()`), `encode` |
| `src/codec/encoded.rs` | 52 | 6 | `EncodedEvent` — lazy XML/proto bytes behind `OnceLock` |
| `src/negotiate.rs` | 88 | 10 | `PROTO_VERSION`, `API_VERSION`, `VALIDITY`, `POINT`, `Announce`, `announce`/`parse_announce`/`request`/`parse_request`/`response`/`parse_response` |
| `src/msgs.rs` | 78 | 12 | `PONG_UID`, `PING_SUFFIX`, `PING_VALIDITY`, `NOTICE_VALIDITY`, `ping`, `pong`, `is_ping`, `is_pong`, `disconnect`, `group_change`, `group_change_uid`, `incognito_toggle` |
| `tests/codec_framed.rs` | (exempt) | 9 | `Framed` over `tokio::io::duplex`: full negotiation + mode switch, CloudTAK-style non-negotiation, refusal, one-byte reads, proto resync, oversize, EOF mid-message |

`rustak-cot/Cargo.toml` gained `memchr` and `tokio-util` (deps) and `tokio` + `futures` (dev only).
`codec/auth.rs` was **not** written — the plan removed the TCP `<auth>` path.

## Public API for M1-04 (server) and M1-05 (client)

```rust
// rustak_cot::codec
pub const MAX_MESSAGE: usize = 8 * 1024 * 1024;   // inbound cap, both modes
pub const MAX_PROTO_PAYLOAD: usize = 65_536;      // outbound b-f-t-r substitution threshold
pub const MAGIC: u8 = 0xBF;
pub const MAX_VARINT_LEN: usize = 10;

pub enum Mode { Xml /* default */, Proto }
pub enum Frame { Xml(Bytes), Proto(Bytes) }       // Proto carries the payload, no magic/length
impl Frame { fn mode(&self) -> Mode; fn payload(&self) -> &Bytes; fn len(&self) -> usize;
             fn is_empty(&self) -> bool; fn into_payload(self) -> Bytes }

pub struct TakCodec;                               // Clone + Debug + Default (= Xml, MAX_MESSAGE)
impl TakCodec {
    const fn new(mode: Mode) -> Self;
    const fn with_limit(mode: Mode, max_frame: usize) -> Self;
    const fn mode(&self) -> Mode;      const fn set_mode(&mut self, Mode);
    const fn max_frame(&self) -> usize;
    const fn skipped(&self) -> u64;    // bytes discarded resyncing the protobuf stream
    const fn dropped(&self) -> u64;    // messages discarded for exceeding max_frame
    const fn dropped_bytes(&self) -> u64;
}
impl Decoder for TakCodec { type Item = Frame; type Error = CodecError; }
impl Encoder<Frame> for TakCodec;                  // writes the frame in ITS encoding (relay path)
impl<'a> Encoder<&'a EncodedEvent> for TakCodec;   // writes in the CODEC's mode (fan-out path)

pub struct EncodedEvent;                           // Debug, From<Event>, Send + Sync
impl EncodedEvent { const fn new(Event) -> Self; const fn event(&self) -> &Event;
                    fn xml(&self) -> &Bytes; fn proto(&self) -> &Bytes;
                    fn frame(&self, Mode) -> Frame; fn len(&self, Mode) -> usize }

pub struct XmlScanner;   impl { const fn new(); fn split(&mut self, &mut BytesMut, max) -> Result<Option<Bytes>, FrameError> }
pub struct ProtoScanner; impl { const fn new(); const fn skipped(&self) -> u64; fn split(..) -> Result<Option<Bytes>, FrameError> }
pub fn put_varint(u64, &mut BytesMut);
pub fn read_varint(&[u8]) -> Result<Option<(u64, usize)>, FrameError>;
pub fn encode_proto_frame(payload: &[u8], dst: &mut BytesMut);
```

```rust
// rustak_cot::negotiate
pub const PROTO_VERSION: u32 = 1;  pub const API_VERSION: u32 = 3;
pub const VALIDITY: Duration = 60s;
pub const POINT: Point = Point { lat: 0.0, lon: 0.0, hae: 0.0, ce: 999_999.0, le: 999_999.0 };
pub struct Announce { pub versions: Vec<u32>, pub server_version: Option<String>, pub api_version: Option<u32> }
impl Announce { fn supports(&self, u32) -> bool }
pub fn announce(uid, server_version, api_version: u32, now: CotTime) -> Event;   // t-x-takp-v
pub fn parse_announce(&Event) -> Option<Announce>;
pub fn request(uid, version: u32, now) -> Event;                                // t-x-takp-q
pub fn parse_request(&Event) -> Option<u32>;                                    // None ⇒ SAY NOTHING
pub fn response(uid, accepted: bool, now) -> Event;                             // t-x-takp-r
pub fn parse_response(&Event) -> Option<bool>;
```

```rust
// rustak_cot::msgs
pub const PONG_UID: &str = "takPong";       pub const PING_SUFFIX: &str = "-ping";
pub const PING_VALIDITY: Duration = 10s;    pub const NOTICE_VALIDITY: Duration = 20s;
pub fn ping(device_uid: &str, now) -> Event;                    // t-x-c-t,   how m-g,       +10 s
pub fn pong(now) -> Event;                                      // t-x-c-t-r, how h-g-i-g-o, +20 s, no detail
pub fn is_ping(&Event) -> bool;   pub fn is_pong(&Event) -> bool;   // case-SENSITIVE, see below
pub fn disconnect(uid, client_uid: &str, last_sa_type: &str, now) -> Event;   // t-x-d-d
pub fn group_change(uid, now) -> Event;                                       // t-x-g-c
pub fn group_change_uid(fresh: &str, client_uid: Option<&str>) -> String;     // "{fresh}[.{clientUid}]"
pub fn incognito_toggle(&Event) -> Option<bool>;                              // t-x-c-i-e / -d
```

All message builders take `now: CotTime` and (where the template has one) a caller-supplied `uid`:
`rustak-cot` has no clock-free randomness and no `uuid` dependency, so the fresh UUIDs the
disconnect and group-change templates call for are the caller's to mint. This also keeps every test
deterministic.

## Behaviour M1-04 / M1-05 must not re-litigate

* **`decode` never reports a framing failure.** `Framed` ends its stream permanently on the first
  `Err` a `Decoder` returns (verified here: the integration test that expected otherwise failed),
  and the wire contract says one unframeable message must never cost the connection. `TakCodec`
  therefore consumes the damage and returns the *next* good message; `dropped()`,
  `dropped_bytes()` and `skipped()` carry the news to `stream/metrics.rs` (`dropped_parse`,
  `proto_resyncs`). The only errors `decode` can yield come from the transport. `XmlScanner` and
  `ProtoScanner` still return `FrameError` so the behaviour stays unit-testable.
* **`decode_eof` discards a half-written message** instead of the default "bytes remaining on
  stream" error; a peer closing mid-message is ordinary.
* **XML framing**: aligns on `<event` followed by one of `` ``/`>`/`\t`/`\n`/`\r`; everything before
  it is discarded (declarations, `\r\n`, a stray `<auth>`, the `<events>` wrapper). A partial start
  token at the tail is kept, never thrown away. `</event>` is searched from `scanned - 7` so a
  token split across reads is found. Oversize consumes then reports.
* **Protobuf framing resynchronises, TAK Server does not.** On a bad magic byte, an unreadable
  varint or a length past the cap, the scanner steps one byte past the magic and rescans, counting
  what it skipped. A `0xBF` inside a payload is never a boundary because a frame is consumed whole.
  Zero-length payloads are legal frames.
* **Two different "unknown" sentinels, both deliberate**: negotiation uses `ce`/`le` `999999`
  (`negotiate::POINT`), pings/pongs/notices use `9999999` (`Point::zero()`). Matching the templates
  byte for byte is the point; see the golden comparison below.
* **Ping is `how="m-g"`, stale +10 s.** `design/02` §1.2's `msgs.rs` row says `h-g-i-g-o`/+20 s for
  the ping, which is the *pong's* envelope pasted one row up; `compat/streaming.md` §6, `plan.md`
  A.1 and `research/07` §3.5 agree on `m-g`/+10 s, and `fixtures/ping.xml` already encodes it.
  `compat/streaming.md` §6's ping template also shows `ce="999999"`, which contradicts research 07
  ("ce/le = no value") and its own §6 pong; the fixture's `9999999` wins. Worth correcting in
  `compat/streaming.md` and `design/02`.
* **`is_ping`/`is_pong`/`incognito_toggle` match case-sensitively.** TAK Server lowercases only for
  the *control-set* lookup and then switches on the original case, so `T-X-C-T` is consumed but
  never answered. Classifying a type as control remains `types::is_control_type` (case-insensitive);
  deciding to *act* on it is these functions.
* **`Encoder<Frame>` writes the frame's own encoding, `Encoder<&EncodedEvent>` writes the codec's
  mode.** The first is the relay path (bytes already chosen), the second the fan-out path.
* **`EncodedEvent::proto()` is the `TakMessage` payload only** — the magic byte and length varint
  are added when the frame is written, so `len(Mode::Proto)` is the number to compare against
  `MAX_PROTO_PAYLOAD` for the `b-f-t-r` substitution.
* `set_mode` must be called **after** the `t-x-takp-r` response is written; the response is the last
  XML on that socket.

## Templates asserted byte for byte

`negotiate` and `msgs` are compared against the exact strings `tests/golden/{takp-v,takp-q,takp-r,
ping,pong,disconnect}.xml` hold, using the fixtures' own instants (`2026-09-17T12:00:00.000Z` and
friends). `msgs::disconnect` builds its `<link>` attribute by attribute rather than through
`Link::peer` so the order matches the verified template (`relation`, `uid`, `type`); a test asserts
`links()` still reads it back as `Link::peer(..)`.

## Exit checks

```
$ cargo test -p rustak-cot
     Running unittests src/lib.rs
running 249 tests
test result: ok. 249 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
     Running tests/codec_framed.rs
running 9 tests
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
     Running tests/golden.rs
running 49 tests
test result: ok. 49 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
     Running tests/roundtrip_prop.rs
running 9 tests
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.26s
   Doc-tests rustak_cot
running 3 tests
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

$ cargo test -p rustak-cot --test codec_framed
running 9 tests
test a_relayed_frame_keeps_its_bytes_exactly ... ok
test a_peer_that_closes_mid_message_is_not_a_codec_failure ... ok
test a_protobuf_stream_resynchronises_after_corruption ... ok
test the_decoder_is_usable_without_a_runtime ... ok
test an_oversized_message_is_dropped_without_ending_the_stream ... ok
test a_refused_request_leaves_both_ends_on_xml ... ok
test a_client_that_never_asks_stays_on_xml_forever ... ok
test a_connection_negotiates_then_switches_both_directions_to_protobuf ... ok
test messages_are_reassembled_however_the_transport_splits_them ... ok
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s

$ cargo clippy -p rustak-cot --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.72s

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-cot --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.85s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_cot/index.html

$ cargo fmt -p rustak-cot --check
(no output: clean)

$ ./scripts/check-file-length.sh
(no output: clean)

$ env -i PATH=/usr/bin:/bin:$HOME/.cargo/bin HOME=$HOME cargo build -p rustak-cot
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 16.68s   # protoc absent from PATH
```

`check-file-length.sh` walks `git ls-files`, and these files are still untracked, so the same `awk`
was run over them directly. Largest: `codec/mod.rs` 163, `negotiate.rs` 88, `msgs.rs` 78,
`codec/xml_frame.rs` 73 — all well inside the 300-line rule, each with exactly one trailing
column-0 `#[cfg(test)] mod tests`.

`cargo fmt --all --check` (workspace-wide) currently fails in
`rustak-server/src/pki/tls/{resolver.rs, …}`, which a concurrent session owns; nothing under
`rustak-cot/` is involved and `cargo fmt -p rustak-cot --check` is clean.

## Coordination with M1-02 (`proto/`)

M1-02 landed `src/proto/{mod,to_proto,from_proto}.rs` while this brief was in flight, so
`EncodedEvent::proto()` calls the real
`crate::proto::encode(&crate::proto::event_to_message(&event))` — **no placeholder was needed and
none is left behind**. `tests/codec_framed.rs` also exercises `proto::decode` +
`proto::message_to_event` on the receiving side of the mode switch. The only shared file touched by
both briefs is `rustak-cot/Cargo.toml`; the edits are disjoint (M1-02 added the `bytes` `serde`
feature and the `proptest` dev-dep, this brief added `memchr`, `tokio-util`, and the `tokio` and
`futures` dev-deps).

## Deviations from `design/02` §1.2, and why

1. **`decode` swallows framing failures** rather than returning `CodecError::Frame` — see
   "Behaviour" above. `CodecError` is still the `Decoder::Error` (the transport and the
   frame→`Event` step both need it), and `FrameError` is still what the two scanners return.
2. **`TakCodec::new(mode)` + `with_limit(mode, max_frame)`** instead of the design's
   `new(mode, max_frame)`, so the documented `MAX_MESSAGE` is the default rather than something
   every call site restates. `design/02` §2.1's `TakCodec::new(Xml, max_frame)` becomes
   `TakCodec::with_limit(Mode::Xml, max_frame)`.
3. **One cap, not two.** `design/02` §0 says "inbound cap 8 MiB for both", so `max_frame` applies to
   both framers; `MAX_PROTO_PAYLOAD` (64 KiB) is exported as the *outbound* substitution threshold,
   which is all ATAK's 64 KiB buffer actually constrains.
4. **`codec/auth.rs` omitted** — the plan removed the TCP `<auth>` path (`compat/streaming.md` §1).
   `auth_tcp.rs` in `design/02` §2.1 goes with it.
5. **`lib.rs` re-exports not added.** `design/02` §1.2 lists `codec::{TakCodec, Frame, Mode,
   EncodedEvent}` among the root re-exports, but this brief may not edit `lib.rs` (it declares
   `pub mod codec; pub mod msgs; pub mod negotiate;` already). They are reachable as
   `rustak_cot::codec::*`; add the re-exports in a follow-up if the orchestrator wants them.
6. **Extra API beyond the design**, all small and all used by the design's own server modules:
   `Frame::{mode, payload, len, is_empty, into_payload}`, `TakCodec::{max_frame, skipped, dropped,
   dropped_bytes}`, `ProtoScanner::skipped`, `encode_proto_frame`, `negotiate::{API_VERSION,
   VALIDITY, POINT, Announce::supports}`, `msgs::{PONG_UID, PING_SUFFIX, PING_VALIDITY,
   NOTICE_VALIDITY, group_change_uid}`, `MAX_VARINT_LEN`.
7. **`tokio-util` is taken with the workspace's feature set** (`codec` + `rt`) rather than
   `codec` alone, because `conventions.md` requires `workspace = true` and workspace features are
   additive. `rustak-cot` still performs no I/O and holds no runtime handle.

## Not done here

`rustak-cot/fuzz/` (design §5 step 8: `xml_frame_feed`, `proto_frame_feed` targets — the scanners
are the obvious fuzz surface and the unit tests only cover hand-picked splits), and everything under
`rustak-server/src/stream/` and `rustak-client/src/stream/`.
