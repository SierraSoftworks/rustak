# M2-02 — Identity services: credentials, devices, group membership + admin API — complete

Brief: `.claude/plan/briefs/M2-02-identity-credentials.md`
Read first: `conventions.md`; `plan.md` → Identity & auth model; `compat/groups.md`, `compat/enrollment.md`;
`design/03-identity-pki-acme-auth.md` §4 (identity model, credential API, bitpos/`__ANON__`, effective
groups), §6 (admin API table), §9 (unit tests); status files `M0-03`, `M0-04`, `M0-06`, `M0-07`, `M0-11`.

## What was built

Five new files under `identity/`, five under `web/api/`, and additions to two `rustak-api` DTO modules.
Nothing under `pki/**`, `main.rs`, `lib.rs` or `runtime.rs` was touched.

| File | Functional lines | Contents |
|---|---:|---|
| `identity/credentials.rs` | 207 | `MintRequest`/`MintedSecret`, `mint`, `record_use`, `revoke`, `to_dto`, `enroll_url`, `enroll_url_template`, per-kind secret generation and lifetime defaults |
| `identity/verify.rs` | 136 | `Purpose`, `Verified`, `VerifyError`, `verify` (dummy-hash on every failing path, cache before argon2) |
| `identity/secret_cache.rs` | 79 | `VerifiedSecretCache`: sha256(id ‖ secret) keys, 5-minute TTL, 4096 cap, `forget`/`sweep`, process-wide `shared()` |
| `identity/devices.rs` | 88 | `upsert_seen`, `list`, `list_for_user`, `get`, `delete`, `to_dto`, `to_dtos` (owner names in one read) |
| `identity/members.rs` | 181 | `grants_for_user`, `replace_manual`, `effective_for_device`, `set_active`, `active_for_device` |
| `identity/groups.rs` | 150 | (existing claim mapping) **+** `list`, `create`, `patch`, `delete`, `to_dto`; `memberships` moved to `members::grants_for_user` |
| `identity/mod.rs` | 12 | module docs and re-exports |
| `web/api/subject.rs` | 64 | `Subject`, `resolve` (self / admin-over-anybody), `owns`, shared `failed` |
| `web/api/credentials.rs` | 186 | `GET`/`POST /credentials`, `DELETE /credentials/{id}`, `GET /credentials/{id}/enroll-url` |
| `web/api/devices.rs` | 126 | `GET /devices`, `GET`/`DELETE /devices/{uid}`, `PUT /devices/{uid}/active-groups` |
| `web/api/groups.rs` | 101 | `GET`/`POST /groups`, `PATCH`/`DELETE /groups/{name}` |
| `web/api/users_groups.rs` | 81 | `GET`/`PUT /users/{username}/groups` |
| `web/api/mod.rs` | 102 | +6 module lines and 14 route lines; the protected-route table extended by the same 14 |

**80 new tests** in the server crate and **15** in `rustak-api`, each file with its single trailing
column-0 `#[cfg(test)] mod tests`.

### `rustak-api` additions (the brief's "only if missing")

| Type | Module | Why |
|---|---|---|
| `CreateGroupRequest`, `GroupPatch` | `group.rs` | nothing existed for channel CRUD; `bitpos` is deliberately absent from the create request |
| `ActiveGroup` (+ `expand()`) | `group.rs` | the body of `PUT /devices/{uid}/active-groups`, and what it answers with |
| `EnrollTemplate`, `ENROLL_URL` | `credential.rs` | the QR pieces without the secret; `ENROLL_URL` is the one place the `tak://` shape is written down |

`Credential`, `CredentialKind`, `CredentialCreated`, `CreateCredentialRequest`, `Device`, `Group`,
`GroupMembership`, `GroupSource` and `MembershipSource` were already right and are unchanged.

`rustak-api/src/lib.rs` was touched once, to add the five new names to the two existing `pub use`
lines for `credential` and `group` — without it the types the brief asked for would not be reachable
from `rustak_api::`.

## Decisions worth recording

### Free functions, not a `Credentials<S>` struct

Design 03 §4 sketches `impl<S: Services> Credentials<S>`. The M0 house style is free functions over
`&Database` (`users::provision`, `groups::apply_claims`, `settings::resolve`), every one of them
testable against `Database::open_in_memory()` with no service wiring. The brief's
`Credentials::{mint, verify, record_use, revoke}` is implemented as
`credentials::{mint, record_use, revoke}` and `verify::verify`, which reads identically at the call
site and stays consistent with the four identity modules already in the tree.

### `verify` is its own module

`credentials.rs` came to 334 functional lines with minting and verification together, over the 300
limit. Split by responsibility rather than by line count: `credentials.rs` is the lifecycle an
operator drives (mint, record, revoke, describe), `verify.rs` is the authentication path (`Purpose`,
`verify`, and the refusal taxonomy). `Purpose` lives with the code that enforces it.

### A one-time token cannot be spent by an ordinary recorded use

`record_use(db, credential, consumed, cache)` takes the row rather than the id, and when
`consumed == false` on a single-use credential it logs and does nothing. The use count is what
exhausts a credential, so an ordinary use recorded by the `GET /Marti/api/tls/config` call that
precedes `signClient` would spend the token and strand the person half-way through enrolling.
`consumed = true` is the only thing that spends one, exactly as design 03 §7 step 3 describes.

### `Purpose` is the whole of "where a secret is accepted"

`Enrollment` takes an `EnrollmentToken` or a `ClientPassword` (ATAK's manual "enroll for
certificate" flow needs the latter); `OAuthPassword`, `Marti` and `StreamAuth` take a
`ClientPassword` and nothing else; `ServiceApi` takes a `ServiceToken` and nothing else. An enrolment
token therefore cannot become a password grant and a service token cannot enrol a device — asserted
by `a_credential_is_refused_where_its_kind_does_not_belong`.

### `VerifiedSecretCache` is process-wide and invalidated by revocation

There is no way to reach `main.rs`/`runtime.rs`/`web/server.rs` from this brief's file list, so the
cache is a `LazyLock` reached through `VerifiedSecretCache::shared()` rather than an `Arc` threaded
through app state. `new(ttl, capacity)` exists for tests. `revoke` and a consuming `record_use` both
call `forget`, so a credential taken back stops working immediately rather than in up to five
minutes. Entries are keyed on sha256(credential id ‖ secret): nothing stored is a secret, and one
account's password cannot produce a hit on another's credential.

### `__ANON__` is the installation's decision, not an administrator's

While `[auth] anon_group_default` is on, `members::replace_manual` puts the `__ANON__` IN+OUT grant
back into any set an administrator sends without it, and `members::effective_for_device` adds the
bits to a subscription whose account never had the grant (an account created while the setting was
off). The device's own preference still wins: a client that switched the default channel off through
`PUT …/active` stays switched off. Turn the setting off and nothing re-adds anything.

### `replace_manual` is a delta, not delete-then-insert

The `members` repository has `replace_provider_grants` but nothing equivalent for manual grants, and
`db/repos/**` is outside this brief. Composing delete-all-then-insert from the existing methods would
leave a member momentarily in no channels at all while a live subscription might read them, so the
handler computes the difference and issues only the grants and revocations that actually change.

### Refusals

- A stranger's account is `403`, not `404`: the caller already knows the name they asked for, so a
  `404` hides nothing and sends an administrator hunting for a typo. A name that genuinely is not
  there is `404`, which only an administrator ever sees.
- A channel named in `PUT /users/{username}/groups` that does not exist is a `400` **naming it** —
  silently dropping it would leave an administrator looking at a membership list that lost a row.
- A channel named in `PUT /devices/{uid}/active-groups` that does not exist is **dropped**, per
  `compat/groups.md` §2: ATAK sends back the list it was given, and a channel deleted since would
  otherwise break every client that still had it cached. The response is the device's resulting
  state rather than an echo, so the caller can see what was dropped.
- A grant sent with `"source": "oidc"` is a `400`: it would be replaced at the member's next sign-in,
  so accepting it would be accepting a change that does not last.

### Secrets

`CreateCredentialRequest` → `CredentialCreated` is the only response that carries a secret, and
`CredentialCreated` redacts it (and the `enroll_url` that embeds it) in `Debug`. The audit entry for
`credential.minted` carries the id, kind, label and expiry and never the secret — asserted by
serialising the audit records and grepping for it. `GET /credentials/{id}/enroll-url` returns
`host`, `username` and a `url_template` still containing `{token}`; an endpoint that could re-emit a
working link would be an endpoint proving the server had kept the secret.

### Audited mutations

`credential.minted`, `credential.revoked` (category `enrollment`); `device.removed`,
`device.channels-changed`, `group.created`, `group.updated`, `group.deleted`,
`user.channels-changed` (category `administration`). Each carries the subject (the account, or the
channel name for the group entries) and the actor.

## TODOs left for the briefs that own the code

| Where | What |
|---|---|
| `identity/credentials.rs::revoke` | `TODO(M2-01/M2-03)`: certificates are revoked in storage and a warning is logged; telling `pki::RevocationCache` and closing live mTLS sessions needs the PKI hook, which does not exist yet (`pki/mod.rs` exports only `ca`, `keys`, `pem`, `server_cert` as of this writing). |
| `web/api/devices.rs::set_active_groups` | `TODO(M2-08)`: emit `t-x-g-c` to the account's *other* devices and re-authenticate the live subscription, per `compat/groups.md` §3. |
| `web/api/users_groups.rs::put` | `TODO(M2-08)`: the forced/broadcast `t-x-g-c` to every one of the account's devices when an administrator changes membership. |

Nothing streams yet, so both notification TODOs have nothing to notify.

## Exit checks

Run in a sandbox copy of the tree at
`/private/tmp/.../scratchpad/wt`, with `rustak-cot/` pinned to its committed state: the agent working
on `rustak-cot` had that crate mid-refactor (`src/proto/mod.rs` referencing `from_proto`/`to_proto`
modules that did not exist yet), so the workspace as checked out would not compile for anybody. All
other crates, including the concurrent `pki/**` work, are the working tree as-is.

```
$ cargo test -p rustak-server --lib identity::
test result: ok. 71 passed; 0 failed; 0 ignored; 0 measured; 719 filtered out; finished in 2.30s

$ cargo test -p rustak-server --lib web::api::
test result: ok. 101 passed; 0 failed; 0 ignored; 0 measured; 689 filtered out; finished in 2.58s

$ cargo test -p rustak-server --lib -- --skip pki::
test result: ok. 642 passed; 0 failed; 1 ignored; 0 measured; 147 filtered out; finished in 7.40s

$ cargo test -p rustak-api
test result: ok. 93 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

$ cargo clippy -p rustak-server -p rustak-api --all-targets --all-features -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-server -p rustak-api --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5.13s
   Generated target/doc/rustak_api/index.html and 2 other files

$ cargo fmt -p rustak-server -p rustak-api --check
(no diff in any file this brief owns)

$ ./scripts/check-file-length.sh
(exit 0 — see the note below about untracked files)
```

Four notes on those runs:

- `cargo test -p rustak-server --lib` with no filter **hangs** in the concurrent PKI work —
  `pki::tls::resolver::tests::*` and `pki::tls::tests::*` each sit for minutes without completing a
  handshake. The whole-crate run above therefore skips `pki::`; every other test in the crate, 642
  of them, passes. The brief's own scope (`identity::`, `web::api::`) is covered by the two filtered
  runs and needs nothing from `pki`.

- Three failures came from files this brief does not own, all in the concurrent PKI/bootstrap work.
  They were patched **in the sandbox only** to get a clean run over this brief's code; the working
  tree is untouched and whoever owns those files should fix them:
  `rustak-server/tests/bootstrap.rs:59` (`clippy::wrong_self_convention` on `fn is_clean(self)`),
  `rustak-server/src/pki/tls/resolver.rs:235,345` (`clippy::cloned_ref_to_slice_refs`), and
  `rustak-server/src/pki/mod.rs:18` (rustdoc: `` [`revoke`] `` is ambiguous between the function and
  the module — `` [`mod@revoke`] `` or `` [`revoke()`] ``).
- `cargo fmt --check` reports diffs in `pki/{facade,p12,testing}.rs` and
  `pki/tls/{mod,peer,resolver,client_verifier}.rs`, all the other agent's. This brief's files were
  formatted individually with `rustfmt --edition 2024` rather than running
  `cargo fmt -p rustak-server`, which would have reformatted somebody else's work mid-edit.
- `./scripts/check-file-length.sh` reads `git ls-files`, so it does not see this brief's new,
  untracked files. They were counted with the same `awk` the script uses: the largest is
  `identity/credentials.rs` at 207 functional lines, well under the 300 limit.
