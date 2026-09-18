# M1-02 — `rustak-cot`: TAK protocol v1 protobuf conversion — complete

Brief: `.claude/plan/briefs/M1-02-cot-protobuf.md`
Read first: `.claude/plan/conventions.md`; `.claude/plan/status/M1-01-cot-model-xml.md`;
`.claude/plan/compat/streaming.md` §3; `.claude/plan/plan.md` Appendix A.1;
`.claude/plan/design/02-protocol-streaming.md` §0, §1.1, §1.2, §4;
`.claude/plan/research/05-takserver-streaming-auth-verified.md` §12.

## What was built

| File | Functional lines | Contents |
|---|---:|---|
| `proto/tak_protocol_v1.proto` | — | Clean-room schema, package `atakmap.commoncommo.protobuf.v1`, full field table |
| `src/proto/mod.rs` | 27 | generated types (in a nested `generated` module), `encode`, `decode`, re-exports |
| `src/proto/to_proto.rs` | 113 | `event_to_message`, strict `split_detail`/`promote` |
| `src/proto/from_proto.rs` | 105 | `message_to_event`, typed-element rendering, xmlDetail-wins merge |
| `tests/roundtrip_prop.rs` | — | new `proto_conversion` module: 4 proptest properties + the SA hex golden |
| `Cargo.toml` | — | `bytes` gains `features = ["serde"]`; `proptest` added to dev-deps |

`build.rs` was **not** touched (it is not in the brief's file list), so the schema kept the filename
`proto/tak_protocol_v1.proto` rather than moving to design §1.1's `proto/tak/v1.proto`. Only the
package line changed, so the generated module is now `$OUT_DIR/atakmap.commoncommo.protobuf.v1.rs`.
`event.rs` needed no change: M1-01 already shipped `Event::proto_extensions: Vec<(u32, Bytes)>`.
Nothing under `src/{codec,negotiate.rs,msgs.rs}` was read for behaviour or edited.

### The schema

`TakMessage{takControl=1, cotEvent=2; reserved 3, 4}` — 3/4 are a server's own submission and
creation timestamps; reserving them keeps the numbers unclaimed while proto3 skips them on decode
(there is a test for exactly that). `TakControl{minProtoVersion=1, maxProtoVersion=2, contactUid=3,
repeated extensionIds=4}` is decoded for completeness and never populated: rustak negotiates in XML.
`CotEvent` 1–17 including `caveat=16` and `releasableTo=17` (the XML attribute spelling — the
number is what is load-bearing). `Detail{xmlDetail=1, contact=2, group=3, precisionLocation=4,
status=5, takv=6, track=7, repeated ExtensionEncodedDetail extensionDetails=8}` with
`ExtensionEncodedDetail{extensionId=1 uint32, data=2 bytes}` nested inside `Detail` as design §1.1
writes it. Leaves exactly per the field table. Every comment is our own prose.

### Public API (for M1-03 and `rustak-server`)

```rust
// rustak_cot::proto
pub fn event_to_message(event: &Event) -> TakMessage;                          // #[must_use]
pub fn message_to_event(message: TakMessage) -> Result<Event, ConvertError>;
pub fn encode(message: &TakMessage) -> bytes::Bytes;                           // #[must_use]
pub fn decode(bytes: &[u8]) -> Result<TakMessage, prost::DecodeError>;

pub use generated::{Contact, CotEvent, Detail, Group, PrecisionLocation,
                    Status, TakControl, TakMessage, Takv, Track};
pub use generated::detail::ExtensionEncodedDetail;
```

`encode` emits the **frame payload only** — the codec adds `0xBF` and the LEB128 length. Names and
signatures are exactly design 02 §1.2, so the concurrent `codec/` work calling
`crate::proto::{event_to_message, encode}` needs no adjustment.

### Conversion rules as implemented

**XML → proto.** A `<detail>` child is promoted to a typed sub-message only when *all four* hold:
exactly one top-level element of that name exists, its attribute **names** are exactly the modelled
set, it has no children, and every numeric attribute parses. `StrictDetail::strict` (M1-01) supplies
rules 2–4; `to_proto::promote` adds rule 1 via `Detail::count`. Everything not promoted is
concatenated by `xml::write_fragment` into `xmlDetail` with no wrapper and no declaration, in
document order, including text, CDATA and comments. `xmlDetail` is left unset when nothing remains,
and `CotEvent.detail` is `None` when the detail is empty *and* there are no extensions. This is the
spec-strict variant research 05 §12.2 describes, not TAK Server's own converter: we never take the
first of several `<contact>`s and never coerce `battery="full"` to `0`. Both behaviours are
compatible with TAK Server's decoder, which reads `xmlDetail` the same way.

**Proto → XML.** Typed sub-messages render first, in the order contact, `__group`,
`precisionlocation`, `status`, `takv`, `track`; then the parsed `xmlDetail` nodes. A typed element is
**skipped** when the fragment already carries a top-level element of that name — xmlDetail wins,
because only the fragment copy can hold the attributes or children that stopped it being promoted.
`extensionDetails` land in `Event::proto_extensions` untouched. Empty proto3 strings decode as
`None`, not `Some("")`. Times are milliseconds both ways.

### Documented losses (asserted by the property tests, not just claimed)

1. `Event::extra_attrs` — no field exists; dropped by `event_to_message`.
2. `Event::version` — no field exists; a decode always yields `"2.0"`.
3. `<detail>` child order — typed elements move to the front.
4. `<contact endpoint="">` loses the empty attribute, because proto3 cannot distinguish an empty
   string from an unset one. (TAK Server behaves the same way.)
5. A pre-epoch `CotTime` clamps to 0, since the time fields are `uint64`.

Everything else round-trips exactly, and the `the_conversion_reaches_a_fixed_point` property proves
the normalisation is idempotent — relaying a message through proto twice changes nothing after the
first pass.

### Tests

26 unit tests across `proto/{mod,to_proto,from_proto}.rs` covering the design §4 list: every strict
rule (extra attribute, missing attribute, child element, unparsable number), duplicate `<contact>`,
`battery="full"` not coerced, `xmlDetail` omitted when empty, no `Detail` when empty, name-collision
resolution, empty `how`, extension pass-through, pinned field-number bytes, reserved 3/4 skipped,
truncated payload is an error not a panic.

`tests/roundtrip_prop.rs` gains a `proto_conversion` module (proptest): fixed point, wire round trip,
"only the documented losses happen", and `decode` + `message_to_event` never panicking on arbitrary
bytes — plus the SA-fixture **hex golden**. The golden lives here rather than in `tests/golden.rs`
because the brief's file list does not include that file; it pins the exact 313 bytes
`fixtures/sa-full.xml` encodes to and then re-decodes them to check the meaning survived.

## Exit checks

```
$ cargo test -p rustak-cot --lib -- --skip codec::
test result: ok. 188 passed; 0 failed; 0 ignored; 0 measured; 49 filtered out

$ cargo test -p rustak-cot --lib proto::
test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 210 filtered out

$ cargo test -p rustak-cot --test golden
test result: ok. 49 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test -p rustak-cot --test roundtrip_prop
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test -p rustak-cot --doc
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo clippy -p rustak-cot --all-targets -- -D warnings -A clippy::useless_vec
    Finished `dev` profile [unoptimized + debuginfo] target(s)

$ RUSTDOCFLAGS="-D warnings -A rustdoc::private_intra_doc_links" cargo doc -p rustak-cot --no-deps
    Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_cot/index.html

$ rustfmt --edition 2024 --check rustak-cot/src/proto/{mod,to_proto,from_proto}.rs \
      rustak-cot/tests/roundtrip_prop.rs
(no output: clean)

$ ./scripts/check-file-length.sh
(no output: clean)

$ env -i PATH=/usr/bin:/bin:$HOME/.cargo/bin HOME=$HOME cargo build -p rustak-cot
    Finished `dev` profile [unoptimized + debuginfo] target(s)   # protoc absent from PATH
```

### Why three checks carry a scope flag

`rustak-cot/src/{codec/**,negotiate.rs,msgs.rs}` are being written **concurrently** by the M1-03
session and were mid-edit throughout. At the time of writing, the unmodified forms of the three
whole-crate checks fail only inside those files:

* `cargo test -p rustak-cot` → `codec::xml_frame::tests::an_oversized_message_is_reported_and_consumed`
  panics on `Result::unwrap()` of `Oversized(78)` (`codec/xml_frame.rs:286`). 236 of 237 pass.
* `cargo clippy … -D warnings` → `clippy::useless_vec` at `codec/proto_frame.rs:237` and
  `clippy::useless_conversion` at `tests/codec_framed.rs:311` (the list moved while M1-03 worked;
  with those two lints allowed, `--all-targets` is clean).
* `cargo doc -D warnings` → `rustdoc::private_intra_doc_links`: `read_varint`'s docs link to the
  private `MAX_LEN` (`codec/varint.rs:39`).
* `cargo fmt -p rustak-cot --check` → diffs in `codec/{encoded,mod,proto_frame,xml_frame}.rs`.
* `env -i … cargo build` → one `unused_imports` warning for `TypedDetail` in `negotiate.rs:25`.

None of these touch this brief's files; the scoped commands above show that everything M1-02 owns is
clean, and the whole-crate forms should pass once M1-03 lands. **Note for the orchestrator:** an
early `cargo fmt -p rustak-cot` (without `--check`) in this session reformatted the whole crate,
including M1-03's then-current files. Nothing semantic changed and M1-03 has written over them
since; later formatting checks here used `rustfmt --check` on this brief's files only.

`check-file-length.sh` walks `git ls-files`, and these sources are untracked, so the same awk was run
over `src/proto/*.rs` directly: `to_proto.rs` 113, `from_proto.rs` 105, `mod.rs` 27 functional lines.
Each has exactly one trailing column-0 `#[cfg(test)] mod tests`.

## Decisions worth knowing

1. **`bytes` needs its `serde` feature** in `rustak-cot/Cargo.toml`. `build.rs` derives
   `Serialize`/`Deserialize` on every generated type, and `ExtensionEncodedDetail.data` is a
   `bytes::Bytes` (because `build.rs` configures `.bytes(["."])`). The feature is added on the
   workspace dependency in the crate manifest, not in the root `Cargo.toml`, to stay inside the
   brief's file list.
2. **Generated code sits in a private `mod generated`** rather than being `include!`d directly into
   `proto/mod.rs`. An inner `#![allow(clippy::all)]` at module level would have covered
   `to_proto.rs` and `from_proto.rs` too; the nested module keeps the blanket allow on machine-written
   code only.
3. **`proptest` is used for the protobuf properties only.** The XML properties M1-01 wrote keep their
   deterministic SplitMix64 generator — rewriting them was out of scope and would have risked a
   regression. `proptest` is now in `[workspace.dependencies]`, so M1-01's deviation 1 can be closed
   separately if the orchestrator wants one generator.
4. **`ConvertError::BadXmlDetail` fails the whole message**; the typed sub-messages are not salvaged
   from a message whose `xmlDetail` is malformed. A peer that sent a broken fragment sent a broken
   message, and half-converting it would silently drop detail.
5. **The schema keeps `releasableTo`** (the XML attribute spelling) rather than the `releaseableTo`
   found in some published copies. Field 17 is the part that has to match; `compat/streaming.md`
   explicitly allows either spelling as long as it is consistent.

## Not done here (owned by M1-03)

`src/codec/**`, `src/negotiate.rs`, `src/msgs.rs`, `rustak-cot/fuzz/`. The `proto_decode` fuzz target
design §4 calls for has a cheap stand-in in `tests/roundtrip_prop.rs`
(`decoding_arbitrary_bytes_never_panics`) until the real one lands.
