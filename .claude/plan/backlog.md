# Backlog — small follow-ups recorded by the orchestrator

Items too small for a brief of their own, or waiting for a milestone. Remove a line when it lands.

M7 (2026-09-19) landed M7-01 plain_bind + ACME handles + wildcard refusal, M7-02 sweep job, M7-03 OIDC channel cache, M7-04 UI tests + demo TLS; M7-05 stdout ANSI is prepared upstream (see below).

- **`/oauth/userinfo` releases the full claim set to every live token** rather than narrowing by the OIDC scopes granted at authorization: the scopes are recorded on the code, which is spent in seconds, and a rustak access token has nowhere flat to carry them. Not a widening (`GET /api/v1/me` answers the same facts to the same token); the ID token *is* narrowed. Fix: record the granted scopes against the session/`jti` and read them at userinfo. Found by M8-01.
- **`interop/cloudtak` still carries the hand-built CSR path it no longer takes.** `src/pki.rs`'s `generateClientRequest`, most of `src/enroll.ts` and `tests/enroll.test.ts` are now the fallback for a server older than M5-03; the suite prefers `POST /api/v1/users/{username}/cloudtak-onboarding` when the surface answers. Worth deleting once no supported release predates it — the parsers are the only offline coverage of `signClient/v2`'s response shape, so they should move rather than go. Found by M5-03.
- **Local toolchain is older than CI's stable** and this machine cannot reach static.rust-lang.org; CI is the authority for new clippy lints until that changes.

- Optional: a dry run for `POST /config-packages`. The only way to find out whether a package can be built for an account is to download one, so the panel has to offer the button before it knows. Left open by M3-06: not worth an endpoint on its own yet.

## Needs a maintainer decision or an upstream release
- **stdout carries ANSI colour when stdout is not a TTY** (reported by the Nomad deployment session; `docker logs` shows escape codes). `tracing-batteries`' OpenTelemetry battery exposes `with_stdout(bool)` but no colour/`NO_COLOR`/`IsTerminal` knob, so this needs an upstream option in `sierrasoftworks/tracing-batteries-rs` first, then a one-line change in `rustak-core/src/telemetry.rs` to pass `std::io::stdout().is_terminal() && env NO_COLOR is unset`.
- **Shared `target/` fills the disk on macOS**: the dev profile's default `split-debuginfo = "unpacked"` keeps every codegen unit's `.o` file under `target/debug/deps` as the debug-info carrier (509k files, >100 GB after a day of parallel agents). Decide with the user: `[profile.dev] split-debuginfo = "packed"` (dsymutil per binary, small dSYMs, slower links) or `debug = "line-tables-only"` plus periodic `find target/debug/deps -name '*.o' -mmin +60 -delete`. Until then the orchestrator prunes `.o` files older than an hour before each landing.
