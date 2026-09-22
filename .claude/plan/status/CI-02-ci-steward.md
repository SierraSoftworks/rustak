# CI-02 — CI steward: running log

Brief: `.claude/plan/briefs/CI-02-ci-steward-handover.md`. Takes over from CI-01
(`CI-01-ci-steward.md`, newest-first, last written 2026-09-20; `docs/ci.md` is the
current description of the pipelines). Entries below are appended, oldest first.

## What I inherited (2026-09-22)

- `main` = `65ed9fe` on `511dbab` on `90c89b7`; `90c89b7`'s push run and the
  dispatched nightly 35672048856 were green. M9-12 has since landed as `2cf6abc`
  (`rustak-plugin-adsb` only).
- Rules in force in `rust.yml`: images publish only after lint/test/e2e/node-tak;
  `:latest`/`:main` move forward only (ancestry of the current `:latest` revision
  label, step `Would the floating tags move forward?`); trunk/cross fetched by
  pinned SHA-256; rust-cache saves only from `main`; five uploads on the publish
  path retry once (never yet fired at handover).
- Production (Dublin, Nomad) pins digests of `rustak`, `rustak-plugin-ais`,
  `rustak-plugin-adsb` and waits for all three at revision >= `65ed9fe`; then
  `rustak-plugin-adsb` at >= `2cf6abc`.
- Owed: (1) registry revisions + digests at >= `65ed9fe` (then adsb >= `2cf6abc`);
  (2) forward-only decision lines for `511dbab`, `65ed9fe`, `2cf6abc` and whether
  any upload retry fired; (3) next nightly (04:17 UTC): `cloudtak-parse-log`
  passing reason has a non-zero line count; (4) `Test` timing samples against the
  60-minute bound (band 16-20 min, worst seen 36m56s).
- Maintainer's, not mine: v0.0.3 publish, empty v0.0.2 and stale "Version 0.0.1"
  draft, issues #4/#5, Dependabot #10 and #11.
- Method: registry read over HTTPS (anonymous pull token -> index -> amd64
  manifest -> config blob -> label); `gh` for runs. No `git`/`but` writes.

## 2026-09-22 01:34Z — `511dbab` published; `:latest` is at `511dbab`, not yet `65ed9fe`

Registry (read 01:33Z), all three multi-arch (linux/amd64 + linux/arm64):

| image | `:latest` revision | index digest |
|---|---|---|
| rustak | `511dbaba5481fa36fd0eb27f8521eb14f618f1b8` | `sha256:baac66664c27b8f466d0ca7cf3d86371079228ce4e3a6ff62691df7e06993a4c` |
| rustak-plugin-ais | `511dbaba…` | `sha256:925bbd00d773b917b07e270cbfcd7fb0fc9b646f62452c09ee96a55ad8491a16` |
| rustak-plugin-adsb | `511dbaba…` | `sha256:2e192bbd2739f01310aed8d800ec1b278018dc097ad56b3021e88c10a33f9fd5` |

Run 35673643785 (`511dbab`): success, every job. Forward-only, all four publish
jobs, same shape: `…:latest is at 90c89b7001f4091f7a18d2eab3af3620d3d3cebe, an
ancestor of 511dbaba5481fa36fd0eb27f8521eb14f618f1b8: moving the floating tags.`
Upload retries: 62 retry/wait steps, all skipped; no first attempt failed.
`Test` sample: 25m37s (00:56:41 -> 01:22:18) — above the 16-20 band, inside the bound.

Run 35673702081 (`65ed9fe`): in progress. Everything green except `Test`, whose
`Run tests` step started 00:57:48 and was still running at 01:34Z (36+ min, at the
worst-seen mark; the 60-minute bound falls at about 01:57Z). Docker jobs have not
started (they need `Test`). Retry steps so far: 46 seen, all skipped.

Run 35676226398 (`2cf6abc`): in progress, started 01:33:55Z.

Watcher started (background, 60 s poll): exits when all three `:latest` are at
>= `65ed9fe`, or when the `65ed9fe` run ends in anything but success.

01:37Z addendum. `65ed9fe` `Test` still in `Run tests` at 40 min — past the
worst-seen 36m56s sample. One sample, no per-binary shape readable until the job
ends, so no cause claimed. If it hits the 60-minute bound (~01:57Z) `65ed9fe`
publishes nothing and >= `65ed9fe` can only arrive through `2cf6abc` (its `Test`
started 01:34:33Z; bound ~02:34Z). `2cf6abc` contains `65ed9fe`, so a genuine
hang introduced by `65ed9fe` would show there too.

`Test` samples, last six completed `main` push runs (samples, not a trend):
`0200029` 26m21s, `a59814a` 24m29s, `480272d` 28m35s, `7a8b6a4` 22m59s,
`90c89b7` 23m57s, `511dbab` 25m37s. All six sit above the 16-20 min band the
brief and `docs/ci.md` call normal; none near the 60-minute bound. `65ed9fe` is
the outlier (41+ min and still running at 01:38Z).

## 2026-09-22 01:45Z — `65ed9fe` is RED on `Test`: its own new wall-clock test; nothing published

Run 35673702081 (`65ed9fe`): **failure**. `Test` failed after 41m30s (00:57:24 ->
01:38:54) — a test failure, not the 60-minute bound. `Docker Build` / `Docker
Publish` were skipped, so this run logged **no forward-only decision** and pushed
no `sha-65ed9fe…` image. Upload retries: none ran (all retry/wait steps skipped).
Registry unchanged: all three `:latest` still `511dbab` (digests as above).

The one failure, in `tests/feed_sidecars.rs` (6 passed, 1 failed; every other
binary passed):

    the_first_batch_a_feed_produces_reaches_the_stream_on_a_clean_start
    panicked at rustak-server/tests/feed_sidecars.rs:224:5:
    the first batch was discarded and republished 41.256214152s later

The test is new in `65ed9fe` (M9-11). It starts a clock **before**
`RunningFeed::start` and requires the first vessel inside 4 s. M9-11's notes
measured ~0.8 s locally. Evidence this is the bound, not the product:

- 41 s is not a republish (that would land at ~5 s, the publisher's
  `min_interval`); it is start-up time on a loaded runner.
- The runner was slow across the board: paired per-binary times vs `511dbab`,
  23 binaries >= 5 s, median x1.55 (range x0.89-x6.01), untouched binaries
  included (`workload_identity` 215 -> 436 s, `marti_channels` 85 -> 208 s);
  54 "running for over 60 seconds" warnings vs 4; binaries sum 1253 s -> 2285 s.
- Even on `511dbab`'s ordinary runner the *fastest* of the six feed tests
  finished 13.7 s after the binary started (they run in parallel, each booting a
  server and enrolling). Indicative, not exact — libtest reports no per-test
  time — but a 4 s bound that includes harness start-up looks unreachable in CI
  on a good host too.

One sample. `2cf6abc` (run 35676226398) carries the same test and is the second
sample; its `Test` started 01:34:33Z. If it fails the same way, `:latest` stays at
`511dbab` until the test changes. Not requesting a rerun: not shown to be
infrastructure, and `2cf6abc` supersedes `65ed9fe` anyway. The file is not mine
(`rustak-server/tests/**`); untouched.

Watcher restarted against run 35676226398 (exits at all three >= `65ed9fe`, or
when that run ends in anything but success).

01:46Z — orchestrator confirms the same reading (test defect; wall clock started
before the harness boots) and instructs: do NOT re-run `65ed9fe`; M9-13 is
replacing the assertion with a count of discarded events and auditing for the
same mistake elsewhere. Owed next: `2cf6abc`'s `Test` result and per-binary
timing the moment it finishes. Second watcher started on job 106583298268.
Caveat passed to the orchestrator: the `511dbab` baseline (fastest feed test done
13.7 s after binary start on an ordinary runner) suggests the 4 s bound may fail
on a normal runner as well, not only a slow one — so `2cf6abc` may well be red
too, and `:latest` would then wait for M9-13.

## 2026-09-22 02:00Z — `2cf6abc` is RED on the same test, on a FAST runner; `:latest` stays at `511dbab`

Run 35676226398 (`2cf6abc`): **failure**. `Test` 22m54s (01:34:33 -> 01:57:27),
one failure, the same one:

    the_first_batch_a_feed_produces_reaches_the_stream_on_a_clean_start
    panicked at rustak-server/tests/feed_sidecars.rs:224:5:
    the first batch was discarded and republished 38.217357711s later

Every other binary passed (`feed_sidecars`: 6 passed, 1 failed). Docker Build /
Publish skipped: **no forward-only decision logged**, no image pushed. Upload
retries: 46 retry/wait steps, all skipped. Lint, e2e, node-tak green.

Registry (02:00Z): all three `:latest` still `511dbaba…`, digests unchanged.
`sha-511dbab…` exists for all three; `sha-65ed9fe…` and `sha-2cf6abc…` are 404
for all three. Nothing containing M9-11 or M9-12 has been published.

Per-binary shape, paired against `511dbab` (23 binaries >= 5 s): median **x0.78**
(range x0.35-x2.36); 5 over-60s warnings (4 on `511dbab`, 54 on `65ed9fe`);
binaries sum 1154 s (1253 / 2285). This runner was *faster* than `511dbab`'s.
So "passes on a normal runner, fails on a slow one" did not hold: two runners
about 2x apart in speed measured 41.3 s and 38.2 s. The 4 s bound fails in CI
regardless of host; and the near-constant ~40 s says the number is not simply
host speed. Cause not established from the logs (libtest gives no per-test
time, the test prints nothing but the panic).

Against the median, three binaries got slower on the fast runner:
`feed_sidecars` 37.6 -> 58.0 s (x1.54; one more test), `sidecar_trust`
10.9 -> 25.8 s (x2.36), `enroll_flows` 9.3 -> 15.1 s (x1.62). One sample each;
recorded, not explained. M9-11 added a bounded hold before a sidecar's first
tick (`FIRST_CONNECT` = 10 s, `run.rs`), which is the obvious place to look.

`main` is red and both open runs are finished; nothing in flight. No rerun
requested (deterministic test failure, not infrastructure). Waiting for M9-13.

02:03Z addendum (read-only look, for M9-13; no file of mine involved). The test's
clock starts before `RunningFeed::start` (`tests/feed_support/mod.rs:80`), which
inside the timed window boots a server, enrols **two** certificates (the sidecar
and the watcher device, each a fresh key in a debug build), connects the EUD and
only then spawns the sidecar — with six sibling tests doing the same in parallel.
That is what the ~40 s is made of, so the elapsed figure says nothing either way
about whether the first batch was discarded. Relevant to the replacement
assertion: the first-tick hold is bounded at `FIRST_CONNECT` = 10 s (`link.rs:60`);
if a CI stream connect ever takes longer, a "zero discards" assertion would fail
for a host reason too. Not observed — flagged only.

## 2026-09-22 02:42Z — M9-13 landed (`b485e49`); watching its run; `Test` band text prepared

`b485e49` (run 35679592144) replaces the wall-clock assertion with counts read
from the running sidecar and makes the first-connect hold injectable. At 02:40Z:
Lint, Build UI, e2e, node-tak green; `Test` in progress since 02:28:57Z. Two
background watchers (60 s): one on the `Test` job, one on the registry (exits at
all three `:latest` >= `b485e49`, or the run ending in anything but success).

**`Test` band — samples.** Fifteen `main` push runs from 2026-09-20 23:34Z to
2026-09-22 01:34Z: thirteen in 22m05s-28m35s, one 17m22s (`173b5d8`), one slow
host 41m30s (`65ed9fe`). The eight before that on 2026-09-20 were 16m18s-19m58s
with three slow-host samples (33m08s, 34m59s, 36m56s). So the 16-20 band in
`docs/ci.md` and `rust.yml`'s comments is stale; 22-28 is what the samples say.

**Per-binary shape says it is the suite, not the hosts.** Binaries' own times
sum: `7fe6714` 662 s, `7093274` 869 s (2026-09-20) against `0728135` 1113 s,
`511dbab` 1253 s, `2cf6abc` 1154 s. The difference sits in what landed:
`workload_identity` (new, 22 tests, 168-215 s — the most expensive binary in
the job), `sidecar_enrolment` (2 -> 5 tests, 12 s -> 39-60 s),
`hostile_server_name` (new, 9-26 s), `sidecar_trust` (new, 11-26 s),
`rustak_server` lib (+66 tests). Untouched binaries moved little.

**Consequence the orchestrator must decide.** `docs/ci.md` sizes the bound as
band-top x 2.8: 28 x 2.8 = 78 min, past the 60-minute bound. Worst whole job
actually seen against this band: 41m30s (x1.5-1.9). I have NOT changed
`timeout-minutes`. Options: raise `test` to 90, or make `workload_identity`
cheaper (not my file), or accept the risk knowingly.

**Prepared in the working tree (not landed; to ride along with the next
workflow change):** `docs/ci.md` (+16/-2: new paragraph after the history of the
bound; "16-20" -> "22-28" in the image-delay trade-off) and
`.github/workflows/rust.yml` (+6/-2, **comments only** — the same two places).
`actionlint`: 8 shellcheck notes, the identical set at `HEAD`; none from this
edit. YAML parses, same twelve jobs, `test` still `timeout-minutes: 60`.

Tooling note for whoever follows: `gh run view --job --log` labels every line
`UNKNOWN STEP` for older runs, so filter on cargo's own lines, not the step name.

02:44Z addendum — on "a cheaper `workload_identity`": there is no single hot
spot to remove. Its issuer keys are already one-per-process (`PRIMARY`,
`ROTATED`, `UNADVERTISED` behind `LazyLock` in `src/testing/workload.rs`, forced
by `warm()`); in three runs the first test finished 20-35 s after the binary
started (that warm-up) and the other 21 then completed steadily, about one every
6-8 s on two threads, to 168-216 s. The cost is 22 tests that each boot a server
and enrol, not key generation. So of the three options, "make it cheaper" is
real work for an implementation agent, not a tweak.

## 2026-09-22 02:48Z — decision taken: `test` goes to 90; change prepared, not landed

Orchestrator's decision: raise the bound now, make `workload_identity` cheaper as
backlog (share one server per suite or group cases — an implementation agent's
job). Reasoning on record: a cancelled `Test` now blocks every image, a timeout
is cheap and reversible, and the suite genuinely grew.

Prepared in the working tree, to land AFTER `b485e49` has finished publishing so
it does not put a competing run in front of the deployment's images:

- `.github/workflows/rust.yml`: `test` `timeout-minutes: 60` -> `90` — the only
  non-comment line changed. The comment above it now carries the third move
  (band 22-28, 28 x 2.8 = 78, so 90 with margin; worst whole job seen 41m30s; a
  `Test` that hits 90 is a bug report); the `docker-publish` comment says
  "22-28 minutes normally and up to its 90-minute bound".
- `docs/ci.md`: "90 for `test`" in the timeouts list; the history now says the
  number moved three times and ends at 90 with the sizing rule kept honest and
  the backlog lever named; "hits 90 is a bug report"; the image-delay trade-off
  reads 22-28 minutes and the 90-minute bound.

Checks: `actionlint` — 8 shellcheck notes, the identical set at `HEAD`, nothing
new. YAML parses; same twelve jobs; every other `timeout-minutes` unchanged
(10/10/20/30/30/30/45/10/30/20/20); `test` keeps its seven steps and keys.

Left alone, noted: `docs/ci.md`'s "Keeping the test job inside its timeout"
still opens "Three things keep it inside 30 minutes" — written when 30 was the
bound. Still literally true of a 22-28 band, so not touched in this change.

## 2026-09-22 02:58Z — `b485e49` is GREEN and published; all three `:latest` at `b485e49`

Run 35679592144 (`b485e49`): **success**, every job (`Update Homebrew Tap`
skipped, as on every push). Registry, read over HTTPS at 02:57Z — all three
multi-arch (linux/amd64 + linux/arm64), revision label
`b485e49256a8f06553bf49c8838e42cce26ff790`, and `sha-b485e49…` resolves to the
same index digest as `:latest` for each:

| image | `:latest` index digest | amd64 manifest |
|---|---|---|
| rustak | `sha256:b8e96ffa67223c67a480899aecd0c6392921dd7fe09a0fd6e5b6b1f25e6f1645` | `sha256:f8ff608a77f904d952bc84f4eb16a54be7ebe101c39f21ffc27e3d1be443fba1` |
| rustak-plugin-ais | `sha256:a851ce6daabb1aaba54c2684fb6e32b3cd469935af1df298ed6e6747bad65e9e` | `sha256:ddf7c3c9de053dce3c2811ecd5aaa4233a7b48484c3abe0b467344d411538cad` |
| rustak-plugin-adsb | `sha256:1016411d91b81315f6097a1e6d1aa4831e7ec106466176fa51185a20b4c5fb37` | `sha256:00d3668e8bda0b13e586dc1c817da5052fd8baeea6662c6de2e1544b312a9a22` |

`b485e49` descends from `2cf6abc` and `65ed9fe`, so the deployment's >= `65ed9fe`
gate (all three) and >= `2cf6abc` gate (adsb) are both met by ancestry.

Forward-only, all four publish jobs, same shape: `…:latest is at
511dbaba5481fa36fd0eb27f8521eb14f618f1b8, an ancestor of
b485e49256a8f06553bf49c8838e42cce26ff790: moving the floating tags.`
Upload retries: 62 retry/wait steps, all skipped; no first attempt failed. The
retry has still never fired.

`Test`: success, **25m44s** (02:28:57 -> 02:54:41) — inside the 22-28 band; one
sample. Binaries sum 1241 s, 8 over-60s warnings, 3165 tests, no failures.
`feed_sidecars`: 7 passed, 0 failed, 60.1 s — including
`the_first_batch_a_feed_produces_reaches_the_stream_on_a_clean_start`.
rustak-client `control::events`: 7 of 7 ok (the real-timer ones included). One
green sample does not retire the flake risk on those 150-300 ms margins.

Tooling note: while a run is still in progress `gh run view --job --log` refuses
even a finished job's log; `gh api --allow-escape-sequences
repos/…/actions/jobs/<id>/logs` returns it.

Nothing in flight on `main`. The prepared `test` 60 -> 90 change (rust.yml +
docs/ci.md) is still in the working tree, ready to land now that publishing is
done. Next owed: the 04:17 UTC nightly — `cloudtak-parse-log`'s passing reason
must report a non-zero line count.
