# M10-03 — Sidecar link messages that say what happened

**Status: complete.** All four deliverables are in and every exit check passes (results below).

## 1. Advice that fits the failure

`rustak-client/src/http.rs` was 209 functional lines, and the classifier would have pushed it past the limit, so the transport-error half moved into a new submodule, **`rustak-client/src/http/failure.rs`** (182 functional lines). `http.rs` declares `mod failure;` and re-exports `pub use failure::{is_transport, transport};`, so every caller (`control`, `marti`, `enroll`, `exchange`, `rustak-server/tests/sidecar_trust.rs`) is unchanged.

- `failure::Class` has nine variants: `Resolve`, `Refused`, `ConnectTimeout`, `Unreachable`, `Tls`, `Timeout` (connected, but the answer came too late), `Status`, `Body` (the body broke off or would not decode) and `Other`. Each has its own advice slice. Only `Tls` mentions `[service] truststore` / `control_truststore`, and it keeps the existing wording that names the way out.
- `classify(&dyn Error)` walks the cause chain with typed checks:
  - `rustls::Error` anywhere in the chain means `Tls`.
  - Then `reqwest::Error::is_status`, `is_body`/`is_decode`.
  - If the error happened while connecting (`is_connect`, or a chain that did not come through reqwest), it checks `is_dns`, then the `io::ErrorKind` (ConnectionRefused / TimedOut / HostUnreachable / NetworkUnreachable / NetworkDown / AddrNotAvailable) or `tokio::time::error::Elapsed`, then `is_timeout`.
  - If not connecting, `is_timeout` means `Timeout`.
- **Finding:** reqwest 0.13 reports a handshake failure as `io::Error(Other, io::Error(InvalidData, rustls::Error))`, and `io::Error::source` skips the error it carries. The walk therefore also looks inside each `io::Error` (`get_ref`). Without this, the real untrusted-server test classed a TLS failure as `Other`.
- One check is textual: a hand-built chain's resolver error. The resolver error types are private to reqwest and hyper-util, so the text `dns error` / `error resolving DNS` is matched. A real `reqwest::Error` uses the typed `is_dns()`.
- `is_transport` used to match the first line of one shared advice slice. It now matches the first line of any *outage* class. Every class leads with its own line, and a test enforces that. **Decision:** `Status` is the only class that is not an outage. It is the server answering, the same rule `LinkHealth` already applies. `Body`, `Timeout` and `Other` still count as outages, which is what every transport error counted as before. No caller passes a status error to `transport` today, so link-health behaviour is unchanged.

Tests:
- Hand-built chains: the Dublin `tcp connect error: deadline has elapsed`, each io kind, DNS, and a nested-io `rustls` error.
- Only TLS advice mentions a truststore, and advice markers are unique.
- `is_transport` for statuses, outages and wrapped errors.
- Real, deterministic failures: a refused loopback port (bound, then released), a wiremock 503 through `error_for_status`, a wiremock non-JSON body through `.json()`, and a raw listener that promises 100 bytes and sends 5.
- The existing untrusted-TLS-server test now also asserts `Class::Tls` and `is_transport`.

## 2. The counted-reopen warning no longer fires for an outage

`sidecar/link_health.rs`:

- `LinkHealth`'s state now counts outages. The counter goes up on each up→down transition. `LinkHealth::link()` returns a `Link { down, outages }` snapshot.
- `Reopenings` was rewritten.
  - `closed(link, now)` holds a close as *pending*. It is dropped if the link is down or an outage has started since the last look.
  - `failed(link)` records a failed opening. It is **never** a close. If the link is down or an outage has started, it drops the pending close and empties the window.
  - `opened(recovered, link, now)` counts the pending close only if no outage started between the close and this opening. A recovery, or an outage the heartbeat saw end first, empties the window: the churn window resets when the link recovers.
  - "The feed's own transport being down" is detected the only way the feed task can see it. `control/events.rs` (not mine) turns a body error into a plain end-of-stream, so the task learns it from the reopening. If the reopening fails with an unreachable server, `LinkHealth` goes down, the outage counter moves, and the pending close is discarded.
- `sidecar/event_feed.rs`:
  - calls `reopenings.closed(health.link(), …)` after a stream ends;
  - calls `reopenings.failed(health.link())` after a failed opening (replacing the old `closed()` that counted it) and after a failed token exchange;
  - passes `health.link()` to `opened`.

Tests (injected clock, pure; nothing waits):
- `a_server_restart_is_one_outage_notice_and_no_churn_warning`: six failed reopenings give exactly one `Report::First`, the rest are `Quiet`, and the reopening after recovery is `Announce`, never `Churning`.
- `a_restart_the_heartbeat_saw_end_first_is_still_not_churn`: the exact 2026-09-22 shape. Five healthy closes, then an outage, then the heartbeat recovers first. The reopening is `Quiet`, and the window starts again from zero.
- `a_close_the_heartbeat_found_the_link_down_for_is_the_outage`
- `a_close_while_the_link_is_down_is_not_counted`
- `a_healthy_feed_that_keeps_being_cut_is_mentioned_once_with_a_count`: six healthy closes give `Churning { closes: 6 }` once, and six more inside the same window stay `Quiet`.
- The existing feed tests were adapted to the new signatures.

## 3. `--check` validates `[service] control_truststore`

- New `http::truststore(path, setting)` (public). It replaces the private `certificates()` and refuses the file with a message naming `[service] <setting>`:
  - `…, which does not exist.` for NotFound;
  - `…, which could not be read (…)` for any other io error;
  - `…, which is not a PEM bundle we can read (…)`;
  - `…, which holds no certificates.`
  
  Each comes with `ADVICE_TRUSTSTORE`. `build()` uses it too and names `control_truststore` when that is the file being read, so start-up now names the key as well.
- New `ServiceConfig::check_control_truststore()` in `sidecar/config.rs`.
- `enrolment::check` calls it first, before the "already enrolled" early return and before the pre-enrolment path stripping. **Decision:** it is validated whenever it is set, even without `[server] control`, because nothing ever writes that file and M9-07 deliberately does not filter it on existence. Before this change, `--check` only caught it indirectly: `SidecarContext::from_config` builds the public client only when `[server] control` is set, and the message did not name the key.
- Tests in `config.rs`:
  - does not exist;
  - cannot be read (a directory, so the test does not depend on running as non-root);
  - holds no certificates;
  - a usable file, or none at all, passes.
  
  One more test in `enrolment.rs` proves `check` refuses a missing one on a pre-enrolment file.

## 4. Docs

`docs/plugins.md`:
- The `--check` bullet now says it writes nothing (it does read the files it names) and refuses a bad `control_truststore` by name.
- The feed-message table now says "closes of a healthy feed". A paragraph after it covers what is not counted and when the count resets, and a second paragraph says advice follows the failure's cause, with truststore hints only for TLS.

## First-connect wording (M9-15)

Confirmed: `rustak-client/src/stream/reconnect.rs:74`, `const NEVER_CONNECTED: &str = "Could not connect to the TAK stream yet; retrying.";`. Not edited.

## Files changed / added

- `rustak-client/src/http.rs` (modified: failure half moved out, `truststore()` public and names the key)
- `rustak-client/src/http/failure.rs` (**new**)
- `rustak-client/src/sidecar/link_health.rs`
- `rustak-client/src/sidecar/event_feed.rs`
- `rustak-client/src/sidecar/config.rs`
- `rustak-client/src/sidecar/enrolment.rs`: two lines in `check` plus one test. This is the file that implements `--check`'s identity half; `run.rs` was not touched.
- `docs/plugins.md`
- `.claude/plan/status/M10-03-sidecar-link-polish.md` (this note)

## On a host ten times slower

- Every `link_health` test runs on a hand-moved clock and waits for nothing.
- The classifier tests on hand-built chains are pure.
- The real-failure tests assert classes, not durations:
  - the refused port fails at once whatever the speed;
  - the wiremock tests have no timeout;
  - the truncated-body server finishes its write before closing.
- The existing TLS test's only bound is `DEFAULT_TIMEOUT` (30 s), a hang guard.

A ten-times-slower host changes nothing but wall time.

## Open

- `scripts/check-file-length.sh` reads `git ls-files`, so it skips the untracked `http/failure.rs` until the orchestrator commits it. I counted by hand with the script's awk rule: 182 functional lines.
- `rustak-server/tests/sidecar_trust.rs` calls `http::transport` and asserts on the rendered description, which is unchanged. I did not run the `rustak-server` suite, per the rules (the crate was not changed).
- `Body`, `Timeout` and `Other` stay outages for `is_transport`, as before. If the harness should treat a response that broke off as "the server answered", that is a one-line change to `OUTAGES`.
