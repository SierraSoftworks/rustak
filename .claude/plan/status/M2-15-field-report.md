# M2-15 — Field report: the first real ATAK enrolment against a production server

**When.** 2026-09-19, 08:30 UTC.
**Client.** ATAK-CIV 5.6 on an Android device, `clientUid = ANDROID-7e0bf5df978a87d8`.
**Server.** The maintainer's production rustak, public listener on `:8446`, stream on `:8089`.
**Account.** `notheotherben`, enrolling with a one-time token (`credential=1` — the first
credential this installation ever minted, so this was a fresh enrolment on a fresh install).

This file is the record of what actually happened on the wire, written before anything was
changed, so that the sequence is on the record independently of the fixes it produced. The
work itself is `.claude/plan/status/M2-15-atak-enrolment-sequence.md`.

## The sequence

ATAK enrols in **three** Basic-authenticated calls carrying the **same** one-time token:

1. `GET /Marti/api/tls/config` — the name entries it builds its signing request from.
2. `POST /Marti/api/tls/signClient/v2?clientUid=ANDROID-…&version=…` — the enrolment itself.
3. `GET /Marti/api/tls/profile/enrollment?clientUid=ANDROID-…` — the enrolment device
   profile, fetched **unconditionally** and with the **same credential**, 0.4 s after the
   certificate came back.

rustak spends the one-time token on the successful sign — by design, M2-03 and M5-02's atomic
claim — so the third call arrived with a credential that no longer existed as far as
`identity::verify` was concerned, and was answered `401`.

## The log, verbatim

The maintainer's server log excerpt, exactly as it reached this brief:

> `tls/config` → WARN "Ignored an ordinary use recorded against a one-time credential.
> credential=1 kind=enrollment_token"; `signClient/v2` → same WARN, then "Saw a device for
> the first time. device=ANDROID-7e0bf5df978a87d8", then "Issued a client certificate.
> subject=CN=notheotherben,O=rustak,OU=rustak … key=RSA-4096"; 0.4 s later
> `GET /Marti/api/tls/profile/enrollment?clientUid=ANDROID-…` → `identity::verify:
> error=BadSecret`, `auth::basic: error=Rejected` (401).

On the device, in order:

> "Failed to get profile: Enrollment (401)"
> "TAK server registration failed"
> "Read error: ssl=…: Failure in SSL library, usually a protocol error" (the first stream
> attempt)

and then the stream came up on its own about a minute later.

## What each line means

- **The WARN on every `tls/config` call** is
  `identity::credentials::record_use` refusing to count an ordinary use against a one-time
  credential. That refusal is correct — counting it would spend the token before it had
  bought anything — but the path it fires on is the ordinary, expected one, so it was noise
  at WARN on every single enrolment.
- **`identity::verify: error=BadSecret`** on the profile call rather than `Exhausted`: the
  claim writes `revoked_at` as it spends the token, and `credentials.find_by_hint` filters
  `revoked_at IS NULL`, so a spent token is not even a candidate row. The refusal is
  therefore indistinguishable from a wrong secret, which is why the log says `BadSecret`.
- **`401` on the profile route** is what ATAK reports as
  "Failed to get profile: Enrollment (401)" and then as "TAK server registration failed"
  (research `07` §2.3: any status other than `200`/`204`/`304` is a `ConnectionException`).
- **The stream error is an ATAK-side read error**, not a rustak log line. rustak logs a
  failed stream handshake at `debug!` (`stream::listener_tls::serve_one`), which is below the
  default level, so the server said nothing about it at all.

## Why no test caught it

The `interop/eud` harness runs commoncommo — ATAK's own enrolment and streaming code — but
its `estream:` command performs `tls/config` → `signClient/v2` → **stream connect**. It never
fetches the enrolment profile, because device profiles are ATAK-Java rather than commoncommo
(M1-00 §matrix). `interop/node-tak` models CloudTAK, which does not fetch one either. So the
third call of the real sequence had no coverage anywhere, on any listener, and the 401 was
invisible to CI while being the first thing a real device does.

## What this produced

1. A grace window: a spent enrolment token stays usable for the two profile routes, for the
   uid that spent it, for `[auth] enrollment_grace` (10 minutes by default) and nothing else.
2. The `tls/config` WARN demoted, and the pointless call removed at its source.
3. Coverage of the real three-call sequence, in `tests/enroll_flows.rs` and in the node-tak
   suite that runs on every push.
4. An answer for the stream handshake — see the work status file. The short version: the
   register the client verifier consults is updated *at issuance*, in the same call that
   writes the certificate row, and the `enroll-basic` commoncommo scenario has been enrolling
   and connecting seconds later in CI all along, so "the cache is refreshed on an interval"
   was not what happened here. What rustak could genuinely be faulted for is that its own
   refusal was invisible: a failed stream handshake is now reported with its reason.
