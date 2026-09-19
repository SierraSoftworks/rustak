# M2-15 — ATAK's real enrolment sequence — complete

Brief: `.claude/plan/briefs/M2-15-atak-enrolment-sequence.md`
Field report (written first, item 5): `.claude/plan/status/M2-15-field-report.md`
Read first: `conventions.md`; research `07` §1–§2; `compat/{enrollment,profiles}.md`; status
M2-03 (the enrolment endpoints), M5-02 (the atomic claim), M3-02 (device profiles), M1-00
(why the commoncommo harness never saw this).

ATAK enrols in three Basic-authenticated calls with **one** credential. rustak spends the
one-time token on the second and answered the third `401`, so the first real device to meet a
production rustak reported "TAK server registration failed". The fix is a grace window scoped
to the one route, the one device and the one window; everything else a spent token could do is
still refused, and each refusal has a test.

## What was built

| File | Functional lines | What changed |
|---|---:|---|
| `rustak-server/migrations/0016_credential_spend.sql` | — | `credentials.spent_at`, `credentials.spent_uid`, a partial index |
| `rustak-server/src/db/repos/credentials.rs` | 245 (was 260) | the two columns on the row; the revocations clear the spend; the single-use lifecycle moved out |
| `rustak-server/src/db/repos/credentials/spend.rs` | 74 (new) | `claim_single_use` (now taking the uid), `release_single_use`, `find_by_hint_including_spent`, `clear` |
| `rustak-server/src/identity/verify.rs` | 177 (was 136) | `Purpose::EnrollmentProfile`, `Grace`, `verify_with_grace`, `usable` takes the grace |
| `rustak-server/src/identity/credentials.rs` | 237 (was 233) | `claim_single_use(…, client_uid, …)`; the one-time-use warning demoted to `debug` |
| `rustak-server/src/auth/basic.rs` | 96 (was 79) | `verify_basic_with_grace`; no `record_use` call for a single-use credential |
| `rustak-server/src/auth/resolve.rs` | 244 (was 216) | `ENROLLMENT_PROFILE_PREFIX`, `enrollment_purpose`, `grace_uid`, `client_uid`; `resolve_principal` builds the grace |
| `rustak-server/src/marti/enroll.rs` | 183 (was 182) | the claim carries `query.client_uid` |
| `rustak-server/src/marti/tls.rs` | 201 (unchanged) | module docs: the third call |
| `rustak-server/src/marti/profiles.rs` | 291 (unchanged) | module docs: what a route added under `/tls/profile/` inherits |
| `rustak-server/src/config/auth.rs` | 215 (was 205) | `[auth] enrollment_grace`, default 10 minutes |
| `rustak-server/src/pki/facade.rs` | 219 (unchanged) | one test: the verifier a listener holds accepts a certificate issued a moment ago |
| `rustak-server/src/stream/listener_tls.rs` | 182 (was 165) | `refusal()`: a refused handshake is `info` with a reason rather than `debug` |
| `config.example.toml` | — | `enrollment_grace` documented with its default |
| `rustak-server/tests/enroll_flows.rs` | — | the three-call contract test and three negatives |
| `interop/node-tak/tests/enrollment.test.ts` | — | the profile fetch with the credential that enrolled |
| `interop/node-tak/src/surfaces.ts` | — | the `enrollmentProfile` surface |
| `.claude/plan/compat/enrollment.md` | — | §4a, a gotcha, the verification sources |
| `docs/compat/atak.md` | — | §1 step 5 rewritten around the third call |

`db/repos/credentials.rs` reached 307 functional lines with the spend on it, over
`conventions.md`'s limit, so the single-use lifecycle moved into a child module —
the same split `certificates.rs`/`certificates/list.rs` already uses, and the
responsibility falls cleanly: the parent is every credential's lifecycle, the
child is what happens to one that can only be used once.

## 1. The grace window

**The shape.** A spent enrolment token is accepted on `GET /Marti/api/tls/profile/enrollment`
and `GET /Marti/api/tls/profile/tool/**`, for the `clientUid` recorded with the spend, until
`spent_at + [auth] enrollment_grace` (10 minutes). Nothing else: not a second `signClient`, not
`tls/config`, not `/oauth/token`, not the Marti API, not another device, not after the window,
and not after a revocation.

**Why it needed columns.** `revoked_at` cannot answer either half of the question. The claim
writes it as it spends the token (M5-02: the consumption *is* the gate), so it says when the
row died but not whether it died by being spent or by an administrator taking it back — and it
says nothing about which device spent it. `0016` adds `spent_at` and `spent_uid`, written only
by `claim_single_use`, cleared by `release_single_use` (an issuance that then failed) and by
both revocation paths. That last one is what keeps an administrator's revocation from being
overridden by a window it cannot see: `revoke` now runs a second, unconditional statement,
because the first one changes nothing on a row that is already spent.

**Why the refusal said `BadSecret`.** `find_by_hint` filters `revoked_at IS NULL`, so a spent
token is not a candidate row at all and the refusal is indistinguishable from a wrong secret —
which is exactly what the field log showed. The grace path asks a second query,
`find_by_hint_including_spent(hint, now - window)`, and only that path can reach those rows.

**Where the decision lives.** Three gates, each in the place that owns the fact:

- `ListenerAuthPolicy::basic_purpose` maps a path to a `Purpose`, and now splits the enrolment
  prefix: `/Marti/api/tls/profile/` is `Purpose::EnrollmentProfile`, everything else under
  `/Marti/api/tls/` stays `Purpose::Enrollment`. That is the blast radius, and it is asserted
  route by route.
- `resolve_principal` builds the `Grace` from the request — `GET`, a `clientUid` in the query,
  a configured window — or `None`. It is never built from anything the caller asserts about
  itself beyond the device it names, which is then checked against what was recorded.
- `identity::verify::verify_with_grace` honours it only for `Purpose::EnrollmentProfile`
  (`grace.filter(…)`, belt and braces) and only for a single-use credential whose `spent_uid`
  matches and whose `spent_at` is inside the window. `verify` itself cannot grant it — a caller
  has to ask by name — and the kind check (`Purpose::accepts`) runs *before* the relaxation, so
  the window can never widen which kinds a route takes.

**One deliberate loosening inside the window.** The token's own `expires_at` is not re-checked
once it has been spent. The spend proves it was live, `spent_at` is written by the claim alone
and cleared by any revocation, so the window measured from the spend is the only clock left
that matters. The alternative re-applies a 15-minute `enrollment_token_ttl` to a device
fetching its profile 0.4 s later, which strands whoever scanned the code at 14:59 — the same
class of failure this brief is about. The window is bounded, the device is named and a
revocation still ends it immediately.

## 2. The first stream handshake — the hypothesis is wrong, and here is the evidence

The brief's leading hypothesis was that the known-certificate register the client verifier
consults is refreshed on an interval, so a certificate issued seconds earlier is refused as
unknown. **It is not.** Three independent facts:

1. **The code.** `Pki::enroll` calls `self.revocations.note_issued(&issued.fingerprint)` in the
   same call that writes the `certificates` row, immediately after it (`pki/facade.rs`), and
   nothing anywhere reloads that cache on a timer — `RevocationCache::reload` has exactly two
   callers, `Pki::load` at start-up and a test.
2. **The wiring.** `runtime::listen` loads one `Pki` and hands the *same* `Arc` to
   `context.install_pki` (what the enrolment handler calls) and to `stream::serve` (which builds
   the `:8089` listener's `ServerConfig` from `Pki::stream_server_config`). Both verifiers hold
   `Arc::clone(&self.revocations)`. There is no second cache to fall behind.
3. **A real ATAK client already proves it in CI.** `interop/eud/scenarios/enroll-basic.toml`
   drives commoncommo — ATAK's own enrolment and streaming code — through `estream:`, which is
   `tls/config` → `signClient/v2` → **an immediate TLS stream connection with the certificate
   that came back**, and asserts `Interface Up` with no `Interface Error`.

New test, so that a future reordering fails here instead of in somebody's field log:
`pki::facade::the_verifier_a_listener_holds_accepts_a_certificate_issued_a_moment_ago` enrols
and then calls `ClientCertVerifier::verify_client_cert` on the verifier
`stream_server_config` is built from, with `require_known_cert` asserted on.

**What actually happened, as far as the evidence reaches.** The device's error —
`Read error: ssl=…: Failure in SSL library, usually a protocol error` — is `SSL_ERROR_SSL` on a
*read*, which is what a client sees when the server sends a TLS alert or drops the connection
without `close_notify`; under TLS 1.3 a client-certificate refusal arrives exactly that way,
after the client thinks the handshake is done. rustak has three ways to end a stream connection
like that, and two of them log at WARN (`client_verifier`: "Refused a client certificate at the
handshake", with the fingerprint and the reason; `listener_tls`: "Refused a stream connection",
when the certificate resolves to no row). Neither appears in the field log. **The third logged
nothing above `debug`: a handshake that fails at the TLS layer** — `serve_one`'s
`debug!("A stream handshake failed")`.

That is the one refusal consistent with everything observed, and its most likely instance is a
connection carrying **no** client certificate: ATAK adds the streaming entry to its connection
list *before* it enrols (07 §1.6) and reconnects the stream only *after* the enrolment profile
comes back (07 §1.5 step 6) — the call that answered `401`. A connection attempt made in that
window has no certificate to present, the mandatory verifier refuses it with an alert, and the
device retries on its own cadence, which is the "about a minute later" that then worked. I can
support that with the client's error string, ATAK's documented sequencing and rustak's own
refusal paths; I cannot *prove* it, because the server did not record it and the only remaining
witness is the device.

**So the fix for item 2 is the one the evidence supports: make the refusal visible.**
`listener_tls::refusal` classifies the handshake error and reports the two failures a device
actually suffers — `NoCertificatesPresented` and `InvalidCertificate` — at `info`, with a
reason that names the likely cause, while leaving the ordinary noise of a listening socket
(scans, browsers on the wrong port, resets) at `debug`. The next occurrence explains itself
from the server's own log.

**Deviation, stated plainly.** The brief asked for a test that fails before the fix. For item 1
there are several. For item 2 there is none that could: the behaviour the hypothesis predicted
does not exist, so what I added is a regression test that passes today and pins the ordering
inside `Pki::enroll`, plus a unit test for the new log classification. If the maintainer still
has the server's journal from 2026-09-19 08:30 UTC, `grep` it for
`"A stream handshake failed"` — the `debug` line — and the `peer` and rustls error on it settle
the remaining question in one line.

## 3. The log noise

`identity::credentials::record_use` fired a WARN on every `tls/config` call, because
`verify_basic` records an ordinary use on every successful verification and a one-time token
ignores it by design. Both halves are fixed: `verify_basic` no longer makes the call for a
single-use credential (nothing changes on the row either way — the guard was already ignoring
it), and the guard's own log is now `debug`. No behaviour change: `last_used_at` for an
enrolment token has always been written by the claim, not by this path.

## 4. Coverage

| Test | What it pins |
|---|---|
| `enroll_flows::atak_walks_its_three_calls_with_one_token_and_none_of_them_is_refused` | the field sequence: `200`, `200`, `200\|204` |
| `enroll_flows::a_spent_token_buys_the_profile_and_nothing_else` | second sign `401`, `tls/config` `401`, `/oauth/token` not `200`, another uid `401`, the right uid served |
| `enroll_flows::the_grace_window_closes_and_the_token_is_spent_again` | a 1 ms window, and `401` after it |
| `enroll_flows::revoking_a_token_inside_the_window_ends_its_grace` | an administrator's revocation beats the window |
| `identity::verify` (7 new) | the same matrix at the unit level, including "the relaxation has to be asked for by name" and a spend that named no device |
| `auth::resolve` (3 new) | which paths map to `EnrollmentProfile`; the uid reader, percent-decoded and case-insensitive; `GET`-only, window-only |
| `pki::facade` (1 new) | the verifier accepts a certificate issued a moment ago |
| `stream::listener_tls` (1 new) | which handshake failures are reported and which stay noise |
| `interop/node-tak` enrolment scenario | the profile fetch with the credential that enrolled, on every push |

The node-tak suite drives CloudTAK's own libraries, which hold a reusable **client password** and
never a one-time token, so the token half of the sequence cannot be expressed there; it is the
Rust contract test above. What node-tak adds is that the third call is exercised on every push
with the credential CloudTAK does hold, which is the regression that would otherwise be caught
only by a person with a phone.

## Exit checks

Run on 2026-09-19. All green.

```
$ cargo fmt --all -- --check
(clean)

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2m 57s
(clean)

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 34.14s
(clean)

$ MAX_FUNCTIONAL_LINES=300 bash scripts/check-file-length.sh
(clean — exit 0)

$ cargo test --workspace --no-fail-fast
… 40 test targets, every one `ok`.
TOTAL 2739 passed; 0 failed.

$ cd interop/node-tak && npm test
… ✔ the enrollment profile answers the credential that just enrolled (444.698ms)
tests 26 | pass 26 | fail 0 | skipped 0

$ cd interop/eud && npm run test:unit
tests 43 | pass 43 | fail 0
(`npm test` also runs test:scenarios, which needs the commoncommo container image
and the GHCR pull CI does; the unit half is what the brief asked for here.)

$ cd interop/node-tak && npm run typecheck
(clean)
```

Sixteen of the 2739 are new: 7 in `identity::verify`, 3 in `auth::resolve`, 1 in
`pki::facade`, 1 in `stream::listener_tls`, and 4 in `tests/enroll_flows.rs`. One
existing `config::auth` test gained an assertion. The node-tak suite gained one
scenario, which is the 26th above.

## Notes for whoever picks this up next

- `interop/eud` has no enrolment-profile scenario because `commotest` cannot fetch one (device
  profiles are ATAK-Java, M1-00). The gap is now covered by the Rust contract test and the
  node-tak scenario; it cannot be closed in the EUD harness without a driver inside the image,
  which the licence posture forbids.
- If a route is ever added under `/Marti/api/tls/profile/`, it inherits the grace window. That
  is called out in `marti::profiles`' module documentation and asserted in
  `auth::resolve::only_the_two_profile_routes_are_the_purpose_a_spent_token_can_reach`.
- `[auth] enrollment_grace = "0s"` restores the old behaviour exactly, and
  `docs/compat/atak.md` §1 step 5 says so, because an operator who sets it will fail that check
  and should know why.
