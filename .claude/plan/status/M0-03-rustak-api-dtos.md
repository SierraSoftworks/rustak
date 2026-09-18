# M0-03 — `rustak-api`: identity newtypes and admin DTOs: status

## Summary

`rustak-api` is implemented per design 01 §2.1 with the brief's plan deltas applied. The M0-01
stub modules were replaced with real types; `src/identity.rs` became `src/identity/` (five files);
`src/passkey.rs` is new. 20 source files, all far inside the 300-functional-line limit (largest is
`service.rs` at 184), each with a single trailing column-0 `#[cfg(test)] mod tests`. 89 tests, all
passing, with a serde round-trip per DTO. All five exit checks pass (outputs below), including the
`wasm32-unknown-unknown` check — which found a real dependency problem, see "Deviations" item 1.

Only files under `rustak-api/` and this status file were touched. No `git`/`but` commands were run.

## Modules

| File | Types |
|---|---|
| `src/lib.rs` | crate docs, module list, flat `pub use` of every DTO |
| `src/identity/mod.rs` | re-exports; `pub(crate) mod newtype` |
| `src/identity/newtype.rs` | `string_newtype!` — Display/Debug/AsRef/FromStr/validating serde for the string newtypes |
| `src/identity/username.rs` | `Username`, `UsernameError`, `MAX_LENGTH`, `RESERVED`, `RESERVED_PREFIX`, `FORBIDDEN` |
| `src/identity/uid.rs` | `DeviceUid`, `ServiceName` (`uid()` → `SERVICE-<name>`), `MissionGuid`, their errors |
| `src/identity/group.rs` | `GroupName` (`ANON`), `Direction{In,Out,Both}`, `GroupNameError` |
| `src/identity/ids.rs` | `define_id!` → `UserId`, `DeviceId`, `GroupId`, `CertificateId`, `CredentialId`, `PasskeyId`, `ServiceId`, `MissionId`, `ResourceId`, `ProfileId` |
| `src/auth.rs` | `AuthMetadata`, `AuthMode::{Oidc{…},Passkey}`, `TokenExchangeRequest`, `TokenRefreshRequest`, `TokenResponse`, `AuthVia`, `Me` |
| `src/passkey.rs` | `PasskeyRegistrationStart/Finish`, `PasskeyLoginStart/Finish`, `PasskeyChallenge`, `PasskeySummary` |
| `src/setup.rs` | `SetupStatus`, `CreateAdminRequest`, `AdminCreated`, `InitCaRequest`, `CaKeyType`, `CaSummary`, `ServerSettingsRequest` |
| `src/user.rs` | `User`, `UserKind`, `UserSource`, `UserPatch` |
| `src/group.rs` | `Group`, `GroupMembership`, `GroupSource`, `MembershipSource` |
| `src/device.rs` | `Device` |
| `src/credential.rs` | `CredentialKind::{EnrollmentToken,ClientPassword,ServiceToken}`, `Credential`, `CreateCredentialRequest`, `CredentialCreated` |
| `src/certificate.rs` | `CertificateKind`, `CertificateSource`, `Certificate` |
| `src/service.rs` | `Capability`, `ServiceEndpoints`, `ServiceDescriptor`, `ServiceState`, `ServiceStatus`, `Heartbeat`, `ServiceSummary` |
| `src/audit.rs` | `AuditCategory` (10 variants), `AuditOutcome`, `AuditRecord` |
| `src/health.rs` | `ComponentStatus`, `Health` |
| `src/settings.rs` | `ServerSettings` |
| `src/error.rs` | `ApiErrorBody` |

## Plan deltas applied

- **No local passwords.** `AuthMode::Local` and `LocalLoginRequest` (design 01 §2.1) are gone;
  `AuthMode` is `{Oidc{authorization_endpoint, client_id, scopes, pkce}, Passkey}`. A test asserts
  `{"kind":"local"}` and `{"kind":"password"}` fail to parse rather than deserialising into
  something else.
- **Credential kinds** are `EnrollmentToken` / `ClientPassword` / `ServiceToken`, wire strings
  `enrollment_token` / `client_password` / `service_token` (matching M0-07's
  `credentials.kind` CHECK). `DevicePassword` and `LocalPassword` are gone, and a test asserts both
  fail to parse. Helpers: `is_single_use()` (enrolment tokens) and `is_compatibility_only()`
  (client passwords, so the UI can label them as such).
- **Passkeys** in `passkey.rs`, plus `PasskeySummary{id, label, created_at, last_used_at}`.
- **`Username` follows design 03 §4**, not design 01 §2.1: trim, lower-case, 1..=64,
  `[a-z0-9._@+-]`, must start alphanumeric, forbidden `}{"\,=/;<>`, reserved
  `anonymous`/`__anon__`/`rustak`/`takserver` and the `__` prefix. There is therefore no
  `Username::key()` — the value is already normalised. `eq_ignore_case(cn)` is provided for
  comparing against a certificate common name.

## Deviations, and why

1. **`uuid` cannot be inherited from the workspace table** (`rustak-api/Cargo.toml`). The workspace
   enables `uuid/v4`, and `v4` refuses to compile for `wasm32-unknown-unknown` without a randomness
   feature — so `cargo check -p rustak-api --target wasm32-unknown-unknown` failed outright. Cargo
   features are additive, so an inherited dependency cannot drop `v4`. The manifest now names the
   version directly: `uuid = { version = "1.26.1", default-features = false, features = ["std",
   "serde"] }`, which is what design 01 §1.2 asks for ("`uuid` here without `v4`/`js`"). **Open item
   for the orchestrator:** the tidier fix is to drop `v4` from the root `[workspace.dependencies]`
   table and have `rustak-server` ask for it, after which this override can go back to
   `workspace = true`. The root manifest is not this brief's file to change. The M0-01 comment in
   this manifest predicted the feature-unification hazard but assumed it would not bite; it does.
2. **`DeviceUid` allows interior spaces and brackets**, against design 01 §2.1's "no whitespace".
   CloudTAK enrols with `clientUid=<username> (ETL)` (research 03 line 230; design 03 §7 and its
   `enroll_json.rs` test expect the device uid `alice (ETL)`), so refusing whitespace would refuse
   CloudTAK. What is refused instead is what could forge a log line or an XML attribute: control
   characters, plus blank and over-256-character values. A test names CloudTAK as the reason.
3. **`Direction` keeps design 01's `Both`** alongside design 03's `{In, Out}`. Storage holds one row
   per single direction (design 03's `CHECK(direction IN ('IN','OUT'))`), so `Direction::expand()`
   and `GroupMembership::expand()` turn a `BOTH` grant into the pair. `Both` exists so that granting
   full access is one choice in the UI rather than two; it is documented as not a TAK wire value.
4. **Deserialisation validates, and `from_storage` does not.** Every string newtype runs its own
   rules in `Deserialize`, so a malformed value in a request body is refused at the edge rather than
   deeper in. Because that would make a row written under older rules unloadable, each type also has
   a non-validating `from_storage` for the database layer (the same split automate's `TenantId`
   makes, but the other way round — automate never validates on deserialise; here the wire matters
   more than the rollback risk, and `from_storage` covers the rollback risk).
5. **Secrets redact in `Debug`** (conventions: "`Debug` impls of key types redact"):
   `CredentialCreated`, `TokenResponse`, `TokenRefreshRequest`, `CreateAdminRequest`,
   `AdminCreated`. Each has a test asserting the secret does not appear in `{:?}` output but does
   survive the serde round trip, because it still has to reach the browser exactly once.

## Types and fields added beyond design 01 §2.1

Each is needed by an endpoint the design tables already describe:

- `PasskeyId` in the id family, and `PasskeyChallenge` as the shared response of both ceremony
  `start` endpoints — `passkey.rs` is a delta module, so neither is in design 01's list.
- `AdminCreated{username, registration_token, expires_in}` — M0-11 requires `POST /setup/admin` to
  return "a short-lived registration token so the wizard can register the admin's first passkey".
  `CreateAdminRequest` carries `setup_token` (design 03 §6) and no password.
- `Me.groups` (design 03 §6: `/api/v1/me` returns "user, groups, is_admin, method").
- `AuthMetadata.passkeys_enabled` — M0-13 asks the login page for "SSO button and/or passkey
  button", which a single `mode` cannot express.
- `TokenExchangeRequest.code_verifier` (design 03 §6: PKCE).
- `User.admin_override` (design 03 §4's model; lets the UI show whether admin came from the ACL or
  from an override), `Credential.username`/`created_by`, `CreateCredentialRequest.username`,
  `Certificate.username` — all needed for the administrator-acting-for-another-user routes.
- `Device.last_certificate_id` (design 03 §6: "device list with last seen/cert").
- `GroupMembership.source` + `MembershipSource` (design 03 §4), so the UI can grey out the
  memberships the identity provider overwrites at each sign-in.
- `CertificateSource` for design 03's `issued_via` column; `GroupSource` for its `GroupKind`,
  renamed to match design 01's field name `source`.
- `Health.message`, `ServiceSummary.metrics` — small, and the alternative was a degraded status with
  no reason attached.

`ComponentStatus` is modelled as the enum itself (`Ok`/`Degraded`/`Down`) rather than a struct, so
`Health.status` and `Health.database` are the same type, which is what design 01's signature
implies.

## Exit checks

```
$ cargo test -p rustak-api
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.11s
running 89 tests
test result: ok. 89 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

$ cargo clippy -p rustak-api --all-targets -- -D warnings
    Checking uuid v1.26.1
    Checking rustak-api v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-api)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.46s

$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-api --no-deps
 Documenting rustak-api v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-api)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.68s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html

$ cargo check -p rustak-api --target wasm32-unknown-unknown
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.14s

$ cargo fmt -p rustak-api -- --check
(clean, no output)
```

### `./scripts/check-file-length.sh`

The script currently aborts before it can report anything, for a reason unrelated to file length:

```
$ ./scripts/check-file-length.sh
awk: can't open file rustak-api/src/identity.rs
 source line number 5
```

It iterates `git ls-files`, and `rustak-api/src/identity.rs` is still tracked while having been
replaced on disk by `rustak-api/src/identity/`. The same thing happens for `rustak-core/src/config.rs`
and `rustak-core/src/identity.rs`, which the concurrent M0-04 agent replaced with directories, so
this is not specific to this brief. Staging the deletions fixes it, and briefs do not run `git`
write commands — **so this is left for the orchestrator's commit**, where the script will pass.

Running the script's own logic over the file set it will see after that commit (its `git ls-files`
output, minus the deleted path, plus this brief's new files) reports nothing and would exit 0.
Functional line counts for every file in this crate:

```
rustak-api/src/audit.rs                  116
rustak-api/src/auth.rs                   141
rustak-api/src/certificate.rs            105
rustak-api/src/credential.rs             105
rustak-api/src/device.rs                  39
rustak-api/src/error.rs                   19
rustak-api/src/group.rs                  100
rustak-api/src/health.rs                  57
rustak-api/src/identity/group.rs         111
rustak-api/src/identity/ids.rs            65
rustak-api/src/identity/mod.rs            12
rustak-api/src/identity/newtype.rs        63
rustak-api/src/identity/uid.rs           158
rustak-api/src/identity/username.rs       87
rustak-api/src/lib.rs                     43
rustak-api/src/passkey.rs                 39
rustak-api/src/service.rs                184
rustak-api/src/settings.rs                22
rustak-api/src/setup.rs                  103
rustak-api/src/user.rs                   107
```

## Open items

1. **Root manifest:** move `uuid/v4` off the workspace table and onto `rustak-server`, then restore
   `uuid = { workspace = true }` here (see Deviations 1). Until then this crate names a `uuid`
   version of its own, which is the only place in the workspace that does.
2. **Commit-time:** stage the deletion of `rustak-api/src/identity.rs` (and `rustak-core`'s two) so
   `scripts/check-file-length.sh` runs to completion in CI.
3. **For M0-11/M0-13:** `AuthMetadata` carries both `mode` and `passkeys_enabled`; the login page
   should read `passkeys_enabled` rather than inferring passkey availability from `mode`.
4. **For M0-07:** `Direction::Both` never reaches storage — expand a grant with
   `GroupMembership::expand()` before writing, and read memberships back as `In`/`Out` only.
5. The environment linked and ran native test binaries without trouble, so the blocker reported in
   `M0-01`'s status (Xcode Command Line Tools licence) no longer applies here.
