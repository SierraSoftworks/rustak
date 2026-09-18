# M0-01 — Workspace skeleton: status

## Summary

The full Cargo workspace skeleton described in the brief was created: root manifest and
supporting files, all six crates (`rustak-cot`, `rustak-api`, `rustak-core`, `rustak-client`,
`rustak-server`, `rustak-plugin-example`) plus the excluded `rustak-ui` crate, and
`scripts/check-file-length.sh`. Every version in the design's `[workspace.dependencies]` table
resolves exactly as specified — **no version fallback was needed anywhere**.

**This environment cannot link any native Rust binary** (Xcode Command Line Tools license not
accepted; see "Blocking environment issue" below), which blocks most of the brief's build/test/run
exit checks for reasons entirely outside these files. I verified everything I could without
linking (manifest/dependency resolution, formatting, the file-length script, `cargo check`-style
analysis via clippy where that doesn't need to link, and independent `protoc`-based validation of
the hand-written `.proto` file) and captured exact reproductions for the checks I could not run to
completion. The same root cause independently blocked the concurrent M0-02 agent (`brew install
actionlint` failing for the identical reason — see `.claude/plan/status/M0-02-ci-pipeline.md`), so
this is corroborated, not a one-off.

## Files created

**Root:**
- `Cargo.toml` — workspace manifest, `[workspace]`/`[workspace.package]`/`[workspace.dependencies]`/
  `[workspace.lints]`/dev-profile overrides copied verbatim from design 01 §1.1.
- `.cargo/config.toml`, `.gitignore`, `LICENSE` (MIT, Sierra Softworks), `README.md`,
  `rust-toolchain.toml` (channel = stable), `config.example.toml` (design §3.3 verbatim, with the
  `[stream.tcp]` block deleted per the plan.md deltas).
- `scripts/check-file-length.sh` — byte-for-byte the design §7.2 script, `chmod +x`.

**`rustak-cot/`:** `Cargo.toml`, `build.rs` (protox + prost-build, design §1.3), `proto/tak_protocol_v1.proto`
(clean-room — see below), `src/lib.rs`, `src/proto/mod.rs` (generated-code `include!` + a
`prost::Message` encode/decode round-trip unit test).

**`rustak-api/`, `rustak-core/`, `rustak-client/`:** `Cargo.toml` each, `src/lib.rs` with crate docs
and the `pub mod` list from design §2.1/§2.2 (rustak-client's M0 list is just `sidecar`, per design
01 §1.2's "M0: sidecar module ... stream/marti/control clients in M1/M2/M6"), and one stub file per
module, each containing only a `//! TODO(M0): ...` doc comment pointing at the design section that
will fill it in. No logic anywhere in these three crates.

**`rustak-server/`:** `Cargo.toml` (`[lib] name = "rustak_server"` + `[[bin]] name = "rustak"` +
`[features] testing = []`), `build.rs` (verbatim port of `automate/agent/build.rs`, path changed to
`../rustak-ui/dist`), `src/lib.rs` (crate doc only), `src/main.rs` (prints `rustak <version>` and
exits 0 — see "Deviation: `--version` flag" below), `migrations/.gitkeep`.

**`rustak-plugin-example/`:** `Cargo.toml`, `src/main.rs` (prints its own name and version),
`config.example.toml` (placeholder; the real sidecar config schema is a later brief).

**`rustak-ui/`:** `Cargo.toml` (pinned versions per design §1.2, not `workspace = true` since
excluded), `Trunk.toml`, `index.html`, `src/main.rs` (Yew function component rendering "rustak"),
`styles.scss` (the same 8 section headings as automate's stylesheet, banner text rewritten in my
own words, bodies empty except a trivial `body { margin: 0; }` reset), `.gitignore` (`dist/`).
`rustak-ui/Cargo.lock` was generated (`cargo generate-lockfile`, 124 packages) but **`trunk build`
itself could not complete** — see the environment issue below — so the lockfile reflects dependency
resolution only, not a proven-buildable state. It is left in place for the orchestrator to commit
per the brief ("Run `trunk build` once so `rustak-ui/Cargo.lock` exists and commit it"); I did not
commit anything myself.

Nothing under `.github/`, `Cross.toml`, `*/Dockerfile`, or `e2e/` was touched — those already exist,
created by the concurrent M0-02 agent (confirmed via `git ls-files`), and I left them alone.

## The clean-room `.proto` file

`rustak-cot/proto/tak_protocol_v1.proto` defines `TakMessage{takControl=1,cotEvent=2}`,
`TakControl{minProtoVersion=1,maxProtoVersion=2,contactUid=3}` (exactly the fields design 01 §1.3
names), and `CotEvent` with the full field-number table (`type=1` … `releasableTo=17`, matching
`.claude/plan/research/07-atak-client-verified.md` §4.2's "documented field numbers" table, which
design 01 §1.3 references but doesn't reprint), plus an intentionally empty `message Detail {}` for
`CotEvent.detail=15` to point at. All comments and prose are my own text; only field numbers/names/
types were taken from the research (facts about an existing wire format, not copyrightable
expression, per plan.md's licensing decision). I independently validated the file's syntax and
field table with the system's `protoc` (`protoc --proto_path=proto -o /tmp/... proto/tak_protocol_v1.proto`,
used only as an ad-hoc verification tool on my end, never as part of the crate's actual build,
which uses `protox`/`prost-build` and requires no `protoc`) — it compiles cleanly and every message/
field name round-trips through the descriptor set.

**Note for the M1 protocol brief:** design 02 (`02-protocol-streaming.md`, out of scope for this
brief and not read in detail) puts the real, fuller protocol definition at a different path/package
(`rustak-cot/proto/tak/v1.proto`, package `atakmap.commoncommo.protobuf.v1`, with `TakMessage`
reserving fields 3/4 and `Detail.extensionDetails = 8`). This M0-01 skeleton deliberately follows
design 01 §1.3's path/package (`proto/tak_protocol_v1.proto`, `rustak.cot.v1`) since that's the
section this brief scoped me to; M1's brief will need to reconcile the two (most likely by
replacing this file wholesale, which is fine — M0 only needed to prove the protox pipeline works).

## Deviations / decisions worth flagging

1. **No dependency versions needed a stable-release fallback.** `cargo generate-lockfile` /
   `cargo update --dry-run` at the workspace root resolved all ~520 packages on the first try,
   with every one of the workspace's own pinned versions (`tokio 1.53.1`, `actix-web 4.15.0`,
   `rusqlite 0.40.2`, `prost 0.14.4`, `protox 0.9.1`, `rustls 0.23.45`, `rcgen 0.14.10`, `rsa
   0.9.10`, `zip 8.6.0`, `tracing-batteries` from the pinned git repo, etc.) present in `Cargo.lock`
   exactly as specified in design 01 §0/§1.1. `rustak-ui/Cargo.lock` resolved 124 packages the same
   way. No crate needed a substitute version.
2. **Per-crate dependency lists follow design 01 §1.2 in full**, including for `rustak-api`,
   `rustak-core` and `rustak-client` even though none of their M0 stub modules use any of those
   dependencies yet ("no logic" per the brief). I chose fidelity to the design table over a
   stripped-down "only what's used today" manifest specifically so that
   `cargo update --dry-run`/`cargo generate-lockfile` would actually exercise every version pinned
   in the root manifest (an unused `[workspace.dependencies]` entry that no crate depends on is
   never resolved or fetched by Cargo, so a minimal skeleton would have silently skipped verifying
   most of the table). This is why the check above is meaningful rather than trivially true.
3. **`uuid` in `rustak-api`**: design 01 §1.2 notes "`uuid` here without `v4`/`js`" for this crate.
   Since `[workspace.dependencies] uuid` is defined once with `features = ["v4", "serde"]` and
   Cargo unifies features for a given dependency version across the whole build, `rustak-api`
   declaring `uuid = { workspace = true }` cannot actually *remove* the `v4` feature that
   `rustak-server` (which does need it) turns on elsewhere in the same workspace build. I kept
   `rustak-api` from requesting `v4` itself and left a comment explaining the unification behaviour,
   rather than trying to fight Cargo's feature model — there's no functional difference since the
   crate's stub `identity.rs` doesn't call anything `v4`-gated yet.
4. **`--version` handling in `rustak-server`/`rustak-plugin-example` main.rs**: the brief's exit
   check is "`cargo run -p rustak-server -- --version` prints the version" with no `clap` wiring
   required yet (that's design step 6/12, out of scope here). `main()` unconditionally prints
   `rustak <version>` regardless of arguments, which satisfies the literal check without adding a
   CLI-parsing dependency this brief doesn't need. `#[allow(clippy::print_stdout)]` is applied
   narrowly to that one `println!` (with a comment explaining why), since the workspace lint set
   makes `clippy::print_stdout` a hard error under `-D warnings` and binaries won't have `tracing`
   wired up until a later brief.
5. **Module-file granularity for API/core stubs**: design 01 §2.1 shows `rustak-api::identity` as
   a `mod.rs` + 4 sub-files (`username.rs`, `uid.rs`, `group.rs`, `ids.rs`); since M0-01 owns no
   logic, I stubbed `identity` (and every other §2.1/§2.2 entry) as a single flat file with a
   `//! TODO(M0): ...` doc comment pointing back at the design table, rather than pre-creating the
   eventual sub-file layout. The DTO-implementation brief (M0-03/M0-04, both already exist under
   `.claude/plan/briefs/`) will do that breakdown alongside writing the actual types.
6. **Fixed a `.gitignore` footgun before it could bite**: I initially lifted automate's bare
   `config.toml` ignore line verbatim. Automate gets away with this only because its own
   `.cargo/config.toml` was already tracked before that rule existed (gitignore never hides an
   already-tracked path); in this brand-new repo nothing is tracked yet, so the same bare pattern
   would have made `git add`/`but` silently skip `.cargo/config.toml` forever (confirmed with
   `git check-ignore -v --no-index .cargo/config.toml` before and after). Changed the rule to
   `/config.toml` (anchored to the repo root) so it still ignores a locally-generated runtime
   config without shadowing the tracked `.cargo/config.toml`. Re-verified: `git status
   --porcelain --ignored=matching` now only lists genuine build-artifact directories
   (`target/`, `rustak-ui/target/`, `rustak-ui/dist/`); every file this brief created shows up as
   a normal untracked (`??`) path ready for the orchestrator to add.

## Blocking environment issue (outside these files)

**This machine's Xcode Command Line Tools license has not been accepted**, and I have no
interactive `sudo` available to run `sudo xcodebuild -license` myself (confirmed: `sudo -n true`
fails with "a password is required"). This is not specific to rustak or to anything in this
brief — I reproduced it with a bare `cargo new --bin hello && cargo build` in `/tmp`, which fails
identically:

```
error: linking with `cc` failed: exit status: 69
  = note: You have not agreed to the Xcode license agreements. Please run
    'sudo xcodebuild -license' from within a Terminal window to review and
    agree to the Xcode and Apple SDKs license.
```

What this does and doesn't block, confirmed empirically:
- **Blocked**: anything that needs to *link* a native host artifact. That includes every `[[bin]]`
  target (`rustak-server`, `rustak-plugin-example`, and any `cargo new --bin` at all), every test
  binary (`cargo test` always links a harness, even for a lib-only crate with zero dependencies),
  and — critically — **every build script**, because a build script is itself compiled and linked
  as a native host executable before it runs. Since build scripts are everywhere in the ecosystem
  (`proc-macro2`, `libc`, `thiserror`, `serde`'s `serde_core` build script, `rustix`,
  `logos-codegen`, `aws-lc-rs`, and of course `rustak-cot`'s own `build.rs`), essentially any crate
  graph beyond the standard library hits this, on **any** target — I confirmed the identical
  failure compiling a `serde`-derive-using crate for `wasm32-unknown-unknown`, because
  `proc-macro2`'s build script still has to run on the host regardless of the crate's target.
- **Not blocked**: `cargo metadata` (pure manifest/index resolution), `cargo generate-lockfile` /
  `cargo update --dry-run` (dependency resolution, no compilation), `cargo fmt --check` (no
  compilation), and `cargo check`/`cargo clippy`/`cargo doc` for a lib-only crate with **zero**
  external dependencies (an `.rlib` doesn't need the system linker) — but the moment any real
  dependency with a build script enters the graph, even `clippy`/`doc` fail the same way once they
  have to run that dependency's build script.

I did not attempt any workaround (installing an alternate toolchain, touching Xcode's license
state, etc.) — none of that is available to a non-interactive agent without `sudo`, and it isn't
something to change silently on the user's machine. **The user needs to run `sudo xcodebuild
-license` (accept it) in their own terminal**, after which every check below should be re-run; I
expect them to pass, since the parts of the pipeline I *could* run (full dependency resolution,
formatting, the proto file's independent `protoc` validation) all came back clean.

## Exit checks — results

1. **`cargo metadata --format-version 1 | jq '.workspace_members | length'`** → **`6`**. Pass.
2. **`cargo build` / `cargo build --workspace`** (incl. `env -i PATH=/usr/bin:/bin HOME=$HOME cargo
   build -p rustak-cot`) → **could not complete**, blocked at the first build-script link step
   (`proc-macro2`/`serde_core`/`libc`/etc., depending on which crate) with the exact Xcode-license
   error above. This is true even for `rustak-cot` alone with a filtered PATH containing no
   `protoc` — the failure has nothing to do with `protoc` (confirmed absent from the filtered
   PATH) and everything to do with `cc`/linking. I could not verify the "builds without `protoc`"
   claim by actually finishing a build; I verified the `.proto` file and the build script logic by
   other means (see above) instead.
3. **`cargo test --workspace`** → same blocker, fails compiling test-harness/build-script binaries
   before any test runs.
4. **`cargo fmt --check`** → **passes**, clean, no output, exit 0, across the whole workspace.
5. **`cargo clippy --workspace --all-targets -- -D warnings`** → same linking blocker once it
   reaches any crate with a build-script dependency (starts with `proc-macro2`/`serde_core`/
   `libc`/`parking_lot_core` depending on run). Not evaluable end-to-end here.
6. **`cargo doc --workspace --no-deps -D warnings`** (with `RUSTDOCFLAGS="-D warnings"`) → same
   blocker.
7. **`cd rustak-ui && trunk build`** → same blocker (`rustversion`, `serde_json`, `zmij`, `serde`,
   `thiserror` build scripts all fail to link); `rustak-ui/Cargo.lock` was still generated via
   `cargo generate-lockfile` (124 packages, no errors) before the build itself was attempted.
8. **`cargo clippy --target wasm32-unknown-unknown -- -D warnings`** inside `rustak-ui` → same
   blocker.
9. **`scripts/check-file-length.sh`** → **the script itself exits 0**, but with an important
   caveat: it drives `git ls-files '*.rs'`, and since nothing from this brief is tracked yet (only
   the concurrent agent's earlier commits are), it currently finds **zero** files to check — a
   vacuous pass, not a real one. I ran the identical awk logic by hand over every `.rs` file found
   via `find` instead (all files this brief created, `target/`/`dist/` excluded) and confirmed
   every one is well within the 300-line cap — the longest is `rustak-ui/src/main.rs` at 14
   functional lines. Re-run the script for real once these files are tracked (`but`/`git add`);
   it should still pass, but this hasn't been verified against the script's own file-discovery
   mechanism.
10. **`cargo run -p rustak-server -- --version`** → same linking blocker; could not execute the
    binary. Source review: `main()` unconditionally prints `rustak {CARGO_PKG_VERSION}`
    (`"rustak 0.1.0"` today) and returns, which should satisfy this check once linking works.

## Open items for the orchestrator / next briefs

- **Accept the Xcode CLT license on this machine** (`sudo xcodebuild -license`, interactively, or
  otherwise ensure a working `cc`/linker) before trusting any exit check beyond the ones marked
  "passes" above. Re-run the full exit-check list at that point — I have high confidence they'll
  pass given full dependency resolution and `protoc`-validated proto syntax already succeeded, but
  I have not proven the actual `cargo build -p rustak-cot` (the specific "no `protoc` needed" claim)
  end to end, and that's the one exit check most worth re-verifying first.
- **`rustak-ui/Cargo.lock`** exists (124 packages resolved) but was never produced by a completed
  `trunk build`; treat it as provisional until that command succeeds once the linker is available.
- Design 02 supersedes design 01's `.proto` path/package (see the "clean-room `.proto`" note above)
  — flagged for whoever writes the M1 protocol brief so they know to replace, not extend, this file.
- `scripts/check-file-length.sh`'s `git ls-files`-based discovery means it only checks *tracked*
  files; worth remembering (not a bug — it's the design's own recipe) when running it locally
  before staging changes.
