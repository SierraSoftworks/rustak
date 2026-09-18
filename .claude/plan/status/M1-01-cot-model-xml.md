# M1-01 — `rustak-cot`: event model, XML parse/write, typed details — complete

Brief: `.claude/plan/briefs/M1-01-cot-model-xml.md`
Read first: `.claude/plan/conventions.md`; `plan.md` → Appendix A.1; `design/02-protocol-streaming.md`
§0, §1, §4, §6; `research/05-takserver-streaming-auth-verified.md` §4–§6 (framing, control set,
`sendPong`, flow tags, `<marti><dest>` order, mission templates §7);
`research/07-atak-client-verified.md` §3.5, §3.9, §7.2–7.5; `research/03-cloudtak-node-tak-contract.md`
§4.2 (never self-closing `<event/>`, control-character stripping).

## What was built

Everything under `rustak-cot/src/` except `proto/mod.rs` (M0-01's, untouched). `proto/`, `codec/`,
`negotiate.rs` and `msgs.rs` were **not** started — they belong to M1-02/M1-03. `rustak-cot/Cargo.toml`,
`build.rs`, `proto/tak_protocol_v1.proto` and the root `Cargo.toml` were **not** modified: the deps this
brief needs (`quick-xml`, `chrono`, `bytes`, dev `rstest`, `pretty_assertions`) were already there.

| File | Functional lines | Contents |
|---|---:|---|
| `lib.rs` | 11 | crate docs + runnable example, `pub mod` list, re-exports |
| `error.rs` | 163 | `BadTime`, `ParseError`, `ConvertError`, `FrameError`, `CodecError` + `From` chain |
| `time.rs` | 77 | `CotTime` (i64 ms), `parse`/`Display`/`Add<Duration>`/`Sub`/`stale_after`/`to_datetime` |
| `types.rs` | 63 | `cot_type::*`, `how::*`, `CONTROL_TYPES`, `is_control_type`, `is_atom`, `is_chat`, `is_mission_notice` |
| `event.rs` | 223 | `Event`, `Point`, `EventBuilder`, `is_sa`/`callsign`/`endpoint`/`contact`/`group`/`takv`/`is_control`/`is_stale_at` |
| `detail/mod.rs` | 234 | `Detail`, `Node`, `Element`, `TypedDetail`, `StrictDetail`, tree accessors |
| `detail/contact.rs` | 49 | `Contact`, `STREAMING_ENDPOINT` |
| `detail/group.rs` | 43 | `Group` (`__group`) |
| `detail/takv.rs` | 45 | `Takv` + `summary()` (`platform:version`) |
| `detail/track.rs` | 53 | `Track` (lenient coerces bad numbers, strict refuses them) |
| `detail/status.rs` | 44 | `Status` |
| `detail/precision.rs` | 43 | `PrecisionLocation` |
| `detail/marti.rs` | 158 | `Dest`, `DestKind`, `take_marti`, `read_marti`, `marti_element`, `ALL_STREAMING` |
| `detail/chat.rs` | 114 | `Chat`, `Remarks`, `remarks()`, `chat_uid()`, `DEFAULT_CHATROOM`, `LEGACY_CHATROOM` |
| `detail/fileshare.rs` | 150 | `FileShare`, `AckRequest`, `AckResponse`, `fileshare_pointer`, `FILESHARE_VALIDITY` |
| `detail/link.rs` | 68 | `Link`, `links()`, `RELATION_P_P` |
| `detail/mission.rs` | 165 | `MissionNotice`, `MissionChange`, `MissionDetail` (M1 skeleton) |
| `detail/flow_tags.rs` | 38 | `has_flow_tag`, `add_flow_tag`, `remove_flow_tag`, `flow_tag_name`, `ELEMENT` |
| `detail/takcontrol.rs` | 85 | `TakControl` (announce / request / response) |
| `xml/mod.rs` | 7 | `DECLARATION`, `MAX_DEPTH`, re-exports |
| `xml/parse.rs` | 247 | `parse`, `parse_str`, `parse_fragment` |
| `xml/write.rs` | 132 | `write`, `write_into`, `write_fragment`, `format_f64` |
| `xml/entity.rs` | 53 | entity/character-reference resolution and attribute normalisation |

Fixtures: `rustak-cot/fixtures/*.xml` (17, hand-written from the field tables — see the
`fixtures/README.md`, no captures copied), pinned outputs `rustak-cot/tests/golden/*.xml`
(regenerate with `RUSTAK_UPDATE_GOLDEN=1`), tests `rustak-cot/tests/{golden.rs,roundtrip_prop.rs}`.

## Exit checks

```
$ cargo test -p rustak-cot
running 153 tests
test result: ok. 153 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
running 49 tests
test result: ok. 49 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
running 4 tests
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo clippy -p rustak-cot --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-cot --no-deps
    Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_cot/index.html

$ cargo fmt --all --check
(no output: clean)   # see the note below — re-run it once rustak-server settles

$ ./scripts/check-file-length.sh
(no output: clean)

$ env -i PATH=/usr/bin:/bin:$HOME/.cargo/bin HOME=$HOME cargo build -p rustak-cot
    Finished `dev` profile [unoptimized + debuginfo] target(s)   # protoc absent from PATH
```

`cargo fmt --all --check` was clean when this brief's work finished, and `cargo fmt -p rustak-cot
--check` is clean at every point. A later re-run of the workspace-wide form fails with
`failed to resolve mod 'runtime': rustak-server/src/runtime.rs does not exist` — the concurrent
`rustak-server/` session has `pub mod runtime;` in `lib.rs` with the file not yet written. Nothing
in `rustak-cot/` is involved; the check passes again once that module lands.

`check-file-length.sh` walks `git ls-files`, and these sources are still untracked, so the same awk
was run over `rustak-cot/src/**.rs` directly. Largest: `xml/parse.rs` 247, `detail/mod.rs` 234,
`event.rs` 223. Every file has exactly one trailing column-0 `#[cfg(test)] mod tests` except
`lib.rs` (covered by its doctest).

## Public API for M1-02 / M1-03

### `rustak_cot` root re-exports

`Event`, `EventBuilder`, `Point`, `Detail`, `Element`, `Node`, `TypedDetail`, `CotTime`,
`ParseError`, `ConvertError`. Modules: `detail`, `error`, `event`, `proto`, `time`, `types`, `xml`.

### `event::Event`

```rust
pub const VERSION: &str = "2.0";

pub struct Event {
    pub version: String,            // "2.0" when the sender omitted it
    pub uid: String,
    pub r#type: String,
    pub how: Option<String>,
    pub time: CotTime, pub start: CotTime, pub stale: CotTime,
    pub access: Option<String>, pub qos: Option<String>, pub opex: Option<String>,
    pub caveat: Option<String>, pub releasable_to: Option<String>,
    pub extra_attrs: Vec<(String, String)>,      // unmodelled <event> attrs, in order
    pub point: Point,
    pub detail: Detail,
    pub proto_extensions: Vec<(u32, bytes::Bytes)>,   // opaque protobuf extensionDetails
}

impl Event {
    fn builder(r#type: impl Into<String>, uid: impl Into<String>) -> EventBuilder;
    fn callsign(&self) -> Option<&str>;      // non-empty <contact @callsign>
    fn endpoint(&self) -> Option<&str>;      // non-empty <contact @endpoint>
    fn is_sa(&self) -> bool;                 // uid + callsign + endpoint all non-empty
    fn is_control(&self) -> bool;            // types::is_control_type(&self.r#type)
    fn contact(&self) -> Option<Contact>; fn group(&self) -> Option<Group>; fn takv(&self) -> Option<Takv>;
    fn is_stale_at(&self, now: CotTime) -> bool;     // stale <= now
}

pub struct Point { pub lat: f64, pub lon: f64, pub hae: f64, pub ce: f64, pub le: f64 }
impl Point { const UNKNOWN_HAE/UNKNOWN_CE/UNKNOWN_LE: f64 = 9_999_999.0;
             const fn new(lat, lon) -> Self;      // hae/ce/le = unknown
             const fn zero() -> Self; }           // 0/0, hae 0, ce/le unknown — the server template
```

`EventBuilder` (all `#[must_use]`, chainable): `how`, `point(lat, lon)`, `point_full(Point)`,
`time(CotTime)`, `start`, `stale(CotTime)`, `stale_after(Duration)`, `access`, `qos`, `opex`,
`caveat`, `releasable_to`, `detail(Detail)`, `push(Element)`, `typed(&impl TypedDetail)`, `build()`.
`Event::builder` seeds `version = "2.0"`, `time = start = stale = CotTime::now()`, `point = zero()`.
**`.time(t)` preserves the current `stale - time` interval**, so `.stale_after(d).time(t)` and
`.time(t).stale_after(d)` agree.

### `detail`

```rust
pub struct Detail { pub nodes: Vec<Node> }
pub enum Node { Element(Element), Text(String), CData(String), Comment(String) }
pub struct Element { pub name: String, pub attrs: Vec<(String,String)>, pub children: Vec<Node> }
```

`Element`: `new`, `attr(k,v)`/`attr_opt(k, Option<v>)`/`with(node)` (builder, `#[must_use]`),
`get(&str) -> Option<&str>`, `set`, `remove_attr`, `elements()`, `child(&str)`, `push(impl Into<Node>)`,
`text() -> String` (own text + CDATA, not descendants), `is_empty()`,
`has_exactly(&[&str]) -> bool` (exact attribute-name set, order-insensitive).

`Detail`: `new`, `is_empty`, `elements()`, `elements_mut()`, `find`, `find_all`, `find_mut`,
`count(&str)`, `remove_all(&str) -> Vec<Element>`, `push(Element)`, `push_node(Node)`,
`get::<T: TypedDetail>() -> Option<T>`, `set::<T: TypedDetail>(&T)` (replace-first-or-append),
`FromIterator<Element>`.

```rust
pub trait TypedDetail: Sized {
    const NAME: &'static str;
    fn from_element(element: &Element) -> Option<Self>;   // LENIENT — reads what it recognises
    fn to_element(&self) -> Element;
}
pub trait StrictDetail: TypedDetail {
    fn strict(element: &Element) -> Option<Self>;         // protobuf rule: exact attrs, no children
}
```

`StrictDetail` is implemented by exactly the six protobuf sub-message types — **M1-02's
`split_detail` should call `T::strict`, never `from_element`**:

| Type | `NAME` | strict attribute set |
|---|---|---|
| `Contact` | `contact` | `{callsign}` or `{callsign, endpoint}` |
| `Group` | `__group` | `{name, role}` |
| `PrecisionLocation` | `precisionlocation` | `{geopointsrc, altsrc}` |
| `Status` | `status` | `{battery}`, must parse as `u32` |
| `Takv` | `takv` | `{device, platform, os, version}` |
| `Track` | `track` | `{speed, course}`, both must parse as `f64` |

All six also carry `extra: Vec<(String, String)>` holding the attributes they do not model, so a
lenient read followed by `to_element()` round-trips. Strict reads leave `extra` empty.

Other typed details (`TypedDetail` only, no `StrictDetail`): `Chat` (`__chat`, with
`participants()` and `room()`), `Remarks` (`remarks`), `FileShare` (`fileshare`), `AckRequest`
(`ackrequest`), `AckResponse` (`ackresponse`), `Link` (`link`), `MissionDetail` (`mission`),
`TakControl` (`TakControl`).

Free functions and constants M1-02/M1-03 will want:

```rust
detail::marti::{ALL_STREAMING, Dest, DestKind, take_marti(&mut Detail) -> Vec<Dest>,
                read_marti(&Detail) -> Vec<Dest>, marti_element(&[Dest]) -> Element};
detail::flow_tags::{ELEMENT, flow_tag_name, has_flow_tag, add_flow_tag, remove_flow_tag};
detail::link::{RELATION_P_P, links(&Detail) -> Vec<Link>};
detail::chat::{DEFAULT_CHATROOM, LEGACY_CHATROOM, remarks(&Detail) -> Option<Remarks>, chat_uid};
detail::contact::STREAMING_ENDPOINT;                       // "*:-1:stcp"
detail::fileshare::{FILESHARE_VALIDITY, fileshare_pointer(uid: &str, &FileShare, CotTime) -> Event};
detail::takcontrol::TakControl::{announce(version, server_version, api_version), request(v), response(ok), supports(v)};
```

`Dest::kind() -> Option<DestKind>` applies first-attribute-wins order (callsign → publish → uid →
mission → mission-guid → group) and honours `after` **only when `path` is also present**, matching
TAK Server. `take_marti` strips every `<marti>` unconditionally, including empty ones, and returns
only `<dest>` children carrying a routing attribute; a `path`-only dest is returned with
`kind() == None`.

### `time::CotTime`

`Copy + Ord + Hash`, i64 milliseconds. `now()`, `from_millis`, `millis()`, `EPOCH`,
`parse(&str) -> Result<Self, BadTime>` (accepts `Z`, numeric offsets, 1–9 fractional digits, and a
zone-less form read as UTC), `Display` → `YYYY-MM-DDTHH:MM:SS.sssZ`, `stale_after(Duration)`,
`Add<Duration>`, `Sub<CotTime> -> i64` (millisecond gap), `to_datetime`/`from_datetime`.

### `types`

`cot_type::{PING, PONG, TAKP_V, TAKP_Q, TAKP_R, DISCONNECT, GROUP_CHANGE, SUBSCRIBE, FILTER,
METRICS, INCOGNITO_ON, INCOGNITO_OFF, MISSION_CHANGE, MISSION_LOG_CHANGE, MISSION_CREATE,
MISSION_DELETE, MISSION_INVITE, MISSION_ROLE_CHANGE, FILESHARE, FILESHARE_ACK, CHAT,
CHAT_DELIVERED, CHAT_READ, CHAT_PENDING, CHAT_FAILED}`, `how::{M_G, H_E, H_G_I_G_O}`,
`CONTROL_TYPES` (the eleven TAK Server types), `is_control_type` (case-insensitive),
`is_atom`, `is_chat`, `is_mission_notice`.

### `xml`

```rust
pub const DECLARATION: &str = r#"<?xml version="1.0" encoding="UTF-8"?>"#;
pub const MAX_DEPTH: usize = 64;

pub fn parse(bytes: &[u8]) -> Result<Event, ParseError>;
pub fn parse_str(text: &str) -> Result<Event, ParseError>;
pub fn parse_fragment(text: &str) -> Result<Vec<Node>, ParseError>;   // xmlDetail -> nodes
pub fn write(event: &Event) -> bytes::Bytes;
pub fn write_into(event: &Event, dst: &mut bytes::BytesMut);
pub fn write_fragment(nodes: &[Node]) -> String;                      // nodes -> xmlDetail
pub fn format_f64(value: f64) -> String;                              // shortest round-trip, never NaN/inf
```

### `error`

`ParseError` (`Xml(quick_xml::Error)`, `MissingEvent`, `MissingPoint`, `BadNumber{attr,value}`,
`BadTime{attr,value}`, `Utf8`, `TooLarge`, `TooDeep`), `BadTime{value}`,
`ConvertError` (`NoCotEvent`, `BadXmlDetail(ParseError)`),
`FrameError` (`Oversized(usize)`, `BadVarint`),
`CodecError` (`Io`, `Frame`, `Parse`, `Convert`, `Decode(prost::DecodeError)`).
All four enums are `#[non_exhaustive]`; `From` impls wire the whole chain up to `CodecError`.
`ParseError::TooLarge` and every `FrameError`/`CodecError` variant are defined but unused so far —
they are there for M1-02/M1-03's framers so `error.rs` does not need reopening.

## Behaviour M1-02 / M1-03 must not re-litigate

* **Outbound bytes** (`xml::write`): `DECLARATION` + one `\n` + `<event …>…</event>`, no trailing
  newline, `<event>` never self-closing, `<detail>` omitted when empty, childless detail elements
  self-closing. Attribute order: `version uid type how time start stale [access qos opex caveat
  releasableTo] extra_attrs…`, then `<point lat lon hae ce le/>`.
* **No control characters reach the wire.** `U+0000`–`U+0008`, `U+000B`–`U+001F` (so `\r` always)
  and `U+007F`–`U+009F` are dropped from names, values, text, CDATA and comments; `\t` and `\n`
  survive in character data only. CDATA containing `]]>` is split across two sections; `--` in a
  comment becomes `- -`.
* **Parsing is forgiving**: BOM, declaration, PIs, junk before `<event` and bytes after `</event>`
  are ignored; single and double quotes are equivalent; unknown entities stay literal; a dangling
  `&` does not swallow a later well-formed entity.
* **Only `MissingPoint` is fatal for a well-formed event.** Missing `time` defaults to
  `CotTime::EPOCH`, missing `start` to `time`, missing `stale` to `start`, missing `version` to
  `"2.0"`; missing `hae`/`ce`/`le` default to the `9999999.0` sentinel. Unknown `<event>`
  attributes go to `extra_attrs`.
* **Round-trip fidelity is semantic, not byte-exact**: element order, attribute order, values, text,
  CDATA and comments survive; runs of whitespace-only character data between elements are dropped.
  Writing is idempotent, so `write(parse(write(e))) == write(e)`.
* **Documented lossy edges for the protobuf brief**: `extra_attrs` and `proto_extensions` have no
  XML/protobuf counterpart respectively — `to_proto` drops `extra_attrs`, `xml::write` drops
  `proto_extensions`.

## Deviations from `design/02` §1.2, and why

1. **No `thiserror` / `proptest` / `memchr`.** None of the three is in the workspace
   `[workspace.dependencies]`, and this session was scoped to files under `rustak-cot/`, so adding
   them would have meant editing the root `Cargo.toml` while another agent held it. `error.rs`
   therefore hand-writes `Display`/`Error`/`From` with exactly the variants the design lists —
   switching to `#[derive(Error)]` later is mechanical. `tests/roundtrip_prop.rs` uses a
   deterministic SplitMix64 generator instead of `proptest` (2 000 generated events, 3 000 random
   byte strings, seeds printed on failure). `memchr` is only needed by `codec/xml_frame.rs`, which
   this brief does not own. **If the orchestrator prefers the design's deps, add `thiserror = "2"`
   and `proptest = "1"` to `[workspace.dependencies]` and these two files can be simplified.**
2. **`fileshare_pointer(uid: &str, share: &FileShare, now: CotTime) -> Event`** rather than the
   design's eight positional parameters, which would trip `clippy::too_many_arguments`.
3. **`StrictDetail` trait** instead of six free `strict(el)` functions, so `to_proto` can be
   generic over the typed kinds. Semantics are exactly as designed.
4. **`xml/entity.rs` added** (not in the file table): reference resolution split out of
   `xml/parse.rs`, which was at 297 functional lines and would have broken the 300-line rule on the
   next edit.
5. **`ParseError::TooDeep` added** — the design specifies a depth cap of 64 but names no error for it.
6. **`Detail::set::<T>` replaces the first matching element only**; later duplicates are left alone.
   `remove_all` is the way to collapse duplicates.

## Not done here (owned by M1-02 / M1-03)

`proto/tak_protocol_v1.proto` (still M0-01's skeleton, package `rustak.cot.v1` — the design calls
for `atakmap.commoncommo.protobuf.v1` and the fuller field table, including
`Detail.extensionDetails = 8` which `Event::proto_extensions` already has a home for),
`proto/to_proto.rs`, `proto/from_proto.rs`, `codec/*`, `negotiate.rs`, `msgs.rs`, `rustak-cot/fuzz/`.
`fixtures/{ping,pong,takp-v,takp-q,takp-r,disconnect,mission-change}.xml` are already the shapes
`msgs.rs`/`negotiate.rs` must reproduce, and `tests/golden/` pins their canonical serialisation.
