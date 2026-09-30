# M10-17 — Generate the test keys once per run, not once per test

**Why — the first `Test` job under nextest, run 36648789737 on 03b09f0e (2026-09-30).** `Run tests` took 18m17s (3m30s compiling, 881 s running 3,866 tests) and `Run doctests` 55 s, against 23m44s for `cargo test` on the commit before: about four and a half minutes saved, at the pessimistic end of M10-16's projection. The runner's four slots were fully busy (test time summed to 3,464 s ≈ 4 × 866 s), so what is left is CPU, and the log says where it goes:

| Binary | Tests | Summed time | Per test |
|---|---|---|---|
| `rustak-server` (library) | 2,056 | 1,556 s | 0.76 s |
| `oidc_provider` | 24 | 192 s | 8.0 s |
| `workload_identity` | 7 | 150 s | 21 s |
| `cloudtak_onboarding` | 20 | 134 s | 6.7 s |
| `oauth_flows` | 31 | 131 s | 4.2 s |
| `api_v1_map` | 16 | 95 s | 5.9 s |

248 tests take five seconds or more. A test that starts a server generates its RSA-2048 keys — instrumented, on a runner core — because under nextest each test is a process and the `LazyLock`s in `rustak-server/src/testing/{keys,oidc,workload}.rs` and `testing/authenticator/keys.rs` are per process. M10-07 found the same cost from the other side: `stream_support::Harness` builds its context through `build_context` → `JwtIssuer::load_or_create` and so generates a key per harness even under `cargo test`. Separately, `auth::ratelimit::tests::a_flood_of_lockouts_does_not_make_every_later_check_pay_for_it` took 42 s on the runner.

**Read first:** `M10-00-wave-rules.md`; status notes M10-16 (figures, the per-binary model) and M10-07; `docs/ci.md` "Keeping the test job inside its timeout" and "Why the tests run under nextest"; `rustak-server/src/testing/{keys,oidc,workload}.rs`, `testing/authenticator/keys.rs`, `testing/context.rs`; `rustak-server/tests/stream_support/mod.rs`; `rustak-server/src/auth/tokens.rs` (`load_or_create`, `load_or_adopt`); `.config/nextest.toml`.

**Deliver.**
1. **One set of test keys per test *run*, shared across processes.** No private key is checked into the repository. The keys are generated on first use and kept in a file under the build's own temporary directory (`CARGO_TARGET_TMPDIR`, or a directory beside the test binaries — find what is set for unit tests as well as integration tests), written atomically (write to a temporary name, then rename) so that many processes starting at once either read a complete file or generate and race harmlessly to the rename. A process that finds a file it cannot parse regenerates it. Test-only: all of it is behind the `testing` feature / `cfg(test)`, and nothing a deployment can reach reads a key from that location. State the threat model in the module doc: these keys protect nothing, and the file lives and dies with `target/`.
2. **Every `LazyLock` key in the testing modules reads through it**, so a test process pays a file read and a parse instead of a generation. Tests that are *about* generation or rotation still generate.
3. **`stream_support::Harness` and the other integration harnesses adopt the shared signing key** the way `TestServer` does (`load_or_adopt`), unless a test needs a key of its own.
4. **The 42-second rate-limiter test**: find out why it costs that under coverage and make it cheap without weakening what it proves (it must still fail if a check scans the whole map).
5. **Measure**, instrumented (`RUSTFLAGS=-Cinstrument-coverage`, `LLVM_PROFILE_FILE=<dir>/%m.profraw`), with `cargo nextest run --workspace --profile ci -j 4`, before and after, interleaved, at least two of each; and uninstrumented both ways. nextest 0.9.146 or later is required (`cargo install cargo-nextest --locked --version 0.9.146 --root <a directory in your worktree>`; the copy in `~/.cargo/bin` is too old and must not be replaced). Report per-binary sums for the six binaries above.
6. **Isolation is not negotiable**: a shared *key* is not shared *state*. Databases, data directories, secret stores and content stores stay per test. Prove the suites pass under both runners, ten nextest runs in a row, and from a cold `target/tmp` with all tests starting at once.
7. **`docs/ci.md`**: replace M10-16's projection with the measured first run (the table above and the step times), and record what this brief changed and measured. `CONTRIBUTING.md` if the local commands change.

**Files you own:** `rustak-server/src/testing/**`, `rustak-server/tests/stream_support/mod.rs` and the other `tests/*_support/` harnesses, `rustak-server/src/auth/ratelimit.rs` (its tests only), `rustak-server/src/auth/tokens.rs` only if `load_or_adopt` needs an additive change, `docs/ci.md`, `CONTRIBUTING.md`, `.config/nextest.toml` only if a setting is needed, your status note. Do not edit `.github/`.
