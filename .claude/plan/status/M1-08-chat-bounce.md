# M1-08 — Bounce undeliverable GeoChat (`b-t-f` → `b-t-f-s`) and audit the control-message gaps

**Status: complete.** An explicitly addressed GeoChat that reaches nobody now comes back to its
sender as a `b-t-f-s`, the control-type set has been audited against `compat/streaming.md` and the
two gaps it had are closed, and incognito is confirmed to hide a subscription from both SA replay
and the contact listing. `compat/streaming.md` gained the bounce rule it was cited for but did not
contain.

## What landed

| Area | Files |
|---|---|
| The bounce builder + predicate | `rustak-cot/src/msgs.rs` |
| Bounce dispatch, control audit tests, incognito confirmation | `rustak-server/src/stream/router.rs` |
| "The sender named a person" (what bounces) | `rustak-server/src/stream/dest.rs` |
| Named the `t-b` family, table-driven control audit | `rustak-server/src/stream/control.rs` |
| `chat_bounced` counter | `rustak-server/src/stream/metrics.rs` |
| Integration case (two enrolled EUDs, delivered + bounced) | `rustak-server/tests/stream_routing.rs` |
| Contract corrections | `.claude/plan/compat/streaming.md` |
| Stale comment refreshed; **assertions unchanged** | `interop/eud/scenarios/chat-direct.toml` |

Nothing outside that list was touched. `stream/{notify,mission_notify}.rs`, `missions/**`,
`marti/**`, `web/api/**` and `rustak-client/**` were read but not edited.

## 1. The bounce

`rustak-cot/src/msgs.rs`:

```rust
pub fn bounces_when_undeliverable(event: &Event) -> bool;          // b-t-f… minus b-t-f-s
pub fn chat_delivery_failure(undelivered: &Event, server_id: &str) -> Event;
```

`chat_delivery_failure` is deliberately **not** a template. It is the sender's own message with
`type` set to `b-t-f-s`, every `<marti>` taken off and *this* server's flow tag removed (other
servers' tags stay, matching TAK Server's `FlowTagFilter.unfilter`). Everything else — uid, point,
times, `how`, `__chat`/`chatgrp`, `<link>`, `<remarks>` — is echoed.

That is load bearing rather than lazy. ATAK files any `b-t-f…` into a conversation by
`__chat/@id`, falling back to the dot-separated components of the event uid (research 07 §7.4), so
a notice built from scratch would land in the wrong conversation or in none and the sender's client
would go on showing the message as sent. Research 05 §8.1 describes TAK Server doing the same
thing: copy, strip the explicit keys and the flow tag, set the type, address it back at the sender.

**Golden test**: `the_bounce_is_the_senders_own_chat_with_its_type_changed` in `msgs.rs` pins the
rendered bytes of a bounce built from an ATAK-shaped direct chat, in the style the ping/pong/
disconnect templates in that file already use. (The `tests/golden/` fixture corpus is not one of
this brief's files, and `msgs.rs` is where the other server-originated templates are pinned.)

One observable consequence of the faithful copy: `remove_flow_tag` takes the attribute, not the
element, so a bounce carries an empty `<_flow-tags_/>`. That is what TAK Server's `unfilter` does
and no client reads it; the golden pins it so the choice is visible rather than accidental.

### Where it fires

`stream/router.rs` step 8, after recipient selection:

```rust
if selection.handles.is_empty() {
    StreamMetrics::incr(&self.metrics.no_recipients);
    if selection.direct { self.bounce(from, encoded.event()); }
    return Disposition::Dropped(DropReason::NoRecipients);
}
```

Delivery is straight down the connection the chat arrived on, exactly like the pong: not brokered,
so no flow tag of its own and no reachability question. A sender whose connection has gone in the
meantime is a no-op. `Disposition` is unchanged — a bounced message is still `Dropped`, and the new
`StreamMetrics::chat_bounced` counter is what distinguishes it.

### Scope: `Selection::direct`

`dest.rs` gained `Selection::direct`, set from `Addresses::names_people()`:

```rust
fn names_people(&self) -> bool {
    self.missions.is_empty() && (!self.callsigns.is_empty() || !self.uids.is_empty())
}
```

Narrower than `explicit`, on purpose:

| Case | Bounces? | Why |
|---|---|---|
| `<dest callsign>` / `<dest uid>` reaching nobody | **yes** | somebody typed at a named person and was not told otherwise |
| no `<marti>` (room chat) | no | a broadcast reaching nobody means nobody is connected |
| `<dest callsign="All Streaming">` | no | already degraded to a broadcast by §8 rule 1 |
| `<dest group>` | no | a channel with nobody listening is not a delivery failure |
| `<dest mission>` (with or without a callsign alongside) | no | the write was stored; no connected subscriber is ordinary |
| `<dest publish>` | no | matches nobody by design, and names nobody |
| non-chat types (SA, `b-f-t-r`, …) | no | a statement about the world, not a message to a person |
| `b-t-f-s` itself | no | or a sender that left between the chat and the bounce would have the bounce bounce |

Receipts (`b-t-f-d`, `b-t-f-r`, `b-t-f-p`) **do** bounce: they are inside the `b-t-f` prefix, which
is TAK Server's own test (`type.startsWith("b-t-f") && type != "b-t-f-s"`, 05 §8.1).

## 2. Control-message audit

The classifier set (`rustak-cot::types::CONTROL_TYPES`) already matched research 05 §5.1 exactly —
eleven types, case-insensitive lookup. Two gaps, both now closed:

1. **`compat/streaming.md` §7 omitted the whole `t-b` family** (`t-b`, `t-b-a`, `t-b-c`, `t-b-q`)
   from its table, so the contract file disagreed with the code that implements it. Added, with
   what TAK Server does with a `t-b` written out: it reads `detail/subscription/tests/@xpath` and
   sets that filter on the caller's own subscription, or reads `detail/subscription/@publish` as
   `proto:host:port` and **opens an outbound connection** to it — both unauthenticated, from the
   stream socket. rustak implements neither, and `control.rs` now says so by name
   (`SUBSCRIPTION_CONTROL`, `Ignored("a subscription control message")`) instead of letting them
   fall into the unrecognised branch.
2. **The doc did not warn about TAK Server's `default:` branch**, which calls
   `deleteSubscription(c.getUid())` on the *subscription* uid — a client that guesses or learns
   another subscription's uid can disconnect it (05 §5.3). rustak's fall-through is a no-op; that
   is now recorded as a deliberate divergence rather than an accident.

Tests added (both table-driven over `CONTROL_TYPES`, so a type added to the set without a decision
fails the suite):

- `control.rs::every_control_type_is_handled_here_and_only_a_ping_is_answered` — each type is
  classified as control, produces the expected `ControlAction`, leaves the subscription registered,
  and only `t-x-c-t` gets a reply.
- `control.rs::the_subscription_control_family_sets_no_filter_and_dials_nobody` — a `t-b…` carrying
  a populated `<subscription publish="stcp:…"><tests xpath="/event"/></subscription>` is ignored.
- `router.rs::every_control_type_is_consumed_and_reaches_no_other_client` — the "never relayed"
  half, which is only observable with a second connection attached.

## 3. Incognito — confirmed, with one deviation to note

`t-x-c-i-e`/`-d` do reach `Hub::set_incognito` (router → `control::handle` →
`msgs::incognito_toggle`), and the flag is honoured in both places the brief asks about:

- **SA replay** — `Hub::latest_sa_for` filters `!peer.incognito`.
- **`/Marti/api/contacts/all`** — `marti/contacts.rs` calls `Hub::snapshot_for`, which filters
  `!peer.incognito`, for an ordinary viewer.

`router.rs::going_incognito_hides_the_sender_from_replay_and_the_contact_list` drives the toggle
through `handle_inbound` and asserts both, in both directions.

**Deviation, pre-existing and left alone:** `marti/contacts.rs` uses the unfiltered
`Hub::snapshot()` for an **administrator**, so an admin still sees incognito subscriptions in the
contact listing. That is a defensible choice — an admin is asking about the server, not about the
network — but it is a divergence from TAK Server, which has no such branch. `marti/**` is not this
brief's to edit; recorded here and in `compat/streaming.md` §7 so it is a decision rather than a
surprise. Raise a brief if it should be a per-viewer rule instead.

## 4. `compat/streaming.md` corrections

The brief cited a "§ routing: undeliverable `b-t-f` ⇒ `b-t-f-s`" rule. **It was not in the file** —
only in `plan.md` line 298 and research 05 §8. Added:

- §8 gained a subsection, *Undeliverable GeoChat bounces back (`b-t-f` ⇒ `b-t-f-s`)*: the rule, why
  the bounce is the sender's own message rather than a template, what comes off it, and the scope
  table above.
- §7's table gained the `t-b` family and an explicit row for a case variant (`T-X-C-T` is consumed
  but not acted on), plus the "do not copy TAK Server's fall-through" warning and a note that the
  set is exactly eleven strings compared case-insensitively.
- §7's incognito paragraph now names the contact listings alongside SA replay, including rustak's
  admin exception.
- *Gotchas* gained three lines: the bounce is an echo, only chat bounces and only at a named
  person, and a `t-b` is not a harmless no-op to implement later.

## 5. `interop/eud/scenarios/chat-direct.toml`

**Expectations unchanged.** The scenario asserts `xml_present = [{ type = "b-t-f-s" }]` on ALPHA
and `{ type = "b-t-f" }` on BRAVO; the implemented bounce is ALPHA's own `b-t-f` handed back with
the type changed, which is exactly what the file already described. The only edit is the comment
block that said the bounce "is not implemented server-side yet" and that the scenario "fails here
rather than passing quietly" — now stale, replaced with where the bounce is built and why the
assertions did not have to move. The scenario is nightly-only (`eud`), so it has not been run here.

`.claude/plan/status/M2-09-eud-interop-scenarios.md` still records `chat-direct` as a standing
expected failure; that line is now out of date, but that status file belongs to another brief and
was left alone.

## Exit checks

Six other briefs were editing this working tree throughout. Everything below is green; where a
check reports a failure it is in a file this brief does not own, and that is called out. Every one
of the six files this brief owns passes `rustfmt --check` and draws no clippy lint.

```
$ cargo test -p rustak-cot
running 253 tests   test result: ok. 253 passed; 0 failed; 0 ignored    (lib)
running 9 tests     test result: ok. 9 passed;   0 failed; 0 ignored    (codec_framed)
running 49 tests    test result: ok. 49 passed;  0 failed; 0 ignored    (golden)
running 9 tests     test result: ok. 9 passed;   0 failed; 0 ignored    (roundtrip_prop)
running 3 tests     test result: ok. 3 passed;   0 failed; 0 ignored    (doc-tests)

$ cargo test -p rustak-server --features testing -- stream
     Running unittests src/lib.rs
running 140 tests
test stream::control::tests::every_control_type_is_handled_here_and_only_a_ping_is_answered ... ok
test stream::control::tests::the_subscription_control_family_sets_no_filter_and_dials_nobody ... ok
test stream::dest::tests::naming_a_person_is_what_an_undeliverable_chat_bounces_on ... ok
test stream::router::tests::a_chat_addressed_to_nobody_reachable_comes_back_as_a_bounce ... ok
test stream::router::tests::a_chat_that_was_delivered_is_never_bounced ... ok
test stream::router::tests::a_chat_nobody_was_addressed_in_is_never_bounced ... ok
test stream::router::tests::a_bounce_that_cannot_be_delivered_does_not_bounce_again ... ok
test stream::router::tests::an_undeliverable_position_report_is_dropped_without_a_word ... ok
test stream::router::tests::every_control_type_is_consumed_and_reaches_no_other_client ... ok
test stream::router::tests::going_incognito_hides_the_sender_from_replay_and_the_contact_list ... ok
  … 130 more …
test result: ok. 140 passed; 0 failed; 0 ignored; 0 measured; 1425 filtered out; finished in 10.46s
(every other target: 0 failed)

$ cargo test -p rustak-server --features testing --test stream_routing
running 12 tests
test an_undeliverable_direct_chat_comes_back_to_its_sender ... ok
test a_relayed_message_carries_this_servers_flow_tag ... ok
test all_streaming_discards_the_callsign_list_and_broadcasts ... ok
test a_client_in_another_channel_hears_nothing ... ok
test addressing_a_callsign_in_another_channel_reaches_nobody ... ok
test two_members_of_one_channel_see_each_other ... ok
test a_broadcast_never_comes_back_to_its_sender ... ok
test a_message_that_has_already_been_here_is_dropped ... ok
test a_message_addressed_to_a_uid_reaches_that_device ... ok
test reachability_is_not_symmetric ... ok
test a_message_addressed_to_a_callsign_reaches_only_that_callsign ... ok
test an_incognito_client_reaches_only_the_people_it_names ... ok
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.62s

$ cargo clippy --workspace --all-targets -- -D warnings
(clean for `rustak-cot` and for every file this brief owns; the run as a whole fails on
 in-progress work in other briefs' files:
   rustak-server/src/config/validate.rs
   rustak-server/src/web/api/services.rs
   rustak-client/src/control/register.rs)

$ cargo fmt --all --check
(no diff in any file this brief owns; the run as a whole reports two other briefs' files:
   rustak-server/src/jobs/service_health.rs
   rustak-client/src/sidecar/mod.rs)

$ rustfmt --edition 2024 --check \
    rustak-cot/src/msgs.rs \
    rustak-server/src/stream/{router,dest,control,metrics}.rs \
    rustak-server/tests/stream_routing.rs
(no output)

$ ./scripts/check-file-length.sh
(no output — every file under 300 functional lines)

$ cargo doc -p rustak-cot --no-deps
    Finished (no broken intra-doc links in the new documentation)
```

Functional line counts after the change, all well inside the 300-line limit: `msgs.rs` 90,
`router.rs` 169, `dest.rs` 189, `control.rs` 44, `metrics.rs` 36.

## Notes for the integrator

- `Selection` gained a public field (`direct`). It is re-exported from `stream::mod`, but only
  `router.rs` reads it.
- `StreamMetrics` gained `chat_bounced`. Nothing enumerates the struct's fields, so no exporter
  needed updating.
- `rustak-cot` is unchanged in behaviour for every existing caller: the two new functions are
  additive and nothing else in `msgs.rs` moved.
