# M7-03 — A channel created by an OIDC sign-in tells the stream at once — complete

Brief: `.claude/plan/briefs/M7-03-oidc-channel-cache.md`
Read first: `.claude/plan/plan.md` → Identity & auth model; `conventions.md`; status
`M2-14-loose-ends.md` (the `GroupCache` hook and why this was left), the account-linking
commits `abba577` / `ec7320f` and migration `0017_user_claims.sql` (which reshaped
`identity/users.rs` after M2-14); `identity/{groups,users,members}.rs`; `stream/{groups,
live,router,notify}.rs`.

## What was built

| File | Functional lines | What changed |
|---|---:|---|
| `rustak-server/src/identity/groups.rs` | 207 (was 174) | `apply_claims` takes `&AppContext` and answers `bool`; `lookup_or_create` reports whether it created; new private `provider_grants_differ` |
| `rustak-server/src/identity/users.rs` | 287 (was 273) | `provision` and `link_identity` take `&AppContext`; new private `claims_applied` |
| `rustak-server/src/web/api/auth.rs` | 209 (unchanged) | one call site: `context.db()` → `context` |
| `rustak-server/src/auth/oauth_server/login.rs` | 274 (unchanged) | the same, for the `/login/*` federation flow |
| `rustak-server/src/web/api/me.rs` | 76 (was 82) | the same, for `POST /me/oidc-link` |
| `rustak-server/src/testing/oidc.rs` | 252 (was 237) | `TestIdentityProvider::set_groups`, so the `groups` claim can change between two sign-ins |
| `rustak-server/tests/oidc_channels.rs` | 236 (new) | the two end-to-end assertions |

### 1. The routing table

`<dest group="…">` resolves a name through `stream::GroupCache`. M2-14 gave
`groups::{create,patch,delete}` a `routing_changed(context)` and could not give it to
`apply_claims`, which took a `&Database` because `identity/users.rs` did.

`apply_claims` now takes the same `&AppContext`. `lookup_or_create` answers
`Option<(GroupRow, bool)>` — the flag is *this call created it* — and the invalidation
happens **once after the loop**, not inside it: an invalidation between two creations lets
the next lookup cache a map that still lacks the second.

### 2. The membership half, which was the worse gap

A channel a claim creates costs a second. A membership a claim *removes* cost until the
device reconnected: a live stream connection holds the `GroupSet` it authenticated with, and
nothing re-read it. So the identity path now ends in the same call the channels API ends in
rather than a second copy of it.

`apply_claims` returns **whether the provider's grants for that account changed** — read
before the write, because `replace_provider_grants` is a `DELETE` followed by `INSERT`s and
says nothing about what was there, and filtered to `source = 'oidc'` so that an
administrator's grants are not reported as the directory's. `Both` is compared as the two
rows the repository is about to write, not as the one value the mapping collected.

`identity/users.rs` turns that flag into `members::channels_changed(context, id, &username,
None)` — which is M2-14's path: re-authenticate every live connection the account has against
`effective_for_{device,account}`, then `t-x-g-c` to it. `None` as the originating device
because a sign-in is not one connected device's action, so every device is told.

The flag matters: without it every sign-in would send `t-x-g-c`, and that notice makes every
ATAK on the account **discard every map item this server gave it and re-fetch**
(`compat/streaming.md` §9). A daily sign-in that changed nothing would empty the operator's
map.

`identity/users.rs` names no stream type. It calls `groups::apply_claims` and
`members::channels_changed`, both of which are `identity`.

## Testing

- `identity::groups` — **2 new.** `a_channel_a_sign_in_created_can_be_routed_to_now` (the
  probe fills the cache with a map that lacks the channel, then a claim creates it and the
  next message is `Relayed { recipients: 1 }`, reusing M2-14's `live_context`/`join`
  harness); `only_a_claim_set_that_actually_changed_is_reported_as_one` (same claims twice,
  one direction rather than two, a claim withdrawn, and an administrator's `__ANON__` grant
  which is not the provider's to report). Six existing tests now drive a context.
- `rustak-server/tests/oidc_channels.rs` — **2 new, the brief's integration tests.** Real
  `App`, real `POST /api/v1/auth/token`, real `TestIdentityProvider` (RS256 over HTTP):
  - a sign-in whose `groups` claim names a channel that does not exist creates it *and* a
    fake EUD routes to it in the same moment — after a probe that has already cached a map
    without it, which is what makes this about the invalidation rather than about the
    one-second refresh;
  - a second sign-in with the claim withdrawn leaves the peer's next message
    `Dropped(NoRecipients)` against the connection she *already had*, and her device
    receives the `t-x-g-c`.
- **Both were confirmed to fail without the fix.** With the two notifications temporarily
  gated off: `left: Dropped(NoSuchGroup("ops")), right: Relayed { recipients: 1, explicit:
  true }` and `expected a channel-change notice, got Err(Empty)`. Nothing sleeps in either
  test, so neither could pass on the one-second refresh.
- `testing/oidc.rs`: the token endpoint now reads a shared `groups` value at redemption
  rather than one baked in at start-up. Default unchanged (`["ops_WRITE"]`), so every
  existing suite is untouched.

## Exit checks

Run on 2026-09-19, on a working tree four other agents were editing at the same time.

```
$ cargo fmt --all -- --check
(clean)

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 22.57s

$ cargo clippy --workspace --all-targets --features rustak-server/testing -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.62s

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.06s
   Generated …/target/doc/rustak_api/index.html and 6 other files

$ ./scripts/check-file-length.sh
exit 0

$ cargo test -p rustak-server --features testing --no-fail-fast
exit 0
TOTAL 2055 passed; 0 failed
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
all doctests ran in 1.65s; merged doctests compilation took 1.24s

$ cargo test -p rustak-server --features testing --test oidc_channels
running 2 tests
test a_channel_a_sign_in_created_is_a_destination_in_the_same_request ... ok
test a_claim_that_stopped_being_sent_takes_a_live_connection_off_the_channel ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.45s
```

Three earlier runs of `cargo fmt --all -- --check`, `cargo doc` and `cargo check` failed on
`runtime.rs`, `web/plain.rs`, `web/tls.rs`, `pki/acme/**` and `rustak-core/src/telemetry.rs`
— other agents' files, mid-edit. Re-run after they settled; the output above is the final
state. One `marti_channels` binary was `SIGKILL`ed once under the load of parallel builds and
passed on the next run.

## Deviations from the brief

1. **`rustak-server/src/auth/oidc/**` does not exist.** The OIDC code is `config/oidc.rs`,
   `web/helpers/oidc/**`, `auth/oauth_server/login.rs` and `testing/oidc.rs`. Nothing under
   that path was created.
2. **Two `provision`/`link_identity` call sites are outside `web/api/auth*.rs`**, which is
   where the brief expected them all: `web/api/me.rs` (`POST /me/oidc-link`, added by
   `abba577`/`ec7320f` after M2-14) and `auth/oauth_server/login.rs` (the server-driven
   `/login/*` flow TAK clients use). Both are one line each — `context.db()` → `context` —
   and neither file is another agent's. Leaving them out would have meant two sign-in paths
   with a different answer from the third, which is the failure mode this brief exists to
   close.
3. **`rustak-server/src/testing/oidc.rs` was edited**, which the brief does not list. The
   provider's `groups` claim was fixed when it started, so "a second sign-in with the channel
   removed from the claim" could not be expressed at all. `set_groups` is additive, the
   default is what it always was, and no other agent holds that file.
4. **No `identity/claims.rs`.** The brief offers the split if the files would otherwise go
   over; `groups.rs` is 207 and `users.rs` 287 functional lines, so both stay whole. `users.rs`
   is close to the limit, and the next thing added to it should take the split.
5. **The `changed` flag rather than the handle, for the *membership* half.** The brief allows
   either. `apply_claims` takes the handle and invalidates the routing cache itself, but the
   `t-x-g-c` is addressed to a **person** and `apply_claims` is given a `UserId` — the caller
   has the `UserRow`. Looking the name back up inside `apply_claims` would have been a read to
   recover something the caller already held.
6. **`patch` was left alone.** The brief is about the claim-driven path; `patch` already had
   the hook from M2-14.

## Backlog

- **Closes** the M2-14 backlog line "A channel a sign-in creates does not invalidate the
  routing cache" (`.claude/plan/backlog.md`). Not removed here: the brief forbids editing
  that file. **The orchestrator should drop that line.**
- **New, small.** `db.members().replace_provider_grants` returns `()`, so
  `provider_grants_differ` reads the account's memberships back a second time to answer
  "did anything change". The repository is doing the `DELETE`/`INSERT` and knows the answer
  for free; having it report the row counts would remove a read from every federated sign-in.
  (`rustak-server/src/db/repos/members.rs`, not this brief's file.) Found by M7-03.
