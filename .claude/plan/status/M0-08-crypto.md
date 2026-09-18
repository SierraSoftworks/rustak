# M0-08 — `crypto/` — complete

Brief: `.claude/plan/briefs/M0-08-crypto.md`
Read first: `.claude/plan/conventions.md`; `design/01-foundations-storage-ci.md` §5;
`research/01-automate-architecture.md` §3 (crypto description); `../automate/agent/src/crypto.rs`
(959 lines, lifted, never `../automate/.env`).

## What was built

`rustak-server/src/crypto/{mod,key,store,context,keyfile}.rs`, replacing the empty stub. `lib.rs`
was **not** touched — it already declared `pub mod crypto;`. Files owned, with functional-line
counts (limit 300):

| File | Lines | Contents |
|---|---:|---|
| `mod.rs` | 8 | module doc (design overview, adapted from automate's); `mod` declarations (private) + `pub use` re-exports of the public surface |
| `key.rs` | 121 | `KeyId`, `SecretKey` (`generate`, `from_encoded`, `to_encoded`, `id`, `duplicate`, `cipher`, `Drop`/zeroize, redacted `Debug`), `decode_key_bytes` |
| `store.rs` | 197 | `Sealed`, `SecretStore::{new, load, ephemeral, active_key_id, seal, open, seal_json, open_json, key_for}`, redacted `Debug` for both |
| `context.rs` | 34 | rustak's `SecretContext<'a>` (8 variants) + `Display` rendering `rustak/v1/<kind>/<id>` |
| `keyfile.rs` | 102 | `key_file_for`, `load_or_create_key`, `write_key_file` (0600), `warn_if_world_readable` |

23 unit tests in total, every file with its single trailing column-0 `#[cfg(test)] mod tests`.

## Brief requirements, point by point

- **Verbatim lift, split into four files.** Every function, struct and test in
  `automate/agent/src/crypto.rs` has a counterpart here. The split required two small mechanical
  additions not present in the single-file original, both scoped `pub(super)` (visible to `crypto`
  and its descendants, i.e. crypto's own submodules, never outside the module):
  - `SecretKey::duplicate()` — `SecretStore::new` used to build the second copy of the active key
    with a same-module struct literal (`SecretKey { bytes: active.bytes }`); across modules that
    needs the field, so a named method replaces the literal. `bytes` itself stays fully private to
    `key.rs`.
  - `SecretKey::cipher()` and the `B64` engine constant — used by both `key.rs` (encoding a whole
    key) and `store.rs` (encoding a nonce/ciphertext), so they are `pub(super)` instead of
    file-private.
- **`SecretStore::load` plain parameters.** Signature is exactly
  `load(secret_key: Option<&str>, previous_secret_keys: &[String], database_path: &Path)`, per the
  brief — **not** `&AuthConfig`. `rustak-server/src/config/auth.rs` (M0-06) landed in the working
  tree while this brief was in progress and does define `AuthConfig{ secret_key: Option<String>,
  previous_secret_keys: Vec<String>, .. }`, confirmed by reading (not editing) that file. The field
  names and types line up exactly with `load`'s parameters, so **M0-06 or M0-09 should add a thin
  wrapper** — `SecretStore::load(config.secret_key.as_deref(), &config.previous_secret_keys,
  database_path)` — rather than changing this module's signature. `crypto/` does not import
  `crate::config` anywhere.
- **Key-id domain.** `SecretKey::id()` hashes `b"rustak/secret-key-id/v1"` (was
  `b"automate/secret-key-id/v1"`).
- **Advice text.** Every occurrence of `[web.auth]` became `[auth]` (5 places: the empty-key error,
  the malformed-previous-key error, the unknown-key error, and both `load_or_create_key`
  file-access errors).
- **rustak `SecretContext` variants**, exactly as design 01 §5: `CaKey{certificate}`,
  `ServerCertKey{certificate}`, `ServiceCertKey{certificate}`, `AcmeAccount{account}`,
  `JwtSigningKey{kid}`, `MissionTokenKey{kid}`, `IdpRefreshToken{token}`,
  `ServiceSecret{service,key}`, rendered as `rustak/v1/<kind>/<id>` (`ca-key`, `server-key`,
  `service-key`, `acme-account`, `jwt-key`, `mission-key`, `idp-refresh`, `service-secret`). The
  design's comment for `IdpRefreshToken` reads `{jti}` while the field is named `token`: the field
  holds the refresh token's `jti`, not the token text, so the context can be rebuilt without first
  decrypting the token it names — documented on the variant. `ServiceSecret` had no literal rendered
  form in the design snippet, so it follows the two-segment shape automate's own `WebhookSecret`
  used: `rustak/v1/service-secret/{service}/{key}`.
- **Two extra tests.**
  - `store::tests::a_ciphertext_relocated_to_another_key_will_not_open` — seals under
    `CaKey{certificate: 1}`, opens under `CaKey{certificate: 2}`, asserts failure (the brief's
    "CaKey{1} vs CaKey{2}" case).
  - `store::tests::a_sealed_values_json_never_contains_the_plaintext` — seals a PEM-shaped secret,
    asserts the serialised `Sealed` JSON contains neither the secret substring nor `"PRIVATE KEY"`.

## Deviations beyond the brief's literal edit list, and why

The brief names five allowed edits (split, parameter change, key-id domain, `[auth]` advice,
`SecretContext` variants). Two more were necessary to avoid shipping wrong prose, not to change
behaviour:

- Every reference to **"Automate"/"the agent"** in doc comments and error messages now reads
  "rustak" (`${{ env.RUSTAK_SECRET_KEY }}` doctest string, envelope-version and downgrade messages,
  the `--env` advice line, `/var/lib/rustak/...` in the key-file test). Automate's literal product
  name would be a straightforward bug in this codebase.
- The **module doc's opening section** (automate's "single-tenant install" / "other people's
  Todoist tokens, GitHub credentials and webhook signing secrets" framing) is rewritten to describe
  what rustak actually seals — CA/server/service private keys, ACME accounts, JWT/mission signing
  keys, IdP refresh tokens, per-service secrets — since rustak is single-tenant and has none of
  automate's `Connection`/`WebhookSecret` concepts. The "Shape of the design" and "Key management"
  sections, which are domain-agnostic, are kept close to verbatim.
- `key_for`'s advice line ("the affected connections and webhook secrets must be recreated") became
  "the affected certificates and secrets must be recreated" for the same reason.
- **`SecretStore::ephemeral()` is `#[cfg(any(test, feature = "testing"))]`, not just
  `#[cfg(test)]`.** Design 01 §3.2 has `AppContext::new_mock` call `SecretStore::ephemeral()` and
  says it "mirrors automate"; that mock is meant to be reachable from the in-process integration
  tests under `tests/`, which compile as a separate crate and only see items gated by the crate's
  own `testing` feature (already declared in `rustak-server/Cargo.toml` for exactly this purpose) —
  a plain `#[cfg(test)]` item is invisible there. Automate does not need this because its
  integration tests live inside the same binary crate.
- One doc-comment intra-link had to be de-linked: automate's `SecretStore` doc points at
  `[`crate::services::Services::secrets`]`. `rustak-server/src/services/mod.rs` is still the M0
  stub (no `Services` trait yet), so that link is unresolvable today and `RUSTDOCFLAGS="-D
  warnings" cargo doc` would fail on it as soon as `db/`'s current errors clear (verified in the
  isolated crate below — this is the one thing in this module that would not otherwise be caught by
  `cargo check`, since broken intra-doc links only surface under `cargo doc`). Rewritten as plain
  code text: `` `Services::secrets` (added to `crate::services` by a later M0 brief)``. No other
  intra-doc link in these four files was affected.

## Dependencies

No `Cargo.toml` edit was needed. `rustak-server/Cargo.toml` already lists `aes-gcm`, `sha2`,
`base64`, `hex`, `rand`, `zeroize`, all as `{ workspace = true }`, and the workspace table already
pins the same versions automate uses (`aes-gcm 0.11.1`, `zeroize 1.9.0`, `human-errors 0.2.4`
confirmed by inspecting the registry checkouts). `rand` is not directly referenced by this module
(key/nonce generation goes through `aes_gcm::aead::Generate`, as in automate), same as before.

## Exit checks

`rustak-server/src/db/` and `rustak-server/src/config/` are being written concurrently by two other
agents (per the brief, not this module's files to touch), and both trees do not compile yet —
`db/repos/mod.rs` currently declares 13 submodules (`certificates`, `credentials`, `devices`,
`groups`, `members`, `oauth_keys`, `passkeys`, `refresh_tokens`, `revoked_jtis`, `services`,
`settings`, `stream_segments`, `users`) whose files do not exist yet. This blocks `cargo test -p
rustak-server`, `cargo clippy -p rustak-server` and `cargo doc -p rustak-server` for the whole
crate, independent of anything in `crypto/`. Retried four times over the course of this session
(the error set changed each time — `db/migrations.rs`, then `config/validate.rs`, then back to
`db/repos/mod.rs` — confirming active, unrelated, concurrent edits rather than a static failure).
The final retry's full error list, for the record:

```
$ cargo build -p rustak-server --lib 2>&1 | grep -E '^error' | sort | uniq -c
   1 error: could not compile `rustak-server` (lib) due to 13 previous errors
   1 error[E0583]: file not found for module `certificates`
   1 error[E0583]: file not found for module `credentials`
   1 error[E0583]: file not found for module `devices`
   1 error[E0583]: file not found for module `groups`
   1 error[E0583]: file not found for module `members`
   1 error[E0583]: file not found for module `oauth_keys`
   1 error[E0583]: file not found for module `passkeys`
   1 error[E0583]: file not found for module `refresh_tokens`
   1 error[E0583]: file not found for module `revoked_jtis`
   1 error[E0583]: file not found for module `services`
   1 error[E0583]: file not found for module `settings`
   1 error[E0583]: file not found for module `stream_segments`
   1 error[E0583]: file not found for module `users`
```

Every error names a `db/repos/*` path; none names anything under `crypto/`.

### Isolated verification

To get real pass/fail signal despite that blocker, the five files were copied into a throwaway
crate outside the repo (`crypto_verify`, in this session's scratchpad) depending on the real
`rustak-core`/`rustak-api` by path plus the same pinned leaf-crate versions, with the workspace's
exact `[lints]` table reproduced. This exercises the same code against the same dependency graph,
without `rustak-server`'s `db`/`config`/etc.

```
$ cargo test
running 23 tests
test context::tests::contexts_that_differ_only_by_id_render_differently ... ok
test context::tests::every_variant_renders_the_documented_form ... ok
test key::tests::a_key_of_the_wrong_length_is_rejected_with_its_actual_length ... ok
test key::tests::an_empty_key_is_diagnosed_specifically ... ok
test key::tests::an_unresolved_environment_expression_is_diagnosed_specifically ... ok
test key::tests::debug_output_never_reveals_key_material ... ok
test key::tests::key_ids_identify_keys_without_revealing_them ... ok
test key::tests::keys_are_accepted_in_the_encodings_operators_actually_paste ... ok
test keyfile::tests::a_configured_key_is_preferred_over_generating_one ... ok
test keyfile::tests::a_generated_key_is_persisted_and_reused_on_the_next_start ... ok
test keyfile::tests::an_empty_configured_key_falls_back_to_the_key_file ... ok
test keyfile::tests::the_key_file_sits_beside_the_database ... ok
test store::tests::a_ciphertext_moved_to_another_kind_of_record_will_not_open ... ok
test store::tests::a_ciphertext_relocated_to_another_key_will_not_open ... ok
test store::tests::a_sealed_value_opens_again_under_the_same_context ... ok
test store::tests::a_sealed_values_json_never_contains_the_plaintext ... ok
test store::tests::a_value_sealed_with_a_retired_key_still_opens ... ok
test store::tests::a_value_sealed_with_an_unknown_key_explains_what_to_do ... ok
test store::tests::debug_output_never_reveals_ciphertext_or_key_material ... ok
test store::tests::envelopes_survive_a_round_trip_through_storage ... ok
test store::tests::json_values_round_trip_through_the_envelope ... ok
test store::tests::sealing_the_same_value_twice_produces_different_ciphertext ... ok
test store::tests::tampering_with_the_ciphertext_is_detected ... ok

test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

exit status 0.

```
$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.92s
```

exit status 0 (workspace `[lints.rust]`/`[lints.clippy]` table reproduced exactly, including
`unsafe_code = "forbid"`, `rust_2018_idioms`, `dbg_macro`, `todo`, `print_stdout`,
`large_futures`).

```
$ RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
 Documenting crypto-verify v0.0.0 (…)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.48s
   Generated …/target/doc/crypto_verify/index.html
```

exit status 0. This run is what caught the broken `Services::secrets` intra-doc link described
above — it was silent under `cargo check`/`cargo test` and only surfaced here.

```
$ cargo fmt -p rustak-server -- --check rustak-server/src/crypto/{mod,key,store,context,keyfile}.rs
```

Two real diffs on the first run (an un-wrapped `ServiceSecret` variant and match arm in
`context.rs`, an un-wrapped `format!` call in `keyfile.rs`) — both were rustfmt's own line-length
wrapping, applied, and the check re-run clean. (The command also reports `db/repos/certificates.rs
does not exist`, from the same in-progress `db/` tree; that does not stop it from checking the
files actually passed to it.)

### `./scripts/check-file-length.sh`

```
$ ./scripts/check-file-length.sh
(no output)
```

exit status 0 — but this only means no *tracked* file regressed, since `git ls-files` does not see
untracked paths and none of `crypto/{key,store,context,keyfile}.rs` are tracked yet (same gap the
M0-04 status file records; no `but`/`git` write was made here to fix it, per this brief's "no
git/but writes"). The same check run explicitly over all five files, tracked and untracked:

```
rustak-server/src/crypto/mod.rs                  8
rustak-server/src/crypto/key.rs                121
rustak-server/src/crypto/store.rs              197
rustak-server/src/crypto/context.rs             34
rustak-server/src/crypto/keyfile.rs            102
over-limit=0
```

Largest file is `store.rs` at 197 functional lines, comfortably under the 300 limit.

## Notes for later briefs

- **M0-06/M0-09**: add `impl AuthConfig { pub fn secret_store(&self, database: &Path) ->
  Result<SecretStore, human_errors::Error> { SecretStore::load(self.secret_key.as_deref(),
  &self.previous_secret_keys, database) } }` (or an equivalent free function in
  `services/mod.rs`'s bootstrap) rather than changing `SecretStore::load`'s signature.
- **`services` module**: once `Services::secrets(&self) -> &SecretStore` exists, consider
  restoring the doc-linked form (`[`Services::secrets`]`) on `SecretStore`'s doc comment — it is
  currently plain code text specifically because the link does not resolve yet.
- `SecretStore::ephemeral()` needs `--features testing` (or a unit-test build) to exist at all; a
  caller outside those two contexts will get "no function `ephemeral`" rather than a runtime panic,
  which is the intended failure mode.
