# M9-07 — A sidecar must trust each endpoint with the right roots

**Status: complete.** Every deliverable in the brief is implemented, the
coordinator's mid-brief addition (control-link log noise and backoff) is
implemented, and every exit check is green.

The production bug is fixed at its root: there is no longer one client with one
truststore. `rustak-client/src/http.rs` now carries a `Trust` policy that every
call site names explicitly, and the decision itself is a pure function
(`roots_for`) that a unit test asserts on without a TLS backend, a file or a
server.

| Endpoint | Presents | Verified against |
|---|---|---|
| CoT stream (`:8089`) | rustak's internal CA, always | `[service] truststore`, **replacing** the platform's roots |
| `[server] marti` (`:8443`) | rustak's internal CA, always | `[service] truststore`, **replacing** the platform's roots |
| `[server] control` (`:8446`) | ACME / `files` / internal CA | the platform's roots **and** `[service] truststore`, or `[service] control_truststore` alone |

`stream/tls.rs` was confirmed to already implement the replace rule in its own
`RootCertStore` (it refuses to start without a truststore rather than falling
back) and was left alone, as the brief asked.

## The exact configuration key added

One, additive, optional, on the sidecar side only:

```toml
[service]
control_truststore = "/data/public-ca.pem"
```

It **replaces** both the platform's roots and `[service] truststore` for
`[server] control` alone, and says nothing about the stream or Marti. Nothing
writes it. It is deliberately *not* filtered on existence the way `truststore`
is: an operator who pinned the public listener and named a file that is not
there is told so at start-up, because silently falling back to the platform's
roots is the class of bug this whole brief is about.

The same key also reaches `ServiceIdentity` (`with_control_truststore`,
`control_truststore()`, and the redacting `Debug`) and `enroll::Enrolment`
(`control_truststore`, `trust`), so enrolment — the *first* call a deployment
makes — is verified by the same rule as the calls that follow it.

## What the Dublin deployment must change

**Nothing. Pull the new image.**

The pack's `plugin.toml` needs no edit and no new file. The public listener is
holding a Let's Encrypt certificate, which the platform's roots in the image
already cover; the truststore enrolment wrote is rustak's own CA, which now
*joins* those roots for the control endpoint instead of replacing them, and
still replaces them for the CoT stream and Marti, which is what those two
listeners present. `control_truststore` exists for an installation whose
**public** listener is behind a private CA — Dublin's is not, so it should stay
unset.

On the server side, `auth.resolve.bearer`'s instrumented error is demoted from
`error` to `debug`, so the `ERROR auth.resolve.bearer: error=Rejected` line that
appeared immediately *before* each successful workload enrolment is gone. No
behaviour change, no configuration change.

## What landed

| Area | Files |
|---|---|
| Trust policy and the rendered cause chain | `rustak-client/src/http.rs` (`Trust`, `Roots`, `roots_for`, `client(identity, trust, timeout)`, `is_transport`, `transport` rendering, `redacted`) |
| The identity that carries it | `rustak-core/src/service.rs` (additive `control_truststore`) |
| The configuration key | `rustak-client/src/sidecar/config.rs` (`[service] control_truststore`, wired into `identity()`) |
| Enrolment picks its own policy | `rustak-client/src/enroll.rs` (`Enrolment::{trust, control_truststore}`), `rustak-client/src/sidecar/enrolment.rs` (`target()` answers `(String, Trust)`) |
| Call sites, each explicit | `rustak-client/src/marti/client.rs` (`Internal`), `rustak-client/src/control/mod.rs` (`Public`), `rustak-client/src/sidecar/mod.rs` (two clients, not one) |
| Control-link noise and backoff | `rustak-client/src/sidecar/link_health.rs` (**new**), `rustak-client/src/sidecar/control_link.rs`, `rustak-client/src/sidecar/mod.rs` (one `mod` line) |
| Server log level | `rustak-server/src/auth/resolve.rs` (`err(level = "debug", Debug)`, one line) |
| Tests | `rustak-server/tests/sidecar_trust.rs` (**new**, 2 tests), unit tests in `http.rs` (12), `link_health.rs` (12), `service.rs` (1 new), `control_link.rs` (2 new), `enrolment.rs` (1 updated) |
| Test fixtures the signature change touched | `rustak-client/src/sidecar/run.rs`, `rustak-server/tests/feed_support/mod.rs`, `rustak-server/tests/services_flow.rs` |
| Docs | `docs/plugins.md` (new "Which roots verify which endpoint" section with the three-row table, the config sample, the `enroll` example), `docs/deployment.md` ("The sidecar images", "Workload identity"), `rustak-plugin-{adsb,ais,example}/config.example.toml` |

No `git` or `but` command was run. No CI file and no `.claude/plan/{plan,backlog}.md`
was touched.

## Saying what failed

`http::transport` now renders the error's whole source chain, so an operator
reads the cause instead of `reqwest`'s wrapper:

```
Could not register the service 'adsb': error sending request for url
(https://tak.example.com:8446/api/v1/services): client error (Connect):
invalid peer certificate: UnknownIssuer.
```

and the advice gained a third line naming `[service] control_truststore` and the
per-endpoint rule, beside the two that were already there.

**No token and no header can reach that string.** The URL is rebuilt from
`reqwest::Error::url()` with userinfo, query and fragment stripped
(`redacted`), and `Error::without_url()` is called so `reqwest`'s own verbatim
copy of it is never printed; there is a unit test for that. Nothing in the chain
below a request error is a header — it is the connector's error and, at the
bottom, `rustls`'s own reason. The chain is capped at six links and consecutive
duplicates are dropped.

Every path the brief named goes through this one function: the workload token
exchange (`sidecar/workload.rs`), the Marti client, and the control client's
`raw()` — which is what `register`, `post_heartbeat`, `config()`, `config_as()`
and the SSE feed all end in. That last one is the coordinator's extra ask: a
plugin's own configuration read at start (`rustak-plugin-ais`'s "Could not read
this service's configuration") now carries the TLS cause, without either plugin
file being edited.

## The coordinator's addition: one outage, four log lines

82 warnings in 150 seconds became: one `warn` with the full cause chain and its
advice, `debug` for every repeat, one `warn` every five minutes naming how long
it has been failing and the last error on one line, and one `info` when it comes
back naming the duration.

`sidecar/link_health.rs` holds the state machine. It is pure and clock-injected
in `FeedPublisher`'s style — every method has a `*_at(now)` form, and the tests
move `at(seconds)` by hand — behind an `Arc<LinkHealth>` shared between the
harness loop and the feed task, so one link has one notion of being down and one
recovery line. Retries are capped exponential, 1s → 60s, doubling per failure
and reset the moment the server answers. Heartbeats are skipped while the link
is down.

**One judgement call worth recording, because it departs from the letter of the
instruction and is the more correct behaviour.** The instruction was to treat
*every* failed control call as an outage. A failure carrying an HTTP status is
the server *answering*, so `Failure::{Unreachable, Refused}` splits the two,
classified by `http::is_transport`:

- **Unreachable** (handshake, DNS, connect, timeout) — the link is down: the
  backoff advances and other calls are held back.
- **Refused** (`401`, `404`, `409`, `503`) — the link is **up**: the message is
  still throttled exactly the same way, but nothing else is held back and
  nothing is backed off.

Without that split, a server whose `/api/v1/events` route answered `404` — an
older rustak, a proxy with a path prefix — would have silently stopped a
perfectly healthy sidecar from heartbeating at all. That regression showed up
immediately as a pre-existing test failure
(`run.rs::the_loop_reports_what_the_health_hook_answers…`, 0 heartbeats instead
of 3) rather than in production, which is the argument for the split in one
sentence.

The second, smaller departure: heartbeats are skipped while down **until the
backoff falls due**, rather than never. A heartbeat that is allowed through once
per backoff window is what finds out the link is back. Skipping them entirely
would have left recovery detection resting solely on the feed task, and a
deployment whose feed route is the broken one would then never recover. The
per-tick noise the instruction was aimed at is gone either way: at the default
30s tick and a 60s ceiling, a long outage costs roughly one attempt per minute
and one log line per five.

`is_transport` matches on the advice rather than on the message, because the
message names whatever the caller was doing and the advice survives a caller
wrapping the error with its own (`enrolment::ensure` does exactly that); there
is a unit test for the wrapped case.

The plugin-visible API did not change. `ControlLink` is `pub(crate)`,
`LinkHealth` is private to `sidecar`, and every method still answers `()`.

## Tests

**Unit — the policy decision, per call site** (`http.rs`): `Internal` replaces;
`Public` joins; `control_truststore` replaces for `Public` and is invisible to
`Internal`; no truststore is the platform's roots under both; both policies
build a real client from a real PEM (`tls_certs_merge` is a different code path
in `reqwest` from `tls_certs_only` and errors on a platform whose verifier
cannot take extra roots — it must not be one of ours); a missing file under
either key is the operator's to fix.

**Unit — the rendered chain** (`http.rs`): a real `rustls` server on an
ephemeral port presenting a certificate from an authority the client does not
hold; the rendered error contains `UnknownIssuer`, names what was being
attempted, and its advice names `control_truststore`. Plus the redaction test,
and the two `is_transport` classification tests.

**Unit — the outage state machine** (`link_health.rs`, 12 tests): down → still
down (quiet) → reminder at 300s → quiet → reminder at 600s → up, with the
duration in the recovery; a second outage is announced again; a refusal is
throttled but leaves the link up and backs nothing off; a server that answers
again ends the outage even by refusing; the backoff doubles 1→60 and stops, and
does not overflow after 80 failures; a success resets it; nothing is attempted
inside the wait; a clock that stepped backwards is not a negative outage.

**Unit — `ControlLink`** (2 new): a `503` leaves the link up (`down: false`); a
server that cannot be reached at all takes it down and the calls inside the
backoff are skipped.

**Integration** (`rustak-server/tests/sidecar_trust.rs`, new): the public route
tree bound over **TLS** on a certificate from a second test authority (the
"public" CA, standing in for Let's Encrypt) while the CoT stream keeps the
internal one. A sidecar given `control_truststore` = that public CA enrols
(before it has any truststore), writes the three PEMs, connects the stream
against rustak's *internal* CA, and then registers, heartbeats and subscribes
the event feed — all three of the calls that failed forever in Dublin. The
counter-proof is in the same test: a client built the old way, `tls_certs_only`
with the enrolled truststore, cannot open a connection to the public listener
and the rendered error says `invalid peer certificate`; the `Public` policy
against the same listener succeeds. A second test shows the failure as a
*start-up* failure — no pin, no covering roots — and asserts that the cause and
the `control_truststore` advice both survive to the fatal start-up line.

## What I could not verify

- **The platform-roots half of the `Public` policy, end to end.** A test cannot
  add a root to the platform's store, and `std::env::set_var` (for
  `SSL_CERT_FILE`) is unsafe under the workspace's `unsafe_code = "forbid"`. So
  the integration suite proves the *replacing* half (`control_truststore`) and
  the fact that the control endpoint is verified against a different set of
  roots from the stream; the *joining* half — platform roots **plus**
  `[service] truststore` — rests on the pure-function unit tests on `roots_for`,
  on `reqwest`'s `tls_certs_merge` being exercised for real in
  `both_policies_build_a_client_from_a_real_truststore`, and on the deployment.
  This is exactly the split the brief anticipated, and it is written into the
  new test file's module documentation as well.
- **`reqwest::ClientBuilder::tls_certs_merge` on non-Unix/Windows targets.**
  `reqwest` returns a builder error where `rustls-platform-verifier` cannot take
  extra roots. The images are Linux and development is macOS, both covered; the
  unit test above would catch a regression on either.
- **The `err(level = "debug")` demotion in a running server.** It is a one-line
  attribute change with no behaviour attached; the existing `auth.resolve.bearer`
  tests still pass, but nothing asserts on a log *level*.
- **`auth.resolve.cert` carries the same `err(Debug)`**, and I checked it as the
  brief asked. I did **not** change it, for two reasons: it is not in the files
  this brief owns (`resolve.rs`, the log level only), and unlike `bearer` it is
  not tried speculatively — `resolve_principal` returns its result directly, so
  a refusal there refuses the request. It is still arguably too loud for a
  client-caused rejection (an unknown or revoked certificate would log at
  `error`), and is worth a one-line follow-up brief.
- **`--check` does not validate `control_truststore`.** A pin naming a file that
  is not there is caught at the first start rather than by `--check`, because
  `check()` reads no files. A deliberate small gap, consistent with how the
  other `[service]` paths are handled for a not-yet-enrolled deployment; worth a
  follow-up if pipelines are expected to catch it.

## Exit checks

```
$ cargo fmt --check --all
exit: 0

$ ./scripts/check-file-length.sh
exit: 0

$ cargo clippy --workspace --all-targets -- -D warnings
    Checking rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.16s

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.82s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 8 other files

$ cargo test --workspace
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
all doctests ran in 1.64s; merged doctests compilation took 1.20s

  (totalled across every binary in the run: 3322 passed, 0 failed, 2 ignored)

  Running tests/sidecar_trust.rs
  running 2 tests
  test a_sidecar_trusts_the_public_listener_and_the_stream_with_different_roots ... ok
  test a_sidecar_that_trusts_only_the_internal_ca_cannot_reach_the_public_listener ... ok
```

The new integration suite runs under a plain `cargo test --workspace` (the same
command CI uses) rather than needing `--features testing` on the command line,
because `rustak-server`'s dev-dependencies pull the feature in.
