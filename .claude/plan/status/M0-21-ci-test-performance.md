# M0-21 — Making the test suite finish under coverage on CI, and the new clippy failure

**Status:** delivered. The brief's mechanism was right and one of its three suspects was already innocent:
`pki::testing::TestAuthority` has been elliptic-curve (P-256) for a while, so it was never the cost. What made
the Test job unfinishable is RSA key generation — one 2048-bit key per `TestServer`, and there are 241 of them —
multiplied by `-Cinstrument-coverage`, which instruments `rsa` and `num-bigint-dig` whatever
`[profile.dev.package]` says about optimising them. argon2id at production cost is the second, smaller half.

A late addition from the coordinator also landed here, because it is in a file this brief owns: the enrolment
`nameEntry` padding that broke the first real commoncommo run (§3).

**Toolchain caveat (correction to the brief).** This machine cannot reach `static.rust-lang.org`, so
`rustup update stable` could not run: every check below is on **rustc/clippy 1.96.0 (ac68faa20c 2026-05-25)**,
not the newer stable CI uses. 1.96 does not raise `result_large_err` at this size, so the fix in `marti/tls.rs`
is applied blind and **CI's Lint job is what confirms it**. Anything else the newer clippy reports across the
workspace could not be seen from here either.

**Concurrency caveat.** Three other agents were editing `missions/**`, `marti/missions/**`, `stream/**` and
`tests/mission_dest.rs` throughout, and their tree was mid-edit (at one point unparseable) when the final checks
ran. Every check that stops short below stops on their files, never on one of this brief's.

---

## 1. Before and after

Measured in an isolated copy of the tree under the session scratchpad, each configuration with its own target
directory, so that the *only* difference between a before run and an after run is this brief's files — nobody
else's edits move between the two. (A first attempt in the working tree was discarded: it timed a cargo
invocation that also had to recompile another agent's changes.) The machine was busy throughout — load average
60–80, three other agents compiling — so the absolute seconds are pessimistic and the ratios are the finding.
`user` is CPU-seconds, which is the number that matters on a 2-vCPU runner.

### Under `RUSTFLAGS=-Cinstrument-coverage` — the CI configuration

| `cargo test -p rustak-server --features testing` | Wall | CPU | Tests over 60 s |
|---|---|---|---|
| **before** | **did not finish** — stopped by hand at **5 min 10 s**, with **21 of 1338** library tests passed | — | 18 already |
| after, key + argon2 + profile | 338.1 s | 2489.6 s | 19 |
| **after, and `auth::tokens`'s helper too** | **193.0 s** | **1263.6 s** | 12 |

| Subset, `--lib <filter>` | Before wall | Before CPU | After wall | After CPU |
|---|---|---|---|---|
| `auth::jwt::` (17 tests) | 333.4 s | **2435.7 s** | **16.6 s** | **30.9 s** (79× less) |
| `web::api::groups::` (11 tests) | 215.4 s | **1616.5 s** | **2.6 s** | **4.3 s** (376× less) |

The CPU columns are the point. Those two subsets are 28 tests of ~1400, and before they alone burned
**4052 CPU-seconds** — more than a 2-vCPU runner has in a whole 30-minute job. After, they cost 35.

### Without instrumentation

The same package, same snapshot, same target directory. Both runs stop after the library target, because another
agent's `missions` tests fail there; the comparison is like for like.

| `cargo test -p rustak-server --features testing` (library target) | In-test | Wall | CPU |
|---|---|---|---|
| before | 22.76 s | 26.30 s | 87.40 s |
| after | **16.05 s** | **17.63 s** | **23.89 s** (3.7× less) |

### What is still slow under instrumentation, and why it stays

Twelve tests still take over 60 s and every one generates an RSA key *because that is what it is testing*:
`pki::{ca,csr,issue,keys}`'s RSA cases (6), `auth::jwt`'s rotation and "signed by another server" (2),
`lib.rs`'s two first-start tests, and whichever test in a process first touches the `LazyLock` keys in
`testing::authenticator` and `testing::oidc` — both of which were already once-per-process, so there was nothing
to fix there. Caching these away would mean not testing them.

## 2. What landed

| File | Change |
|---|---|
| `rustak-server/src/testing/keys.rs` | **New.** One `LazyLock<Vec<u8>>` holding an RSA-2048 key in PKCS#8 DER, generated the first time a test in the process asks. Same crate, same size, same encoding the production path seals — only the *number* of generations changes, from one per `TestServer` (241) to one per test binary (12). |
| `rustak-server/src/auth/jwt.rs` | **Additive.** `JwtIssuer::load_or_adopt(db, secrets, auth, base_url, pkcs8)` behind `#[cfg(any(test, feature = "testing"))]`: when the database holds no key it stores the caller's rather than generating one, then falls through to `load_or_create`. `create_key` was split so the sealing-and-recording half (`store_key`) is shared — the adopted key is sealed under that test's own ephemeral secret store and written to that test's own `oauth_keys` row exactly as a generated one would be. The file's own tests moved onto the shared key too, except the two that are *about* generation. |
| `rustak-server/src/testing/context.rs` | `TestServer` adopts the shared key and calls `password::use_testing_params()`. Databases, data directories, secret stores, content stores and rate limiters stay per test — isolation is unchanged. |
| `rustak-server/src/testing/mod.rs` | Declares `pub mod keys`. |
| `rustak-server/src/auth/tokens.rs` | Its test helper adopts the shared key (§5 — outside the brief's file list, worth 7 of the remaining slow tests and 145 s of the full instrumented run). |
| `rustak-core/src/identity/password.rs` | `Params` (`PRODUCTION` m 19 MiB / t 2 / p 1; `TESTING` m 8 MiB / t 1 / p 1), `Params::active()`, and `use_testing_params()` behind `#[cfg(any(test, feature = "testing"))]`. `hash` builds its hasher from `Params::active()`. `verify` is deliberately untouched: a PHC string carries the parameters its hash was made at, which is what makes the switch safe — a row written cheaply verifies anywhere, and the existing test asserting `m=19456`/`t=2` still passes unchanged. |
| `rustak-server/Cargo.toml` | `testing = [… , "rustak-core/testing"]`, so `use_testing_params` exists when the server's tests are built. `cfg(test)` in `rustak-core` is not active when another crate compiles it, so a feature is the only way through. |
| `Cargo.toml` | `[profile.dev.package.argon2]` and `[profile.dev.package.blake2]` at `opt-level = 3`, beside the existing `rsa`/`num-bigint-dig` pair. `blake2` matters more than `argon2` itself — it is argon2's compression function, so that is where the loop is. `sha2` was left alone; nothing showed it near the top. |
| `rustak-server/src/marti/tls.rs` | The clippy fix (§ below) and the `nameEntry` padding fix (§3). |
| `rustak-server/src/config/validate.rs` | One more line of advice on `[pki] name_entries`: neither half may be blank, and why (§3). |
| `.claude/plan/compat/enrollment.md` | §1 gains the "a `nameEntry` value must be non-empty" rule (§3). |
| `.github/workflows/rust.yml` | `timeout-minutes` on all twelve jobs; coverage kept on; the stale `interop-node-tak` comment (still listing M2-06 among the milestones whose scenarios skip) corrected. |
| `docs/ci.md` | New "Keeping the test job inside its timeout" section: why instrumentation costs this much, the three levers that pay for it, and why instrumenting only the workspace is not available on stable. The job-graph list gains the timeout summary. |

### The clippy fix

`caller` in `marti/tls.rs` now returns `Result<(Resolved, Arc<Pki>), Box<HttpResponse>>` and its three call
sites `return Ok(*response)`. Boxing rather than switching to `MartiError` keeps the plain-text
`WWW-Authenticate` contract exactly where it was — `refusal()` is untouched, so the 401 still carries
`Content-Type: text/plain` and the Basic challenge, and the existing tests assert the same bytes. This is the
change 1.96 cannot verify; CI's Lint job is the check.

### Why the cheap argon2 cost cannot reach a deployment

Three independent reasons, because this is the one change that weakens something: the setter does not exist
without the `testing` feature; the only caller anywhere is `TestServer::start_with`, which a server never
builds; and the default is `PRODUCTION` unless somebody calls it, so even a binary compiled with
`--features testing` hashes at full cost.

## 3. The enrolment fix (coordinator's late addition)

`certificate_config()` padded the name entries to the two elements CloudTAK's `xml-js` parser needs by pushing
`("OU", "")`. commoncommo's `generateCSR` hands each value to OpenSSL's `X509_NAME_ENTRY_create_by_NID`, whose
directory-string minimum for `OU` is 1, so every enrolment on the first real commoncommo run (nightly
35369773295) died at `EnrollUpdate: step 1 … status 14 (CSR generation failed using provided parameters)`.

The padding now carries a **non-empty** value — the organisation, which is also TAK Server's own default shape
(`O=TAK` beside `OU=TAK`). The logic moved into `padded_entries(entries, organization)` so it can be tested as
data rather than through a whole `Pki`:

- nothing configured → `O=<organization>`, `OU=<organization>`;
- one entry → that entry plus `OU=<organization>`;
- two or more → exactly as configured;
- `organization` blanked as well → the first configured entry's value, and failing that the shipped default
  `"rustak"`. Unreachable in practice (validation refuses a blank `name_entries` value) but the invariant is
  held here unconditionally rather than on the strength of a check in another module.

`config::validate` already rejected a blank value; it now says *why* ("an EUD builds its signing request from
these entries, and OpenSSL refuses a zero-length subject component"). The CloudTAK ≥2 rule is unchanged and
still asserted, by the new unit test, by `tests/enroll_flows.rs` and by the node-tak suite.
`.claude/plan/compat/enrollment.md` §1 records the OpenSSL rule beside the `xml-js` one.

New test: `marti::tls::tests::the_name_entries_are_padded_to_two_and_never_to_an_empty_value`, which walks all
five shapes above and asserts both invariants (≥2 entries, no empty value) on each. There was no existing test
pinning the empty value — only `tests/enroll_flows.rs`'s "at least two `<nameEntry>`" count, which still holds.

## 4. Exit checks

Run after the other agents' trees had settled; every check that still stops, stops on their files.

```
$ cargo fmt --all --check
clean, exit 0

$ scripts/check-file-length.sh
clean, exit 0
  auth/jwt.rs 294 functional lines (was 268 — six to spare, worth watching)
  marti/tls.rs 220, identity/password.rs 120, auth/tokens.rs 134, pki/testing.rs 104

$ cargo test -p rustak-server --features testing --lib
test result: ok. 1357 passed; 0 failed; 2 ignored; 0 measured; finished in 16.51s

$ cargo test -p rustak-server --features testing --test bootstrap --test enroll_flows \
    --test enroll_oauth --test marti_channels --test marti_contract --test profiles_contract \
    --test stream_routing --test stream_session --test stream_store --test sync_contract
exit 0 — ten suites, 102 tests, all passing:
  3, 10, 10, 9, 14, 14, 11, 10, 8, 13

$ cargo test -p rustak-server --features testing
Could not build: tests/mission_dest.rs (another agent's, in flight) does not compile
against their in-progress `stream::mission_notify` — four errors, E0432/E0603. That
is the one integration binary missing from the list above; the brief's instruction
for exactly this case was to run filtered, which is what the two commands above are.

$ cargo clippy --workspace --all-targets -- -D warnings
FAILS on two of theirs, never on this brief's:
  rustak-server/src/marti/missions/layers.rs:179  needless_borrow
  rustak-server/src/stream/mission_payload.rs:220 wrong_self_convention
Re-run with those two lints allowed so every target compiles — including the test
targets, where this brief's new test code lives — and no diagnostic points at a file
listed in §2 (the rest are also theirs: missions/archive.rs:481, missions/logs.rs:414
and :462, tests/mission_dest.rs).
  $ cargo clippy -p rustak-core --all-targets -- -D warnings   → clean, exit 0

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
FAILS on three of theirs: marti/missions/mod.rs:17, missions/notify.rs:36,
auth/mission_token.rs:18. Documented again with `-p rustak-server --features testing`
so this brief's `testing/**` items and their intra-doc links are actually built:
same three, none of mine. `cargo doc -p rustak-core --no-deps` clean.
(Two of my own doc links were caught and fixed on the way: `use_testing_params` is
cfg'd out when rustak-core is documented without the feature, so it is named in prose
rather than linked.)
```

The 1357 library tests and the 102 integration tests include every suite this brief touched —
`auth::{jwt,tokens}`, `marti::tls`, `config::*`, `testing::*`, `pki::*`, `identity::password`, and the
enrolment flows end to end.

## 5. Deviations from the brief

1. **No `TestAuthority` cache.** The brief expected an RSA-2048 root; `pki/testing.rs` already builds its
   authority with `KeyType::EcdsaP256`, which is microseconds of keygen. What is left per authority is an
   in-memory database and its migrations, shared with every other test that opens one, and caching *that* would
   mean re-implementing `pki::ca`'s sealed-storage path inside `pki/testing.rs` — a non-owned module's
   internals — for a cost that does not appear in the profile. `TestAuthority::new()` is unchanged, and no
   `fresh()`/`foreign()` split was needed because no test needed a second authority cheaply.
2. **Three files edited that the brief did not list.** `rustak-server/Cargo.toml` needed one line
   (`"rustak-core/testing"`) or `use_testing_params` would not exist for the server's tests.
   `rustak-server/src/auth/tokens.rs`'s test helper was most of what remained slow after the first pass — a
   three-line, test-only change worth 145 s of the instrumented run. `rustak-server/src/config/validate.rs` and
   `.claude/plan/compat/enrollment.md` are the coordinator's §3 request. None is touched by another agent
   (checked against `git status` immediately before each edit).
3. **Timeouts on the heavy jobs are 30, not 20.** The brief said "test 30, build 45, others 20"; `ui`, `e2e`,
   `interop-node-tak` and `docker-build` each compile or image something substantial from a possibly cold
   cache, and a timeout that fires on a slow-but-healthy run is worse than no timeout. Bookkeeping jobs took 10.
4. **`RUSTFLAGS` unchanged.** The brief allowed narrowing instrumentation to workspace crates if items 1–2 were
   not enough. They were (193 s for the whole package), and narrowing is not actually available on stable:
   `-Cinstrument-coverage` is per-invocation, and per-package `rustflags` is nightly-only behind
   `-Zprofile-rustflags`. `cargo llvm-cov` does not change that — it sets the same flag and filters at report
   time, as `grcov` already does. `docs/ci.md` records this so the next person does not go looking.
5. **`auth/jwt.rs` is now 294 functional lines** against the 300 limit. Within the rule, but the next addition
   to that file will need the `SigningKey` half split out into its own module.
6. **The node-tak interop suite was not extended** with a non-empty `nameEntry` assertion to match §3;
   `interop/**` belongs to M2-09, who is editing it concurrently. Worth a line in their suite when they land.
