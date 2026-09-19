# M5-04 — the `/api/v1` fall-through, and why CI-01's `500` was never about load — complete

Read first: `.claude/plan/status/CI-01-2026-09-19-onboarding-probe-500.md` (the report this
answers); `conventions.md`; `M5-03-cloudtak-onboarding.md`.

## 1. The cause, reproduced

`rustak-server/build.rs` creates `rustak-ui/dist` so that `include_dir!` resolves in a tree where
`trunk` has never run. **No job in `.github/workflows/rust.yml` builds the UI**, and
`rustak-ui/dist` is ignored wholesale (`.gitignore:19`), so on every runner that directory is
*empty*: `ASSETS.get_file("index.html")` is `None`, and `web::ui::shell` answered that with

```rust
None => HttpResponse::InternalServerError()
    .content_type(ContentType::html())
    .body("<!DOCTYPE html><title>rustak</title><p>The user interface has not been built.</p>"),
```

Move `rustak-ui/dist/index.html` aside on this machine and the failure is the CI failure, verbatim:

```
assertion `left == right` failed: … it answered 500 Internal Server Error with content-type
Some("text/html; charset=utf-8") and body: <!DOCTYPE html><title>rustak</title><p>The user
interface has not been built.</p>
  left: 500
 right: 200
```

So it was deterministic all along, in one direction: **`200` in a tree where `trunk` has run,
`500` everywhere else.** The test was added in `d36ecca` and failed on its first CI run; it is the
only test in the crate that asserted a *status* on the fall-through (`web::server`'s
`an_unknown_path_reaches_the_single_page_shell` asserted `assert_ne!(NOT_FOUND)`, which a `500`
satisfies), so nothing caught it earlier.

The load hypothesis does not survive contact: the request never reached a handler, no route
matched, and the 12× runtime has a separate cause (§3). The middleware hypothesis does not
survive either — `api_auth` *does* run on this path and *does* resolve the token against the
database, but it resolved it successfully and passed the request through to the shell, which is
how the response carried `text/html` at all.

## 2. What the surface answers now

Two changes, each removing one way for the answer to depend on the machine it ran on.

**`rustak-server/src/web/ui.rs` — the shell is a `200` in both kinds of build.** Whether the UI
was compiled in is a property of the binary, identical for every request it will ever answer;
nothing about the *request* failed, so the status no longer says one did. The placeholder body
still names what is missing. `shell_of(Option<&[u8]>)` is split out so both halves are reachable
from a test whichever way this tree was built — the previous arrangement let CI exercise only the
`None` arm and a developer only the `Some` arm, which is precisely how an assertion that could not
be true in both places survived review.

**`rustak-server/src/web/api/mod.rs` — `/api/v1` stops at its own default service.** The guarded
scope now carries `.default_service(web::to(unmatched))`, answering `404` with the same
`{"error": …}` body and the same exact `application/json` as every other failure on that surface.
`web::server::marti_services` already refuses to let a TAK client parse the SPA shell as a payload;
there was no reason the admin API should hand HTML to a JSON client either.

The contract, as the route-table test now states it:

| Request | Answer |
|---|---|
| A path no route claims, with a session | `404 application/json` |
| A known path with the wrong method, with a session | `404 application/json` — the same thing |
| Either of those with no credential | `401 application/json` |
| Anything outside `/api/v1` and outside the Marti surface | `200 text/html`, the shell |

**There is no `405` here and there cannot be.** `actix_web::Scope::route` hoists the route's method
guard onto the resource it builds, so a resource whose method does not match never matches at all —
a known path addressed with the wrong method is indistinguishable from a path that does not exist.
`405` was one of the two options this brief was asked to choose between; it is not reachable without
rebuilding the whole route table on `web::resource`, and a surface where `/users/{u}/cloudtak-
onboarding` answered `405` while every other wrong method answered `404` would be worse than either.
`404` is also the one that tells a prober less.

**The gate still answers first**, and deliberately: `api_auth` wraps the guarded scope including its
default service, so an unmatched path with no credential is a `401`. The API does not tell an
unauthenticated caller which of its paths exist. The cost is that an unmatched path still pays for
one token resolution, and a database that cannot answer still makes that a `500` — which is the
correct answer for a broken database, and not what happened here.

`interop/shared/src/probe.ts` matches: `404` is now documented as what an unmatched `/api/v1` path
answers, HTML-on-success as what the shell answers *outside* `/api/v1`, and the file carries an
explicit "never probe a path that only answers a method other than `GET`" — replacing a comment
that claimed a POST-only endpoint answers `405`, which was never true of this server.

## 3. Why `preparing_a_hand_over_sweeps_the_one_before_it` ran for over a minute

Not a sleep, not a real-time TTL wait, and not an over-broad sweep. Every expiry case in that suite
closes the window by rewriting the stored timestamp (`backdate`), and `cloudtak::sweep` lists one
key/value partition holding at most a handful of rows. Paused tokio time and an injected clock
would have bought nothing, because nothing in the flow waits.

It is **RSA-2048 key generation, twice** — `identity/cloudtak/mod.rs` generates the client's key on
the real code path, and that test was the only one in the suite that performed two hand-overs.
Generating one is a random prime search, measured here with the optimisation the root `Cargo.toml`
already gives `rsa` and `num-bigint-dig`:

```
rsa2048 csr: 1.376s   181ms   884ms   722ms   306ms      (pki load, EC: 50ms;
                                                          p12 legacy write: 20–81ms;
                                                          signed_in: 3.7ms)
```

A 7.6× spread on a fast machine, and roughly 12× that on the instrumented two-vCPU runner. Two
draws from that distribution in one test is why libtest named *that* test and not the other
eighteen. It also accounts for the binary's whole CI wall clock: ~19 tests × ~1.4 hand-overs ×
~0.7 s × 12 ≈ 120 s, against the 122.58 s observed.

The test now stashes the stale bundle through `cloudtak::stash` — the same function a hand-over
stashes through — and performs one real `POST`, which is still the thing that sweeps. It also
gained the assertion that was missing: `take` deletes before it checks the window, so an expired
bundle the sweep had *missed* would answer `410` just the same. The test now reads the partition
and asserts one row, which is what actually proves the sweep ran.

**Outstanding, and not this brief's to fix:** every hand-over test still pays for one RSA-2048
generation on the real code path, and that remains the entire runtime of this binary in CI. The
established remedy is in the tree already — `testing/keys.rs` shares one RSA key per test process
for the JWT issuer, through `JwtIssuer::load_or_adopt`'s existing key parameter, "because
generating a key … once per test is what made this suite unfinishable on a two-core runner under
coverage". There is no equivalent seam for the hand-over's client key, and inventing one is an
architectural change (an injectable key source on `AppContext`) rather than a test change.
Worth a brief of its own; it would take this binary from ~120 s to a few seconds in CI.

## 4. Files

| File | Change |
|---|---|
| `rustak-server/src/web/ui.rs` | `shell_of`; the UI-less placeholder is a `200`; module doc; `a_deep_link_reaches_the_shell_rather_than_a_not_found` tightened from `assert_ne!(404)` to status + content type; `the_fall_through_is_the_same_answer_whether_or_not_the_ui_was_built` |
| `rustak-server/src/web/api/mod.rs` | `NO_SUCH_ROUTE`, `unmatched`, `.default_service(…)` on the guarded scope; module doc; `UNMATCHED`, `a_path_no_route_claims_is_a_json_not_found_rather_than_the_shell`, `an_unmatched_path_is_refused_before_it_is_looked_for` |
| `interop/shared/src/probe.ts` | the convention, corrected and extended with the POST-only warning |
| `rustak-server/tests/cloudtak_onboarding.rs` | the probe test asserts `404 application/json`; `expire` → `backdate(server, id)`; `preparing_a_hand_over_sweeps_the_one_before_it` performs one hand-over and asserts the row is gone |

No production behaviour changed for any path that matches a route.

## 5. Failing-before evidence

- `web::api::tests::a_path_no_route_claims_is_a_json_not_found_rather_than_the_shell` — with
  `.default_service(…)` removed: `left: 200, right: 404` on `GET /api/v1/users/ada/cloudtak-onboarding`.
- `web::ui::tests::the_fall_through_is_the_same_answer_whether_or_not_the_ui_was_built` — against
  the previous `shell()`: the `None` arm is a `500`.
- The original assertion, reproduced by moving `rustak-ui/dist/index.html` aside (§1).

## 6. Checks

| Check | Result |
|---|---|
| `cargo test -p rustak-server --features testing --test cloudtak_onboarding -- --test-threads=2` ×5 | 19/19 each, 8.2–10.6 s |
| The same, with `rustak-ui/dist/index.html` moved aside (the CI configuration) | 19/19 |
| `cargo test -p rustak-server --features testing --lib web::api` | 200/200 |
| `cargo test -p rustak-server --features testing` (whole crate, no UI built) | 1757 passed, 2 ignored, 0 failed |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo doc --workspace --no-deps` with `RUSTDOCFLAGS=-D warnings` | clean |
| `cargo fmt --all --check`, `scripts/check-file-length.sh` | clean |
| `cd interop/node-tak && npm test` | 30/30, 0 skipped |
| `interop/{node-tak,cloudtak,eud}` `typecheck` and `test:unit` | clean; 30 / 47 / 43 passed |

The full `cloudtak` and `eud` suites need their containers and run nightly; their probes target
`/Marti/*`, `/files/api/config`, `/oauth/token`, `/api/v1/groups`, `/api/v1/certificates` and
`/api/v1/cloudtak-onboarding/probe.p12` — every one of them a path that answers `GET`, so none of
them changes outcome under the new default service.
