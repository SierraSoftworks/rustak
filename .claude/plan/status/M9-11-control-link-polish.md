# M9-11 — Control-link polish from the first working deployment

**Status: done.** All four production findings in the brief are addressed; every exit check passes.

## What changed

### 1. A long-lived feed with no total timeout

`rustak-client/src/http.rs` grew a second client constructor. `client()` (unchanged callers, unchanged
semantics) now also sets `CONNECT_TIMEOUT` (10s) alongside its total `DEFAULT_TIMEOUT` (30s);
`feed_client(identity, trust, idle)` sets the connect timeout and a **read** timeout and **no total
timeout at all**. Both go through one private `build()` so the roots and the client certificate are
decided in exactly one place — a feed that trusted a different set of roots would be a second
security decision nobody made.

- `FEED_IDLE_TIMEOUT = 65s`. The server already writes an SSE keep-alive comment every **20s**
  (`KEEPALIVE` in `rustak-server/src/web/api/events.rs`) — nothing was added there; the constant's
  doc comment now records that a client depends on it. 65s is three missed keep-alives plus slack,
  and `http.rs` has a test asserting the relationship (`>= 3 × 20s`, `< 6 × 20s`) so that shortening
  one without the other fails.
- `reqwest 0.13`'s `ClientBuilder::read_timeout` is the idle timeout: it resets on every byte, so
  the keep-alive comments *are* the liveness signal. When it fires it surfaces as an error on the
  body, which `EventStream` already treats as "the feed ended" — so the reconnect path, the resume
  from `Last-Event-ID` and `link_health`'s backoff are all unchanged.
- `ControlClient` gained an optional `feed: Option<reqwest::Client>` and
  `with_feed_http(client)`; `events()` builds its request through `request_on(self.feed_http(), …)`.
  `None` falls back to the ordinary client, which is what the wiremock tests want.
  `ControlClient::new()` and `SidecarContext::from_config` both supply a real feed client.
- **Token reuse across reopenings: verified.** `AccessTokens::current()` caches until
  `RENEW_BEFORE` (60s) before expiry, and the feed asks before every opening. Test
  `reopening_the_feed_does_not_buy_another_token` asserts five openings cost one `/oauth/token`.
  I also serialised the exchange behind a `tokio::sync::Mutex` (double-checked): the harness and the
  feed task both ask the instant a sidecar starts and both used to miss the cache, so a clean start
  cost two exchanges. Test `two_callers_asking_at_once_buy_one_token_between_them`.

### 2. Quiet reopening

The feed task moved out of `control_link.rs` into a new `rustak-client/src/sidecar/event_feed.rs`
(`control_link.rs` was at 269 functional lines, 31 from the limit). `note`/`announce`/`recovered`
moved into `link_health.rs`, which both callers now share — that module is already "what an operator
should be told", and the two copies would have drifted.

`link_health.rs` gained `Reopenings`, in the same shape as `LinkHealth` (pure, clock injected,
`Opened::{Announce, Quiet, Churning}`):

| State | Level | Line |
|---|---|---|
| First open ever | `info` | `The server-event feed is open.` |
| Open after an outage `LinkHealth` announced | `info` | same |
| Clean close → clean reopen | `debug` | `The server-event feed is open again.` |
| More than 5 clean closes in 5 minutes | `warn`, once per window | `The server-event feed has closed and reopened N times in the last 5m00s; something between this sidecar and [server] control is cutting it.` |

A failure *before* the first successful open does not make that open a "reopen" — a sidecar that
starts before its server does still gets one `info` line, not a `debug` one.

### 3. Server: routine success is not INFO

`rustak-server/src/web/telemetry.rs` — the request span (`info_span!("request", …)`) is **unchanged**
and still exported for tracing/OTel; what changed is that the middleware now has an explicit,
tested decision function:

```rust
fn level_for(status: StatusCode) -> Option<Level>   // 2xx/3xx → None, 4xx → DEBUG, 5xx → WARN
```

`finished(status)` records `http.status_code` on the span (as before) and then logs at that level, or
not at all. Both the `Ok` and the `Err` arms go through it, so a handler that returns a 500 as a
response and one that returns an actix error are treated the same. `record_custom_error` (Sentry) is
untouched. **The audit log is untouched.** So are the refusal lines in `auth/workload/mod.rs`,
`plugins/auth.rs` and `auth/resolve.rs`, which are where a security *decision* is recorded — those
were the "401/403 on credential endpoints" posture the brief asked about, and they are not in the
middleware.

One `info` was demoted in my own file: `rustak-server/src/web/api/events.rs` —
`"A consumer opened the server-event feed."` is now `debug`. It was one of the three lines each feed
reopening cost, and opening a feed is a routine success; what an administrator actually wants
(which services are attached) is `GET /api/v1/services`, a fact rather than a log line.

**What an operator sees at `LOG_LEVEL=info`, idle server, two sidecars: zero lines per minute.**
Both sidecars hold one `GET /api/v1/events` open indefinitely and post one heartbeat per tick; all
of those are 2xx and none of them logs. Server-side traffic that still produces `info` lines, and
how often:

| Line | Cadence |
|---|---|
| `A workload identity authenticated.` + `Issued an access token from a workload identity.` (`auth/workload`) | once per sidecar per access-token lifetime (~1h), **not** per feed reopening — previously ~2/minute/sidecar |
| Nothing else | — |

Before this change the same two idle sidecars produced 74 lines in 2.5 minutes (~30/minute).

### 4. Identity line

`workload.rs` now has two sentences, both rendered by pure functions so the wording is asserted
directly:

- `identity_line(source, account)` — **unchanged**:
  `Identity: this sidecar is 'adsb', from the certificate at '/data/adsb.pem'.`
- `control_identity_line(source, account)` — new:
  `Identity: authenticating to the control API as 'svc.adsb' with the workload identity from NOMAD_TOKEN_rustak.`
  with `identity_source`, `issuer` and `account` fields as before.

The account is the exchanged token's `sub` (`unverified_subject`, factored out of `unverified_issuer`
into a shared `unverified_claim`) — the account rustak *resolved* the assertion to — falling back to
`[service] account` (plumbed in via `AccessTokens::for_account`) and then to `-`. Never a
description of the endpoint. Logged **once**: an `AtomicBool` on `AccessTokens`, so neither the
hourly re-exchange nor the two start-up callers repeat it.

### 5. First connect

- `run.rs`: before the first tick (and racing the shutdown, so Ctrl-C does not wait), the harness
  calls `link.settle(FIRST_CONNECT)` — 10s, then it proceeds and the ordinary path takes over.
- `link.rs`: `settle` polls the link until it is connected, queueing whatever the connection produced
  into a new `queued: VecDeque<SidecarEvent>` that `poll_next` drains first — so the plugin is still
  told `Connected`/`Negotiated`, in order, and nothing is consumed. A link with no `[server] stream`
  answers immediately.
- `link.rs`: a `Discards` state machine replaces the unconditional `warn!`:

| State | Level | Line |
|---|---|---|
| Never connected | `debug` | `Discarding events: the CoT stream has not finished connecting yet.` |
| Was up, dropped, first batch lost | `warn`, once | `Discarding events: the CoT stream is reconnecting. Further discards are logged at debug until it is back.` |
| Still down | `debug` | `Discarding events: the CoT stream is still reconnecting.` |
| Still down, 5 minutes on | `warn` | `The CoT stream has been reconnecting for 5m00s; N events have been discarded since this was last reported.` |

A reconnection resets it, so the *next* outage is announced again.

### 6. Tests

- `control/events.rs`: a raw-socket SSE server (wiremock cannot write a body over time) that never
  closes the connection, so only a client timeout can end a stream.
  - `a_feed_outlives_the_total_timeout_an_ordinary_call_carries` — the same feed, read through a feed
    client and through one carrying a total timeout: the first delivers the late event, the second is
    cut. Milliseconds, injected, not 30 seconds.
  - `a_feed_that_stops_sending_keepalives_is_ended_rather_than_held_open`
  - `a_keepalive_comment_keeps_a_silent_feed_open`
- `link_health.rs`: six tests on `Reopenings` (state machine, not log capture), including the churn
  reminder, the window pruning and "a failure before the first open is not a reopen".
- `link.rs`: four on `Discards` (never-connected / first / quiet / reminder / re-announce after
  recovery) and two on `settle`'s bounds.
- `http.rs`: `feed_client` builds from the same identity and roots as `client`; the idle timeout
  leaves room for three missed keep-alives.
- `workload.rs`: both rendered identity lines; `unverified_subject`; the line is written once; five
  openings cost one exchange; four concurrent callers cost one exchange.
- `telemetry.rs`: three tests on `level_for` (2xx/3xx → nothing, 4xx → debug, 5xx → warn).
- `run.rs`: `the_first_tick_publishes_into_a_connection_that_is_actually_up` — a real listener, a
  one-hour tick so the start-up tick is the only tick there will ever be. **Verified to fail without
  the fix** (`Timeout("an event BRAVO was waiting for")` after 10s when the `settle` call is removed).
- `rustak-server/tests/feed_sidecars.rs`:
  `the_first_batch_a_feed_produces_reaches_the_stream_on_a_clean_start` — the first vessel has to
  arrive inside 4s, which is less than the publisher's 5s `min_interval`, so anything that arrives is
  the first batch rather than a republish. Additive; no other hunk in that file touched.

### 7. Docs

- `docs/plugins.md`: new "The feed is long-lived, and it says so quietly" under **Reacting to server
  events** (keep-alive interval, idle timeout, the table of what is logged and when, token reuse);
  the identity-line block now shows the control-API line and explains why it is a second sentence.
- `docs/deployment.md`: **Logging and telemetry** now states the status→level rule and the
  zero-lines-per-minute target explicitly, plus a new "A reverse proxy in front of `[web.public]`"
  subsection (do not buffer; do not idle out under ~70s; do not put a total request timeout on it,
  naming the 20s keep-alive). The workload-identity start-up block shows both identity lines.

## What I could not verify

- **The ~40 ms first-connect race does not reproduce in `feed_sidecars.rs`.** On this machine the
  loopback TLS handshake completes before the AIS plugin's first tick either way — I measured 802 ms
  with the hold and 813 ms without, i.e. the first batch was never discarded locally. The harness
  test therefore asserts the right end-to-end property but is not currently *sensitive* to a
  regression. The deterministic guard is `run.rs`'s `the_first_tick_publishes_into_a_connection_that_
  is_actually_up`, which I confirmed fails when the `settle` call is removed.
- **No live re-deployment.** The 31-second reopening, the 74 lines and the doubled identity line were
  reproduced from the code and from the brief's log extracts, not from a running Dublin deployment.
  Confirming the fix in production needs one deploy and a `LOG_LEVEL=info` watch.
- **Log *text* is asserted only where it is a pure function** (the two identity lines). Everywhere
  else the tests assert the state machine that chooses the level, per the brief.
- **Finding 4 (adsb.lol `Retry-After: 20`)** was explicitly no-code and is untouched; M9-08's adaptive
  cadence landed at `7a8b6a4`.

## Exit checks

All run at the end, whole workspace unless scoped by the brief. No failures in M9-10's in-flight
files, so nothing needed re-running scoped.

```
$ cargo fmt --check
(no output)                                                             exit 0

$ cargo clippy --workspace --all-targets -- -D warnings
    Checking rustak-plugin-example v0.1.0 (…/rustak-plugin-example)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 13.92s   exit 0

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 17.31s
   Generated …/target/doc/rustak_api/index.html and 8 other files          exit 0

$ ./scripts/check-file-length.sh
(no output)                                                             exit 0

$ cargo test -p rustak-client -p rustak-plugin-ais -p rustak-plugin-adsb -p rustak-plugin-example
test result: ok. 262 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.10s
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.31s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
test result: ok. 121 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
test result: ok. 84 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
                                                                        exit 0

$ cargo test -p rustak-server --test feed_sidecars --test services_flow \
      --test sidecar_trust --test sidecar_enrolment --test workload_identity
     Running tests/feed_sidecars.rs
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.40s
     Running tests/services_flow.rs
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.32s
     Running tests/sidecar_enrolment.rs
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.36s
     Running tests/sidecar_trust.rs
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.56s
     Running tests/workload_identity.rs
test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.96s
                                                                        exit 0

$ cargo test -p rustak-server --lib web::
test result: ok. 295 passed; 0 failed; 0 ignored; 0 measured; 1617 filtered out; finished in 9.26s
                                                                        exit 0
```

## Files

Changed:

- `rustak-client/src/http.rs` — `CONNECT_TIMEOUT`, `FEED_IDLE_TIMEOUT`, `feed_client`, shared `build`
- `rustak-client/src/control/mod.rs` — `feed` client slot, `with_feed_http`, `feed_http`, `request_on`
- `rustak-client/src/control/events.rs` — opens through the feed client; module docs; SSE timeout tests
- `rustak-client/src/sidecar/mod.rs` — wires the feed client and `for_account` into the context
- `rustak-client/src/sidecar/control_link.rs` — feed task moved out; shares `link_health`'s helpers
- `rustak-client/src/sidecar/link_health.rs` — `Reopenings`/`Opened`; `note`/`announce`/`recovered`
- `rustak-client/src/sidecar/link.rs` — `settle`, `queued`, `Discards`/`Discarded`, `FIRST_CONNECT`
- `rustak-client/src/sidecar/run.rs` — holds the first drain; first-tick-reaches-the-wire test
- `rustak-client/src/sidecar/workload.rs` — the two identity lines, `unverified_subject`,
  `for_account`, report-once, exchange serialisation
- `rustak-server/src/web/telemetry.rs` — `level_for`, `finished`, module docs, three tests
- `rustak-server/src/web/api/events.rs` — feed-open line demoted to `debug`; `KEEPALIVE` doc
- `rustak-server/tests/feed_sidecars.rs` — one additive test
- `docs/plugins.md`, `docs/deployment.md`

Added:

- `rustak-client/src/sidecar/event_feed.rs`
- `.claude/plan/status/M9-11-control-link-polish.md`

No `git`/`but` writes. No CI files, no `.claude/plan/{plan,backlog}.md`, nothing under
`rustak-server/src/testing/`, `interop/` or `e2e/`.
