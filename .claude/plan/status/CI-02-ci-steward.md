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
