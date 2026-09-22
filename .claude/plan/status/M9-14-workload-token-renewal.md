# M9-14 — A sidecar's workload identity must survive its own renewal

**Done.** The control link no longer dies two hours after a start: a file source
is preferred over the environment, re-read and expiry-checked on every use, a
refused credential backs off instead of hammering, and both ends now say which
check failed and by how much.

## What the production logs actually say

Read from Loki (`{service_name=~"rustak.*"}`, 2026-09-22, times UTC) rather than
reasoned about. This is the whole defect, in seven lines:

| Time | Who | Line |
|---|---|---|
| 04:19:29 | adsb | `Identity: authenticating to the control API as 'adsb' with the workload identity from NOMAD_TOKEN_rustak.` |
| 05:18:29 | rustak | `/oauth/token` → `A workload identity authenticated … account=adsb` — **exchange #2, 59m00s after the start** |
| 05:21:30 | rustak | `error=Rejected` + `Ending a server-event feed whose caller is no longer authorized.` — 1h02m after the start |
| 06:17:34 | rustak | `ERROR … auth.workload.verify: error=Claims`, inside the `request{… http.headers=…}` span — 1h58m05s after the start |
| 06:17:34 | adsb | `Could not exchange this sidecar's workload identity for an access token…`, then a reminder every 5m for 45m, retrying every ~1–5s throughout |
| 07:02:59 | adsb | restarted with `env = false`; first exchange refused **429 Too Many Requests** — no `/oauth/token` line on the server at all |
| 07:03:28 | adsb | `Identity: … from /secrets/nomad_rustak.jwt.` + `The control link is back after 29s.` |

### How the first access-token expiry was survived (deliverable 5)

**By the 60-second renewal margin, and by nothing else. No expired assertion was
ever accepted.**

`RENEW_BEFORE` is 60 s: the client discards its access token one minute before
it expires. `[auth] access_token_ttl` is 1 h and the Nomad identity's `ttl` is
1 h, and both were minted within a second of each other, so the second exchange
falls in the **last minute of the workload JWT's life** — at 05:18:29, exactly
59m00s after the 04:19:29 start, with the assertion still ~60 s from its `exp`.
rustak accepted it because it was valid. That bought an access token good until
06:18:29, whose own renewal at 06:17:29 presented an assertion that had expired
58 minutes earlier and was correctly refused — which is the "about two hours"
of the brief, and the `expired 58m ago` in it.

The single `error=Rejected` at ~1h02m is a different thing and also correct: the
**access token** issued at 04:19:29 expired at 05:19:29, and the long-lived
event feed opened with it was cut when the server re-checked its caller
(`Ending a server-event feed whose caller is no longer authorized.`). The
sidecar reopened the feed with the token bought at 05:18:29, which is why there
was no sidecar-side warning.

So: **no security bug**. rustak refused every expired assertion it was shown.
The tests now hold it to that in both places it can be shown one — see
*Tests* below, including the `clock_skew` boundary.

### Why the clean restart's first exchange failed (coordinator item 1)

**The rate limiter, not a truncated file and not a clock.** The previous process
spent 45 minutes presenting an expired assertion every 1–5 s (a `Refused`
failure reset `LinkHealth`'s backoff, so there was none), and each refusal
called `limiter.record_failure(address, "workload-identity")`. Ten failures in a
minute is a fifteen-minute lockout on that `(address, subject)` pair, so the
**next process's very first exchange was refused before it was looked at** — the
sidecar's own log says `429 Too Many Requests`, not 400, and its advice line was
"Too many attempts. Wait for the lockout to pass and try again." The link
recovered 29 s later when the lockout expired.

That is also why rustak logged no `auth.workload.verify` line: the limiter
answers ahead of the verifier and emitted **no event at all**. The two `ERROR`
lines at 07:02:59 were `plugins.auth: error=Rejected` for
`GET /api/v1/services/{adsb,ais}/config` — the plugins' configuration read going
out while the harness had no credential, which is coordinator item 2.

Both halves are fixed: the credential backoff (5 s → 5 min) means a refused
exchange can no longer earn a lockout, and a rate-limited credential now
produces one throttled `warn` on the server instead of silence.

## The detection order, as implemented

`[service] workload_identity` still names a source outright. With nothing set,
`workload::probe` tries, in this order:

1. `${NOMAD_SECRETS_DIR}/nomad_rustak.jwt` — Nomad, `file = true`
2. `/var/run/secrets/tokens/rustak` — a Kubernetes projected `serviceAccountToken`
3. `NOMAD_TOKEN_rustak` — **only when neither file exists**

Every file beats the environment, because both orchestrators renew by rewriting
the file and the environment variable is frozen at process start. An `env`
source — configured or auto-detected — is honoured and produces one start-up
`warn` naming the file alternative and `env = false, file = true`.

## What an operator sees now

Sidecar, start-up:

```text
INFO Identity: authenticating to the control API as 'adsb' with the workload identity from
     /secrets/nomad_rustak.jwt (expires 2026-09-22T05:19:27Z, renewed by the orchestrator).
```

…or, from the environment:

```text
INFO Identity: … from NOMAD_TOKEN_rustak (expires …, which an environment variable cannot have renewed).
WARN This sidecar's workload identity comes from 'NOMAD_TOKEN_rustak', an environment variable, which is
     frozen when the process starts. … Set `env = false, file = true` in the Nomad identity block …
```

Sidecar, credential refused (one warn, debug repeats, a counted reminder every
five minutes, one info on recovery):

```text
WARN Could not exchange this sidecar's workload identity for an access token. The server answered, so the
     control link is up; it is the credential that was refused. The next attempt is in 5s, widening to
     5m00s; repeats are logged at debug until this changes.
     error=The workload identity this sidecar presented expired 58m00s ago; its source NOMAD_TOKEN_rustak
     is an environment variable, which the orchestrator cannot renew. The server refused it
     (400 Bad Request): exp: expired 58m12s ago.
WARN This sidecar's workload identity has been refused for 5m00s over 7 attempts. error=…
INFO The server accepted this sidecar's workload identity again after 29s.
```

Server administrator — `warn`, never `ERROR`, never inside the request-header
span (`parent: None`), once per `(issuer, subject, reason)` per five minutes:

```text
WARN Refused a workload identity: exp: expired 58m12s ago. Repeats of this one are logged at debug, with
     a count every five minutes. issuer=https://nomad.example.com
     subject=global:default:rustak-plugin-ais:sidecar:ais:rustak reason=exp
WARN Refused a workload identity 58 more times in the last five minutes: exp: expired 58m12s ago.
WARN Refused a workload identity without checking it: too many attempts from this address, for another
     29s. Something is presenting a credential this server will not take, over and over.
```

A credential that was never a workload assertion is **not** one of these. An
ATAK client presenting an ordinary enrolment token on `/Marti/api/tls/**` would
otherwise earn a warning apiece for not being a JWT, so `Presented::Perhaps`
(the enrolment route, which takes other credentials too) keeps a malformed or
unknown-issuer refusal at `debug` and lets the request carry on, while
`Presented::Deliberately` (`grant_type=jwt-bearer`, which can only have meant
one thing) warns about everything.

Other reasons render as `nbf: not valid for another 40s`,
`aud: expected "rustak", token names ["vault"]`, `iat: issued … in the future`,
`kid "k9" is not published by issuer nomad`, and a signature failure gets no
detail at all (the bytes are not the orchestrator's). The same sentence is the
`error_description` of the unchanged `400 {"error":"invalid_grant"}`. Audit
entries are untouched.

## The other two coordinator items

**(2) The configuration read that never happened again.** Both feed plugins read
`GET /api/v1/services/<name>/config` once, in `start`, and a read that failed
there left an administrator's `area` override silently not in effect for the
life of the process. The retrying now lives in one place —
`rustak_client::sidecar::ServiceSettings`, which both plugins hold — and it:

- never provokes a token exchange of its own (it uses the credential the
  harness is already holding, and waits for the next tick when there is none),
  so a plugin cannot add a second refusal to one cause;
- retries 30 s after a failed read and re-reads every 5 minutes after a
  successful one, which is also how a change an administrator makes now reaches
  a running sidecar without a restart;
- answers a document only when it has *changed*, so nothing is applied or
  logged twice;
- is `debug` before the first success (a start-up where the link is not up yet
  is not news) and one `warn` for a read that fails after one has worked.

Both plugins apply an `area` that differs from the one in effect — reopening the
source, because a feed subscribes with its area — and log one line naming the
area and where it came from:

```text
INFO The ADS-B sidecar is watching. … area=Circle{…} area_from="an administrator, through the control API"
INFO The area an administrator set for this service is now in effect. area=… area_from=…
```

**(3) The doubled full stop.** `Could not read this service's configuration:
That credential is not one this server accepts for the control API..` came from
`rustak-client/src/control/mod.rs`'s `refused`, which appended a full stop to a
detail that was already a sentence. Fixed at the source as `http::full_stop`,
used by all three renderers that embed the server's own words
(`control::refused`, `marti::refused`, `http::transport`), with two tests on the
renderer: a detail that already ends is not given another, and one that does not
is.

## What the deployment must change

**Nothing beyond `env = false`, which it has already made.** The reference
jobspec in `docs/deployment.md` now reads
`identity { name = "rustak", aud = ["rustak"], ttl = "1h", file = true, env = false, change_mode = "noop" }`.
Leaving `env = true` in place is no longer fatal — the file wins — so a
deployment that has not been updated is fixed by this change alone, on the next
sidecar image. No server configuration changes; no migration; no restart beyond
the ordinary one that picks up the new binaries.

## Tests

Client (clocks injected, no sleeps, nothing asserts an elapsed duration):

- `sidecar::assertion` — `exp`/`iss` read unverified; an unreadable token is not
  fresh; the 60-second staleness margin; a token being rewritten is read again
  and the newer one presented; a file that is not there yet is read again; an
  expired token with nothing better is still presented.
- `sidecar::workload` — the file beats the environment when both exist; the
  environment is last and only without a file; only a file is renewable; the
  `env` warning's wording; the identity line with source + expiry, for a file,
  for the environment, and for an expiry we cannot read.
- `sidecar::credential` — 5 s → 5 min, doubling, capped; first warn, quiet
  repeats, a counted reminder at five minutes; acceptance clears the run; a
  replaced credential (different fingerprint) is tried at once; a fingerprint
  carries none of the token.
- `sidecar::exchange` — the refusal names the expiry first, then the source,
  then the server's `error_description`, and contains no `eyJ`; an environment
  source says the environment cannot be renewed and advises `env = false`; a
  refused exchange is exchanged once per backoff window (five calls, one
  request); a rewritten file is presented at once; a transport failure is the
  link's problem and leaves the credential's backoff alone; the presented
  assertion changes when the file does.
- `sidecar::settings` — a document is applied once and not again until it
  changes; a failed read is retried on a cadence rather than abandoned for the
  life of the process; an unusable setting leaves the one in effect alone.
- `control` — the doubled full stop: a server sentence that already ends is not
  given another, and one that does not is.

Server:

- `auth::workload::verify` — each refusal renders its own sentence
  (`exp: expired 58m12s ago`, `nbf: not valid for another 40s`,
  `aud: expected "rustak", token names ["vault", "consul"]`, a missing claim, a
  signature with no detail), none of them containing the token; a hostile claim
  name cannot carry a log line away (control characters stripped, length
  capped); `iat` refusals name themselves.
- `auth::workload::refusals` — the cadence as a state machine (first, repeats,
  a counted reminder, quiet again), a second deployment still heard over the
  first, the map bounded and swept, and the issuer/subject tidied.
- `auth::workload` — every refusal has a word of its own; every sentence names
  no account; only a refused *credential* is described back to its holder; and
  a credential that was never a workload assertion is not a warning on a route
  that takes other credentials.
- `tests/workload_identity.rs` — an expired assertion is refused on **both**
  routes that take one, with `error_description` starting `exp: expired 1h` and
  carrying no token; the `clock_skew` allowance is a boundary and not a
  door; and a sidecar whose token file is renewed mid-run keeps its control link
  across an access-token expiry (`access_token_ttl = 30s`, so every call
  exchanges again; the file's content is what moves).

### How each new test behaves on a host ten times slower

Every one of them passes unchanged, because none measures elapsed time.

- **Clock-injected tests** (`assertion`, `credential`, `refusals`, `settings`,
  the `workload` renderers, `exchange`'s backoff tests) take the instant as an
  argument. A slower host changes nothing at all: `at(0)`, `at(300)` and the
  fingerprints are the same values whenever they are evaluated.
- **Counting tests** (`five calls, one request`, `four callers, one exchange`)
  assert the number and order of HTTP requests a mock server received. A slower
  host makes them slower, not different.
- **Content-driven tests** (the file rewritten between two exchanges, the
  renewal in `tests/workload_identity.rs`) are driven by writing a file, never
  by waiting for one.
- **The one place a real duration appears** is `exchange`'s `SETTLE` (500 ms
  between the two reads of a token that is not worth presenting), which tests
  set to `Duration::ZERO`. Nothing asserts how long it took.
- **`the_clock_skew_allowance_is_a_boundary_and_not_a_door`** deliberately uses
  a ten-minute `clock_skew` and tokens 5 and 15 minutes past `exp` rather than
  the brief's `± 1 s` around the 30-second default. A one-second margin would
  assert that this host verified the token within a second of minting it, which
  is exactly the host-dependent upper bound the rule forbids; ten minutes puts
  both assertions on the same side of the boundary on a host a hundred times
  slower while testing the same property.
- **`an_expired_assertion_is_refused_wherever_it_is_presented`** asserts the
  `error_description` starts `exp: expired 1h` and ends ` ago`, not
  `1h00m ago`: pinning the minutes would assert that this host got from minting
  the token to verifying it inside a minute. The check name, the unit and the
  magnitude are what the test is for.
- **`a_sidecar_whose_token_file_is_renewed_keeps_its_control_link`** shortens
  `access_token_ttl` to 30 s — *below* the client's 60-second renewal margin —
  so every call exchanges again with no waiting at all. A slower host simply
  takes longer to make three HTTP calls.

## Exit checks

All five run on the final tree, in order, from a clean invocation.

```
$ cargo fmt --all --check
(no output; exit 0)

$ ./scripts/check-file-length.sh
(no output; exit 0)

$ cargo clippy --workspace --all-targets -- -D warnings
    Checking rustak-plugin-example v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-plugin-example)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 38.80s
(exit 0)

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 34.50s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 9 other files
(exit 0)

$ cargo test --workspace --no-fail-fast
    test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.59s   (workload_identity)
    all doctests ran in 2.50s; merged doctests compilation took 2.06s
(exit 0 — 60 result blocks, 3602 tests, 0 failed)
```

One assertion in `tests/workload_identity.rs` was relaxed after that run (the
`1h00m` → `1h` change described above); that binary was re-run on its own
afterwards: `test result: ok. 24 passed; 0 failed`, and `cargo fmt --all --check`
is still clean.

## Files

New:

- `rustak-client/src/sidecar/assertion.rs` — reading this sidecar's own
  assertion: `exp`, `iss`, freshness, and the read-twice-on-a-renewal rule.
- `rustak-client/src/sidecar/credential.rs` — whether the server is taking the
  credential: the 5 s → 5 min backoff, its log cadence, and the fingerprint that
  resets it when the orchestrator replaces the token.
- `rustak-client/src/sidecar/exchange.rs` — `AccessTokens`, moved out of
  `workload.rs` (which would otherwise be over the 300-line limit) and given the
  fresh read, the backoff and the richer refusal.
- `rustak-client/src/sidecar/settings.rs` — `ServiceSettings`, the retrying
  service-configuration read both feed plugins now use.
- `rustak-server/src/auth/workload/refusals.rs` — the throttled, span-detached
  `warn` that replaces `err(Debug)`-at-`ERROR`.

Changed:

- `rustak-client/src/sidecar/{workload,control_link,event_feed,mod}.rs`
- `rustak-client/src/{http.rs,control/mod.rs,marti/mod.rs}` — the doubled full
  stop, fixed once in `http::full_stop` and used by all three renderers.
- `rustak-server/src/auth/workload/{mod,verify,grant,request,rules}.rs`
- `rustak-server/src/{plugins/auth.rs,auth/basic.rs,identity/verify.rs}` — the
  same `err(level = "debug", Debug)` treatment for the refusal paths that were
  logging a caller's mistake at `ERROR` with the request's headers.
- `rustak-plugin-{ais,adsb}/src/lib.rs` — the configuration read retries, and an
  `area` an administrator sets is applied when it differs from the one in
  effect, with one `info` naming the area and where it came from.
- `rustak-server/tests/workload_identity.rs`
- `docs/deployment.md`, `docs/plugins.md`,
  `rustak-plugin-{ais,adsb,esb,example}/config.example.toml`

`docs/ci.md` says `workload_identity` is "22 tests, 170–215 s"; it is 24 now.
Left alone deliberately — it is the CI steward's file and the brief forbids CI
edits — but the number is stale.

`.claude/plan/compat/enrollment.md` does not state the detection order, so it
needed no change. `rustak-plugin-{esb,example}/config.example.toml` are outside
the brief's list but carry the same paragraph verbatim; leaving them saying
"environment first" would have been a documented bug.

## What I could not verify

- **Nomad's renewal on this cluster.** Everything about the file being rewritten
  at half the TTL is Nomad's documented behaviour plus the production evidence
  that the *file*-sourced restart is still healthy; I did not watch a file
  change on the host.
- **The 500 ms `SETTLE`.** No production sample of a half-written
  `nomad_rustak.jwt` exists — the clean restart's first refusal turned out to be
  the rate limiter, not a truncated read — so the read-twice path is proved by
  its unit tests rather than by a reproduction. It is harmless when nothing is
  wrong: a token that is fresh is read once and never waits.
- **The lockout arithmetic.** The 29-second recovery is consistent with a
  15-minute lockout that began during the previous process, but the limiter's
  in-memory state is not observable after the fact.
