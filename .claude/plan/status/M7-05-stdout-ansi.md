# M7-05 — No ANSI colour on a non-TTY stdout

Brief: `.claude/plan/briefs/M7-05-stdout-ansi.md`. Date: 2026-09-19.

**Outcome.** The fix is in the upstream crate's working tree
(`../tracing-batteries-rs`, uncommitted, as the brief requires) and is proven
end to end. **Nothing changed in rustak**: `with_ansi` does not exist in the
revision rustak's lockfile pins, so `rustak-core/src/telemetry.rs` and
`docs/deployment.md` are untouched — the diffs to apply after the upstream
release are below. `Cargo.toml` and `Cargo.lock` are byte-identical to HEAD
(verified by checksum; they were never edited — see "Deviation 1").

## 1. What the bug actually was, and the proof it is fixed

`build_stdout_layer` returned a bare `tracing_subscriber::fmt::layer()`.
`tracing-subscriber`'s `ansi` feature is on by default and the battery takes
that default, so the layer colourized **unconditionally** — which is the
`docker logs` full of escape codes that Nomad reported. rustak has no other
source of ANSI: `fmt::layer`, `with_ansi`, `NO_COLOR`, `IsTerminal` and raw
`\x1b[` appear nowhere in `rustak-core` or `rustak-server`.

A probe binary (scratchpad, `tracing-batteries` as a path dependency, one
`info!` through the same `OpenTelemetry::new("").with_stdout(true)` battery
rustak uses) run against the tree before and after, `| cat -v`:

```
BEFORE, stdout piped  ^[[2m2026-09-19T16:35:01.397813Z^[[0m ^[[32m INFO^[[0m ...   <- the reported bug
AFTER,  stdout piped  2026-09-19T16:34:44.903458Z  INFO ansi_probe: ansi probe line
AFTER,  on a pty      ^[[2m2026-09-19T16:34:44.913580Z^[[0m ^[[32m INFO^[[0m ...   <- a human still gets colour
AFTER,  pty NO_COLOR=1  2026-09-19T16:34:44.922131Z  INFO ansi_probe: ansi probe line
AFTER,  pty NO_COLOR=   2026-09-19T16:34:44.930707Z  INFO ansi_probe: ansi probe line
```

## 2. Upstream change — to release (working tree only, not committed)

`git diff --stat` in `../tracing-batteries-rs`:

```
 README.md                        |   6 +++
 src/integration_opentelemetry.rs | 101 ++++++++++++++++++++++++++++++++++++++-
 2 files changed, 106 insertions(+), 1 deletion(-)
```

- `OpenTelemetry` gains `ansi: Option<bool>` and a `with_ansi(bool)` builder
  (doc comment with a doctest, beside `with_stdout`).
- The default is factored into a pure function so it is testable without
  touching the process environment, exactly as the brief asked:

  ```rust
  fn build_ansi(&self) -> bool {
      self.ansi.unwrap_or_else(|| {
          Self::should_use_ansi(std::io::stdout().is_terminal(), std::env::var_os("NO_COLOR"))
      })
  }

  fn should_use_ansi(is_terminal: bool, no_color: Option<OsString>) -> bool {
      if no_color.is_some() {
          return false;
      }

      is_terminal
  }
  ```

- `build_stdout_layer` becomes `fmt::layer().with_ansi(self.build_ansi())`.
- Two unit tests: `colour_defaults_to_a_terminal_without_no_color` (all four
  combinations of the two inputs, plus an empty `NO_COLOR`) and
  `with_ansi_overrides_the_environment` (the explicit setting wins both ways).
- Struct-level doc section "Colour in the stdout output" + a README paragraph
  in the OpenTelemetry section.

**One judgement call for the maintainer.** The brief specified `NO_COLOR`
as "set to any value disables colour", so `NO_COLOR=` (empty) disables colour
here and the test asserts it. <https://no-color.org> says "present **and not an
empty string**", which is what `anstyle`/`clap` implement. If the maintainer
prefers the spec-strict reading, it is one line —
`if no_color.is_some_and(|v| !v.is_empty()) { return false; }` — plus flipping
the empty-string assertion in the test.

**Release.** No CHANGELOG exists in that repo, so none was added. **No version
bump**: the crate is not on crates.io, has no tags, and stays `0.1.0`;
consumers (rustak included) track the git `main` branch, as the README says
explicitly. "Releasing" is therefore: review, commit, merge to `main`. The
local checkout sits at `581102c`; rustak's lockfile pins `cbecffa6`.

## 3. rustak follow-up — after the upstream merge

Step 1 — take the new revision (this alone fixes the Nomad logs; the default is
already what rustak wants, so no `telemetry.rs` change is required):

```sh
cargo update -p tracing-batteries    # Cargo.lock only: cbecffa6 -> the merged rev
```

Step 2 — the `docs/deployment.md` sentence, ready to apply (after the
`RUSTAK_SENTRY_DSN` row of the "Logging and telemetry" table, before "What gets
logged at `info`"):

```markdown
| `NO_COLOR` | Set to any value to keep ANSI colour out of the stdout logs. |

Colour is decided by where stdout goes: a terminal gets it, a pipe or a file —
`docker logs`, a systemd journal, a CI log — gets plain text, so captured logs
carry no escape codes. `NO_COLOR` turns it off on a terminal too.
```

Step 3 — *optional*, only if rustak should pin the behaviour rather than
inherit the library default. Ready to apply to `rustak-core/src/telemetry.rs`;
it compiled and passed `cargo test -p rustak-core` against the patched local
checkout (140 unit + 14 doc tests), and fails to compile against the pinned
revision, which is why it is not in the tree:

```diff
-        .with_battery(tracing_batteries::OpenTelemetry::new("").with_stdout(options.stdout));
+        .with_battery(
+            tracing_batteries::OpenTelemetry::new("")
+                .with_stdout(options.stdout)
+                // Colour belongs to a human at a terminal: a container's stdout is a pipe, so
+                // `docker logs` gets plain text rather than escape codes. `NO_COLOR` turns it
+                // off even on a terminal.
+                .with_ansi(
+                    std::io::IsTerminal::is_terminal(&std::io::stdout())
+                        && std::env::var_os("NO_COLOR").is_none(),
+                ),
+        );
```

I recommend **not** applying step 3: it restates the library default verbatim
and would drift from it silently. Steps 1 and 2 are the whole follow-up.

No `RUSTAK_LOG_COLOR` override was added. The brief gated it on "`LOG_LEVEL`
already has a home there", and it does not: `LOG_LEVEL` is read inside
`tracing-batteries`' `build_level`, not in `telemetry.rs`.

## 4. Exit checks

**`../tracing-batteries-rs` — `cargo test`: pass.**

```
running 10 tests ... test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.33s
tests/opentelemetry_grpc.rs        test result: ok. 1 passed; 0 failed
tests/opentelemetry_http_binary.rs test result: ok. 1 passed; 0 failed
tests/opentelemetry_http_json.rs   test result: ok. 1 passed; 0 failed
tests/opentelemetry_propagation.rs test result: ok. 1 passed; 0 failed
Doc-tests: test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
```

Including the two new ones and the new doctest:

```
test integration_opentelemetry::test::colour_defaults_to_a_terminal_without_no_color ... ok
test integration_opentelemetry::test::with_ansi_overrides_the_environment ... ok
test src/integration_opentelemetry.rs - integration_opentelemetry::OpenTelemetry::with_ansi (line 266) ... ok
```

**`../tracing-batteries-rs` — `cargo clippy --all-targets -- -D warnings`: fails
on 6 lints, all pre-existing, none from this change.**

```
error: constants have by default a `'static` lifetime   --> src/integration_opentelemetry.rs:24:36
error: this import is redundant                         --> src/integration_sentry.rs:5:1
error: this `if` statement can be collapsed             --> src/backtraces.rs:93:9
error: this `if` statement can be collapsed             --> src/integration_opentelemetry.rs:322:29
error: this `if` statement can be collapsed             --> src/integration_opentelemetry.rs:375:29
error: this lifetime isn't used in the function definition --> src/lib.rs:90:24
error: could not compile `tracing-batteries` (lib) due to 6 previous errors
```

Proven pre-existing rather than assumed: the same command on a pristine
`git archive HEAD` export produces the identical six lints at the
corresponding lines (22, 5, 93, 288, 341, 90 — the three in this file shift by
my added lines). They are rustc/clippy 1.98.1 drift; that repo's own CI runs
`cargo clippy --all-targets --all-features` **without** `-D warnings`, so they
do not fail it there. Left alone: fixing them would enlarge a diff the
maintainer has to review for an unrelated reason.

`cargo fmt --check --all` upstream: clean (exit 0).

**rustak — `cargo test -p rustak-core`: pass** (against the published
dependency, i.e. the tree as I leave it).

```
test result: ok. 140 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.10s
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
```

Same result against the temporarily patched local checkout, with the step-3
diff applied.

**rustak — `cargo fmt --check`: exits 1, on six files none of which are mine.**

```
Diff in .../rustak-server/src/identity/groups.rs
Diff in .../rustak-server/src/identity/users.rs
Diff in .../rustak-server/src/pki/acme/challenge.rs
Diff in .../rustak-server/src/pki/acme/state.rs
Diff in .../rustak-server/src/web/api/me.rs
Diff in .../rustak-server/src/web/tls.rs
```

All six are `rustak-server/**`, in-flight work of the four agents running in
parallel (M7-01…M7-04). I own no rustak file in this session, so nothing here
is mine to fix, and I did not touch them.

## 5. Deviations from the brief

1. **`[patch.crates-io]` does not apply.** rustak does not depend on
   `tracing-batteries` from crates.io — `Cargo.toml:101` takes it from
   `git = "https://github.com/sierrasoftworks/tracing-batteries-rs.git"`, so
   the patch key has to be that URL. I also avoided editing `Cargo.toml` at
   all (four agents build in this tree concurrently) by passing the patch on
   the command line instead:

   ```sh
   cargo --config 'patch."https://github.com/sierrasoftworks/tracing-batteries-rs.git".tracing-batteries.path="/Users/bpannell/dev/gh/SierraSoftworks/tracing-batteries-rs"' \
     test -p rustak-core
   ```

   `Cargo.lock` was backed up first and came back unmodified regardless; both
   files were verified by checksum against their pre-session copies.
2. **No CHANGELOG entry** — that repo keeps none (no `CHANGELOG*`, no tags, no
   crates.io release); see "Release" above.
3. **No `RUSTAK_LOG_COLOR`** — the brief's condition for it does not hold
   (§3).
4. **Nothing left in rustak's tree**, per the brief's own fallback: the
   `telemetry.rs` call cannot compile against the pinned revision
   (`error[E0599]: no method named with_ansi found for struct OpenTelemetry`,
   confirmed by running `cargo check -p rustak-core` unpatched), and the
   `docs/deployment.md` sentence is gated on that change landing.

## 6. Files

- `../tracing-batteries-rs/src/integration_opentelemetry.rs` — modified, uncommitted.
- `../tracing-batteries-rs/README.md` — modified, uncommitted.
- `.claude/plan/status/M7-05-stdout-ansi.md` — this note.

No `git`/`but` write command was run in either repository.
