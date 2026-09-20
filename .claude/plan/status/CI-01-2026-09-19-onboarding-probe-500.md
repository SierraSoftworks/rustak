# CI-01 — `GET /api/v1/users/{u}/cloudtak-onboarding` answered `500`

**RESOLVED by M5-04** — `761a389`, "fix(web): Answer unmatched API paths with a
JSON 404 and never 500 without a built UI". **The cause was deterministic, and
two of my conclusions below were wrong.** Both are kept rather than edited out,
because the reasoning errors are more useful than the conclusion was.

## The actual cause

**CI never builds the UI.** `rust.yml`'s `test` job runs without `trunk`, so
`web::ui::shell` answered **500 — "The user interface has not been built"** on
every fall-through. Locally the UI *is* built, so the same request answered 200.
Nothing to do with load, and nothing to do with the route.

The fix: the placeholder is a `200`, unmatched `/api/v1` paths answer a JSON
`404` behind the auth gate, `interop/shared/src/probe.ts` follows the new
convention, and the >60 s test does one RSA generation instead of two.

## My two mistakes

**1. "`ui::serve` is infallible, so it cannot produce a 500."** It returns
`HttpResponse` rather than `Result` — and I read an infallible *signature* as an
inability to produce an error *status*. A handler that cannot fail can still
answer `500` deliberately, which is exactly what `shell()` did. The signature
told me nothing; I should have read the body of `shell()`, which is four lines
further down the file I already had open.

**2. "Not reproducible locally, therefore load-dependent."** This is the worse
error. I ran the test alone (passed), the binary three times at
`--test-threads=2` (19/19 each), saw CI take 12x longer, and concluded
contention. The correct inference from *passes locally, fails in CI, every
time it runs there* is an **environment difference** — and the environment
difference was sitting in the workflow I own: the `test` job has no `ui` step
and no `ui-dist` artefact, while my working tree has a built UI from earlier
work.

**The rule for next time:** a test that passes locally and fails in CI is an
environment difference until proven otherwise. Load is the explanation of last
resort, not first, and "I could not reproduce it" is evidence about my machine
rather than about the test. Where the two environments differ is knowable — for
this repository it is written down in `docs/ci.md`'s job graph, which says
plainly that `ui` is a separate job from `test`.

## What was half right

The `api_auth` middleware thread was right that the middleware resolves the
token on an unmatched path — M5-04 confirmed it — but it passed through, so it
was never the failure. Ruling out the `users/{username}` routes and the shell
*as a route match* was correct and saved time; the conclusion drawn afterwards
was not.

## Standing note for this log

`rust.yml`'s `test` job runs **without a built UI**. The shell tests now cover
both shapes. If a UI-dependent test ever differs between CI and local again,
this is the first thing to check.

---

<details>
<summary>The original write-up, kept for the record — its conclusion was wrong</summary>


**Found by:** `rust.yml` run
[35449569084](https://github.com/SierraSoftworks/rustak/actions/runs/35449569084)
(`d36ecca`, M5-03), `Test` job. One test of nineteen:

```
---- the_surface_the_interop_suites_probe_for_answers_something_they_can_read ----
panicked at rustak-server/tests/cloudtak_onboarding.rs:1039:5:
assertion `left == right` failed
  left: 500
 right: 200
test result: FAILED. 18 passed; 1 failed … finished in 122.58s
```

**Owner:** whoever holds `rustak-server/src/web/` routing and the
`cloudtak-onboarding` routes. CI-01 does not edit non-test `src/`.

---

## 1. What the assertion was

`GET` on a **POST-only** route. The test documents a real trap: rustak serves
the admin UI's single-page shell for anything it does not recognise, so a `GET`
on a POST-only path is **`200 text/html`** rather than `404` or `405` — which is
why the interop suites probe the *download* path and not this one. The test
pins that fall-through so nobody breaks the probe's assumption silently.

It got `500`.

## 2. What is and is not known

**Not reproducible here.** On this machine, with the same commit:

- the test alone: **passes** (0.83 s);
- the whole binary, `--test-threads=2`, three times: **19/19 each**, ~10 s.

CI took **122.58 s** for the same nineteen tests — roughly 12× — which is the
instrumented two-vCPU runner, and the same suite logged
`preparing_a_hand_over_sweeps_the_one_before_it has been running for over 60
seconds`. So this is a load-dependent `500`, not a logic error that any run
would show.

**The cause is not in the run.** libtest captures a test's output and prints
only the panic, so the server's own error for that request never reached the
job log. `left: 500, right: 200` is the entirety of the evidence.

## 3. Why it matters even though it is rare

Two reasons, and the second is the one I would act on:

1. A fall-through that answers `500` under load is a fall-through the **surface
   probe cannot trust**. `interop/shared/src/probe.ts` reads `404` and
   `200 text/html` as "not served"; anything else it reads as *served*. A `500`
   would therefore make a scenario **run** rather than skip — so this direction
   is safe. The dangerous direction is the reverse, and it is worth knowing
   which way the route fails under load before relying on it.
2. Whatever produced a `500` on a request that should never reach a handler is
   doing work it does not need to do.

### Correction: it is not the `users/{username}` routes, and it is not the shell

My first reading — that the request matches a `/api/v1/users/{username}/…`
route and errors for an unknown account — is **wrong**, and I checked before
anyone spent time on it. The registered `users` routes are:

```
GET   /users            GET /users/{username}            GET /users/{username}/groups
POST  /users            PATCH /users/{username}          PUT /users/{username}/groups
```

None matches `/users/probe/cloudtak-onboarding`: `{username}` is one segment,
and the three-segment routes end in the literal `groups`. The onboarding route
itself is `POST` only (`web/api/cloudtak_onboarding.rs:45`). So the path really
does fall through.

It is **not the shell either**. `web/server.rs:79` sets
`.default_service(web::get().to(ui::serve))`, and `ui::serve` returns
`HttpResponse` rather than `Result` — it serves an asset or the shell and has no
failure path at all. It cannot produce a `500`.

### The mechanism that fits

`rustak-server/src/web/api/mod.rs:93` wraps the whole `/api/v1` scope:

```rust
.wrap(from_fn(middleware::api_auth))
```

**Middleware runs before routing resolves.** An unmatched path *inside* the
scope still goes through `api_auth` — and this request carries an
`authorization` header, because the suite's `fetch!` macro inserts the admin
token on every call. So `api_auth` resolves that token, which reaches the
database, on a request that was never going to match a route.

That explains all three observations at once: it is load-dependent (the database
is what contends), the shell never rendered (the middleware short-circuits
before routing), and `ui::serve` being infallible is irrelevant.

**Where I would look:** what `middleware::api_auth` does when its token
resolution fails under contention, and whether that maps to `500`. A token that
cannot be *checked* is a different case from one that is invalid, and only one
of them is a server error.

**A second question worth asking while there:** whether an unmatched path inside
`/api/v1` should be paying for authentication at all. The Marti scope already
answers its own JSON `404` through `.default_service(web::to(marti_unmatched))`
(`web/server.rs:100`); an equivalent on the API scope would make the
fall-through explicit rather than incidental — though note the interop probes
currently *depend* on the HTML shell answering here, so changing it would need
`interop/shared/src/probe.ts` changed with it.

## 4. What I changed, and what I did not

**Not the product**, and **not the assertion** — the test is asserting the right
thing and a `500` is a genuine failure.

I made the next occurrence diagnosable instead
(`rustak-server/tests/cloudtak_onboarding.rs`): the response body and
content-type are now read *before* the status assertion and included in its
message, so a repeat prints what the server said rather than `left: 500`. The
test still passes and still proves exactly what it did.

## 5. What would settle it

Either of these, in one run:

- The next failure, which now carries the body and content-type.
- `RUST_LOG=debug cargo test -p rustak-server --features testing --test
  cloudtak_onboarding -- --nocapture --test-threads=2` on a loaded machine,
  which would show the handler the request actually reached.

If it turns out the request is matching a `users/{username}` route, the fix is
error mapping there rather than anything about the probe — and this test is
then pinning the right invariant for the wrong reason, which is worth a comment.


</details>
