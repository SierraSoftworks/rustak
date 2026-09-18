# M0-20 — remove native OpenSSL from the passkey stack — complete

Brief: `.claude/plan/briefs/M0-20-pure-rust-webauthn.md`
Read first: `conventions.md`; status files `M0-11` (where the passkey ceremonies came from) and
`M0-18` (the resident-key patch and the username fallback this replaces properly).

No `git`/`but` commands were run.

## The decision: `webauthn_rp` 0.3, not an in-house verifier

The brief's approach (1) was to evaluate `webauthn_rp` 0.3 and use it if it covers the contract. It
does, so route (1) it is. The evaluation, so the next person does not have to repeat it:

| Contract clause | How `webauthn_rp` covers it |
|---|---|
| Pure Rust, no `openssl` | `p256`, `p384`, `rsa`, `ed25519-dalek`, `data-encoding`, `precis-profiles`. `cargo tree -i openssl-sys` is empty on every CI target. |
| attestation `none`, `residentKey: "required"` + `requireResidentKey: true`, `userVerification: "required"` | `PublicKeyCredentialCreationOptions::passkey` **is** that profile. M0-18's patch-the-serialised-JSON work-around is deleted, not ported. |
| ES256 / RS256 / EdDSA | `CoseAlgorithmIdentifiers::ALL.remove(Es384)`. All four are supported; ES384 is dropped so the advertised list is exactly what the API documents. |
| exclude list of the user's existing credentials | `PublicKeyCredentialCreationOptions::passkey(rp_id, user, exclude)`. |
| discoverable **and** username-assisted sign-in | `DiscoverableCredentialRequestOptions` and `NonDiscoverableCredentialRequestOptions`. |
| sign-count regression: fail only from a non-zero stored value | `SignatureCounterEnforcement::Fail` is literally `if prev == 0 \|\| cur > prev { ok }`. It is the default; it is named explicitly anyway, because it is a security decision and not a default worth inheriting silently. |
| origin check with today's semantics, including the loopback any-port relaxation | `DomainOrigin { scheme, host, port }` with `Port::Any` for `localhost`/`*.localhost` and `Port::Val`/`Port::None` everywhere else. |
| ceremony state in the existing DB-backed store with the same TTL | the `serializable_server_state` feature makes the three server-state types `Encode`/`Decode`; the bytes go in the same `auth_state` record, base64url, under the same five-minute TTL. |
| the browser's JSON, unchanged | the `serde_relaxed` feature. `RegistrationRelaxed` / `AuthenticationRelaxed` ignore unknown keys and do not require `response.publicKey`, which is what lets `rustak-ui`'s payload (`extensions: {}` instead of `clientExtensionResults`, no `getPublicKey()` result) be accepted without touching the UI. The strict `Registration::deserialize` would have rejected it, so this feature is load-bearing rather than convenience. |
| credential rows keep their schema | they do; no migration. |

**Trade-offs accepted.**

- The crate has one maintainer and its own git host, and its strictness (CTAP2 canonical CBOR,
  RFC 8265/8266 name profiles, exact interrelated-data matching) is a compatibility risk with an
  authenticator in the field that we cannot see. Two places soften it deliberately, both commented
  where they happen: `error_on_unsolicited_extensions` is `false` (browsers return extension outputs
  we did not ask for, most often `credProps`; refusing those is a sign-in failure with no security
  benefit, and nothing here reads an extension it did not request), and `credProtect` is set to
  `CredProtect::None` (the `passkey()` profile asks for `userVerificationRequired` **and permits the
  client to enforce it**, which makes a browser fail the whole ceremony against an authenticator
  that does not implement the extension — `userVerification: "required"` in the authenticator
  selection is what actually binds the credential to a verified user, so the extension bought
  nothing and cost compatibility).
- A display name RFC 8266 will not carry is dropped rather than failing the ceremony. Usernames go
  through `Username::try_from` and *do* fail the ceremony, which is safe: rustak usernames are
  `[a-z0-9._@+-]`, which RFC 8265's UsernameCasePreserved accepts.
- It brings `p384` and `precis-profiles` into the tree, and a second `rand` major version. Against
  `openssl-sys`, that is nothing.
- **An in-house verifier was not written.** Roughly 600 lines of CBOR, COSE and three signature
  paths that we would own the correctness of for ever, when a maintained crate that is stricter than
  the specification already exists, would have been the worse trade even though the brief sanctioned
  it.

## Files

| File | Functional lines | What it is now |
|---|---:|---|
| `rustak-server/src/auth/passkeys/mod.rs` | 135 | **new** (replaces `auth/passkeys.rs`): `Passkeys` — the relying-party identity, the origin list, the two verification-option builders, `to_summary`, the shared refusals |
| `rustak-server/src/auth/passkeys/register.rs` | 141 | **new**: `start_registration` / `finish_registration`, the exclude list, the two deviations from the `passkey()` profile |
| `rustak-server/src/auth/passkeys/login.rs` | 165 | **new**: `start_login` / `finish_login`, both ceremonies, and the credential-id lookup that selects the row to verify against |
| `rustak-server/src/auth/passkey_store.rs` | 173 | rewritten below the ceremony store: `StaticState`/`DynamicState` ↔ `PasskeyRow`, `UserHandle16`, transports, base64 ceremony state |
| `rustak-server/src/testing/authenticator/mod.rs` | 226 | **new** (replaces `testing/authenticator.rs`): the software authenticator, plus seven builders that each tell exactly one lie |
| `rustak-server/src/testing/authenticator/keys.rs` | 120 | **new**: ES256 / RS256 / Ed25519 / an unoffered algorithm, COSE keys and the attestation object, via `ciborium` |
| `rustak-server/src/web/api/passkey.rs` | 284 | handlers **unchanged**; seventeen tests added (13 → 30), one comment corrected |
| `Cargo.toml`, `rustak-server/Cargo.toml` | — | `webauthn-rs` out; `webauthn_rp` in; `p256`, `ed25519-dalek`, `ciborium` added for the software authenticator (optional, behind `testing`, and dev-dependencies too, exactly as `tempfile`/`wiremock` already are) |
| `docs/deployment.md` | — | the algorithm list, the sign-count rule, and the upgrade note below |

`auth/passkeys.rs` and `testing/authenticator.rs` became directories. Both are modules the brief
names; no `mod` declaration outside them changed, so `auth/mod.rs` and `testing/mod.rs` — which
other agents are in — were not touched.

## What changed that an operator can see

1. **Passkeys registered by an earlier build cannot be read.** The table and its columns are
   unchanged, but `passkeys.public_key` now holds `StaticState::encode()` rather than a serialised
   `webauthn_rs::Passkey`. An affected sign-in is refused the way every other unusable passkey is
   (401, the same message as every other cause) and the log says the credential "may have been
   written by a different version of rustak". Written up in `docs/deployment.md` under
   **First-run setup and sign-in**, with what to do about it. A migration was considered and
   rejected: the two encodings are different libraries' internals, so a converter would have to
   re-derive a COSE key from a JSON blob whose shape we would then have to keep a copy of for ever,
   for a project with no released version.
2. **Nothing else.** The request/response JSON is the same, the algorithms offered are the same,
   `residentKey`/`userVerification` are the same, the sign-count rule has the same effect, and the
   thirteen Playwright specs — which drive a real Chromium virtual authenticator — pass unchanged.

`rp.name` is the one thing that needed putting back by hand: `webauthn_rp` serialises it as the
relying-party *identifier* (a host name, where the browser prompt has room for a sentence), and
`[server] name` is what was there before and what an operator configured. It is read by nothing but
the prompt — it never comes back, and no check looks at it — so `name_the_installation` sets it on
the way out, with that reasoning at the function.

## What is now tested

The existing suite passed **on the first run after the swap**, including the two ceremonies end to
end, the phishing-origin refusal and the cloned-counter refusal. What the brief asked for on top —
a negative test for every check the specification requires of a relying party — is new:

| Check | Test |
|---|---|
| challenge freshness, registration | `a_registration_for_a_challenge_nobody_issued_is_refused` |
| challenge freshness, assertion | `an_assertion_for_a_challenge_nobody_issued_is_refused` |
| challenge replay | `a_challenge_is_good_for_exactly_one_attempt`, `a_ceremony_is_good_for_exactly_one_attempt` |
| origin | `an_assertion_from_another_origin_is_refused`, `a_registration_run_from_somewhere_else_is_refused` |
| RP ID hash (the half a browser does not police) | `an_assertion_signed_over_another_relying_party_is_refused`, and the `at_rp_id` arm of the registration test |
| `type` (`webauthn.create` vs `webauthn.get`) | `a_ceremony_labelled_as_the_other_one_is_refused` |
| `crossOrigin` | `a_ceremony_run_inside_somebody_elses_frame_is_refused` |
| UP flag | `an_assertion_nobody_was_present_for_is_refused`, and the registration arm |
| UV flag | `an_assertion_nobody_verified_is_refused`, and the registration arm |
| signature over `authData ‖ SHA-256(clientDataJSON)` | `an_assertion_whose_signature_covers_something_else_is_refused` |
| COSE algorithm allow-list, positive | `every_algorithm_the_registration_offers_can_actually_sign_somebody_in` (ES256, RS256, EdDSA, each a whole ceremony) |
| COSE algorithm allow-list, negative | `a_credential_using_an_algorithm_we_never_offered_is_refused` (a real P-256 key labelled `-65535`) |
| sign-count regression | `a_cloned_authenticator_is_caught_by_its_counter` |
| ceremony-kind confusion | `a_registration_handle_cannot_be_spent_on_a_sign_in`, `a_sign_in_handle_issued_for_one_account_cannot_sign_in_another` |
| one credential, two accounts | `the_same_passkey_cannot_be_registered_twice` (and it asserts *why* it was refused) |
| `id` and `rawId` disagreeing | `an_assertion_whose_two_identifiers_disagree_is_refused` — the seam between the row this code selects and the assertion the library verifies |
| the wire shape the UI depends on | `the_options_a_browser_is_handed_are_the_ones_the_ui_knows_how_to_read` |
| CTAP2 canonical CBOR in the test authenticator itself | `every_cose_key_is_in_ctap2_canonical_order` |

Every negative test sits beside a positive control in the same test or the same file, so a refusal
is the lie under test rather than something else about the server. Nothing was weakened to make a
test pass; the two softenings above are in the production path and are argued at the point of use,
not in a test.

## Exit checks

```
$ cargo tree -i openssl-sys --workspace
error: package ID specification `openssl-sys` did not match any packages

$ cargo tree -i openssl --workspace
error: package ID specification `openssl` did not match any packages

# and for each target the CI cross-build uses:
$ for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl \
           x86_64-pc-windows-msvc aarch64-apple-darwin; do
      cargo tree --target "$t" -i openssl-sys --workspace; done
error: package ID specification `openssl-sys` did not match any packages   (×4)

$ cargo test -p rustak-server --features testing
test result: ok. 1067 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out   # lib
test result: ok. 3 passed   …   10 … 10 … 14 … 11 … 10 … 8 passed              # tests/*
test result: ok. 5 passed                                                       # doc-tests
# Whole crate, not filtered: the other agents' trees happened to be in a
# compiling state. Nothing in this brief needed a filtered run in the end.

$ cargo test -p rustak-server --features testing --lib -- web::api::passkey auth::passkey testing::authenticator
test result: ok. 55 passed; 0 failed; 0 ignored; 0 measured; 1014 filtered out

$ cargo test --workspace --features rustak-server/testing
# every crate green; no `test result: FAILED` anywhere

$ cargo clippy -p rustak-server --all-targets --features testing -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 19.08s

$ cargo clippy --workspace --all-targets --features rustak-server/testing -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 23.32s

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
(no output)

$ cargo fmt --check
(no output)

$ ./scripts/check-file-length.sh
(no output, exit 0)

# The new files are untracked, so `check-file-length.sh` (which walks git) does
# not see them yet; measured by hand with the same awk:
rustak-server/src/auth/passkeys/mod.rs:           135
rustak-server/src/auth/passkeys/register.rs:      141
rustak-server/src/auth/passkeys/login.rs:         165
rustak-server/src/auth/passkey_store.rs:          173
rustak-server/src/testing/authenticator/mod.rs:   226
rustak-server/src/testing/authenticator/keys.rs:  120
rustak-server/src/web/api/passkey.rs:             284

$ cd rustak-ui && trunk build
2026-09-18T14:30:59 INFO ✅ success

$ cd e2e && npm run typecheck
> tsc --noEmit
(no output, exit 0)

$ RUSTAK_E2E_CHROMIUM="…/chromium-1234/…/Google Chrome for Testing" npx playwright test
Running 13 tests using 1 worker
  ✓   1 [setup] › setup.spec.ts:29 › the first-run wizard turns a token on disk into an administrator who can sign in (1.1s)
  ✓   2 [setup] › setup.spec.ts:124 › the wizard closes itself for good once it has been completed (306ms)
  ✓   3 [chromium] › auth.spec.ts:31 › a browser holding no passkey for this server cannot sign in, and is not told why (485ms)
  ✓   4 [chromium] › auth.spec.ts:50 › a passkey registered for one host is refused at another (508ms)
  ✓   5 [chromium] › auth.spec.ts:86 › an administrator signs in with a passkey, and signing out ends the session (459ms)
  ✓   6 [chromium] › auth.spec.ts:140 › a passkey the browser cannot offer on its own is reached by naming the account (502ms)
  ✓   7–13 navigation.spec.ts, smoke.spec.ts
  13 passed (9.2s)
```

`npx playwright install chromium` still cannot reach `cdn.playwright.dev` from this machine, so the
run above used the cached Chrome for Testing through the `RUSTAK_E2E_CHROMIUM` knob M0-14 added for
exactly that. **CI leaves it unset.** The e2e launcher started the server without the
`[pki] server_names` work-around M0-18 needed, so whoever owned `runtime.rs` has since fixed that.

`cargo deny` / `cargo audit` were not required and were not run. `webauthn_rp` is MIT OR Apache-2.0,
as are `p256`, `ed25519-dalek` and `ciborium`, so the licence set is unchanged.

## Deviations from the brief

1. **`auth/webauthn/**` was not created.** It was the brief's suggestion for the in-house route
   (approach 2), which was not taken. The library route needed a split of `auth/passkeys.rs`
   instead, because the two ceremonies plus the relying-party identity do not fit in 300 lines.
2. **`p256`, `ed25519-dalek` and `ciborium` were added** beyond "what you need" for the verifier
   itself. They are test-only (optional behind `testing`, and dev-dependencies), and they exist so
   that the COSE algorithm allow-list is *exercised* rather than asserted — an allow-list nothing
   runs against is an allow-list nobody knows works. All three are already in the tree through
   `webauthn_rp` and `ciborium`'s own dependencies, so nothing new is compiled for the binary.
3. **`web/api/passkey.rs` was touched**, which the brief allows "only if the library swap forces a
   change". The handlers did not change at all — `Passkeys`' public signatures are identical. What
   changed is its `#[cfg(test)] mod tests`: seventeen new tests (13 → 30), and one comment that said
   `webauthn-rs` asks for `discouraged`.
4. **`rp.name` is patched onto the serialised options.** Described above. It is the same *kind* of
   thing M0-18 had to do for `residentKey`, but for a field that is decoration rather than a
   security control, and the reason is written at the function rather than in a status file.

## Notes for the orchestrator and the briefs that follow

- **`auth::passkeys::require_discoverable` is gone.** M0-18's note that it "is a patch on
  `webauthn-rs`'s output — worth revisiting when `webauthn-rs` 0.6 stabilises" is now answered: the
  resident-key and user-verification requirements come from the library's own passkey profile, so
  any future ceremony gets them by construction.
- **`passkey_store::{static_state_of, dynamic_state_of, credential_id_of, transports_of}`** are the
  four functions any new credential ceremony should build an `AuthenticatedCredential` from.
  `user_verified` and `authenticator_attachment` are deliberately not columns — the reasons are at
  the top of that file.
- **`SoftAuthenticator` now has seven ways to misbehave** (`at_origin`, `at_rp_id`,
  `without_user_verification`, `without_user_presence`, `with_tampered_signature`, `in_a_frame`,
  `mislabelling_the_ceremony`) and three real algorithms plus one unoffered one. Anything else that
  verifies a WebAuthn assertion should reuse it rather than growing a second one.
- **The `passkeys.public_key` encoding changed**, so any tooling that reads that column directly
  (there is none today) needs to know it is `webauthn_rp`'s binary `StaticState`, not JSON.
