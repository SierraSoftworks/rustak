# M0-17 — Project docs: deployment, contributing, README: status

## Summary

Wrote `docs/deployment.md` (new), `CONTRIBUTING.md` (new), expanded `README.md`, and rewrote the
"Nightly interop" section of `docs/ci.md`. Read `plan.md` (Context, Decisions, Architecture →
Listeners, Milestones, Appendix A summary), `conventions.md`, `config.example.toml`, `docs/ci.md`,
`docs/interop.md`, `docs/plugins.md`, the existing `README.md`, and every file under
`.claude/plan/status/` (`D-01`, `M0-01` through `M0-11`, `M0-13`, `M0-15`, `M1-00`, `M1-04` — the
ones that exist; `M0-12`, `M0-14`, `M1-01`, `M2-01`, `M2-02` have briefs but no status file), plus
`.claude/plan/compat/cloudtak.md` for the CloudTAK three-URL/TLS-trust detail the deployment brief
asked for. Also inspected `rustak-server/src/main.rs`, `rustak-server/src/lib.rs`,
`rustak-server/Dockerfile`, `.github/workflows/nightly.yml`, `scripts/check-file-length.sh`, the
root `Cargo.toml`, `.gitignore` and `e2e/README.md` directly, to describe only what the code
actually does today rather than what the design says it will do.

Only `docs/**`, `CONTRIBUTING.md`, `README.md` and this status file were touched. No `git`/`but`
commands were run.

## The one finding that shaped every document

`rustak-server/src/main.rs` has no run loop yet: `rustak --config config.toml` (without `--check`)
loads and validates the configuration, then prints `"this build has no run loop yet"` and exits —
verified by reading the file, not inferred. `--check` is real and tested
(`.claude/plan/status/M0-06-server-config.md`). The bootstrap brief that wires up
`main.rs`/`lib.rs::run`/`runtime.rs` (`M0-12`) has no status file, so nothing after it — the setup
wizard reachable through a *running* server, the listeners actually binding, the Docker image
actually serving traffic — is verified working end to end, even though the HTTP layer itself is
fully tested in-process (`M0-11`: 211 tests, setup wizard, OIDC, passkeys). I treated this as the
line between "designed and validated" and "runs": every document below states plainly, once, near
the top, that the deployment walkthroughs describe the target shape and are not yet an end-to-end
claim, and does not repeat the caveat at every subsequent mention.

## Files written

| File | Contents |
|---|---|
| `docs/deployment.md` | New. Status callout (the finding above, plus ACME/stream/Marti "planned" call-outs with milestone numbers); `config.example.toml` walked section by section; `--check`; a TLS-modes table (`internal`/`files`/`acme` planned‑M2/`none`); running natively, in Docker (image, `docker run`, a plain `docker-compose.yml`, a CloudTAK compose snippet), and under systemd; first-run setup token + passkey/OIDC sign-in (citing what `M0-11` actually tested); a backup table (`rustak.sqlite` + `.sqlite.key` + `pki/` + `content/` + `streams/`, with why they are not independently restorable); links to the other docs. |
| `CONTRIBUTING.md` | New. Workspace layout table (one line per crate, from `plan.md` Architecture); build order (`trunk build` → `cargo build`, and the 500-response trap if skipped, from `e2e/README.md`); local checks mirroring `docs/ci.md`'s reproduction commands; the <300-functional-line rule with the exact exemptions `scripts/check-file-length.sh` applies (`tests/`, `testing/`, `fixtures/`, `*_tests.rs`, and the note that it only sees `git ls-files`, so new files need a manual check before commit — several `status/` notes hit this same gap independently); error/tracing/config/storage/wire conventions distilled from `conventions.md`; testing conventions including `--features testing` and exactly what it gates (`M0-11`'s manifest change); the GPL-facts-only licensing rule, naming all three reference implementations and the `interop/eud` arm's-length pattern; GitButler branch/commit conventions from `conventions.md`, reframed for a by-hand contributor (the automated agent workflow uses `but`; a human contributor does not need to). |
| `README.md` | Expanded. What rustak is and why (from `plan.md` → Context); a **Status** line; a feature-status table, one row per milestone (M0–M6), each cell's status sourced from the `status/` files listed above rather than from `plan.md`'s exit criteria alone — M0 is "in progress" (lists exactly which M0 briefs are done vs. not), M1 "in progress" (EUD-harness decision recorded, image job delivered-unbuilt, `rustak-cot`/streaming has no status file), M2–M6 "not started" (briefs exist for M2 with no status yet; M6's sidecar SDK already exists from M0 despite M6 itself not having started, called out explicitly so the table isn't misleading); quick start (build order + `--check`); links to `docs/*` and `CONTRIBUTING.md`; the existing "Project plan" section kept and pointed at `.claude/plan/README.md` too. |
| `docs/ci.md` | Only the "Nightly interop" section rewritten, to match the real `.github/workflows/nightly.yml` (read directly): two schedules (nightly 04:00 UTC, weekly Monday 03:00 UTC) and three jobs, one active (`interop-eud-image` — builds and smoke-tests the commoncommo image, runs on the weekly schedule and `workflow_dispatch`, not the nightly one) and two `if: false` placeholders (`interop-eud` → M2, `interop-cloudtak` → M4). This was flagged as understated by `.claude/plan/status/M1-04-interop-eud-image.md` → "Follow-ups for other briefs", which is exactly the finding I fixed. |

## Never claiming an unverified capability

Per the brief's constraint, every mention of ACME, the CoT stream (`:8089`), the Marti API, and
ATAK/CloudTAK enrollment is marked "planned — Mx" with the milestone it lands in, both in
`docs/deployment.md`'s status callout and inline at first mention in each section (the TLS-modes
table, the listener list, the backup table's `streams/` row, the enrollment-token paragraph). I
did **not** mark passkey/OIDC sign-in or the setup wizard as "planned" — `M0-11`'s status file
verifies 211 passing tests covering exactly that flow, including the passkey ceremony end to end
against a real software authenticator — but I did distinguish "tested in-process" from "reachable
through a running `rustak` process," which is the real, narrower gap M0-12's absence leaves.

The Docker section states plainly that the image-publish workflow exists and has been checked
structurally (`M0-02`) but that a green, published run has not been confirmed in this session,
rather than asserting `ghcr.io/sierrasoftworks/rustak:latest` exists.

## Deviations / judgement calls

1. **The CloudTAK compose snippet does not name a CloudTAK image or its Postgres service.** I have
   no verified source for CloudTAK's own container image name or its compose environment variables
   — inventing them would fail exactly the constraint this brief is built around, just for someone
   else's project instead of rustak's. `compat/cloudtak.md` §1/§3 gave verified field semantics
   (`url`/`api`/`webtak`, the `NODE_EXTRA_CA_CERTS` requirement and why), so the snippet shows only
   rustak's side plus the trust relationship, with a comment pointing at CloudTAK's own docs for the
   rest and at `interop/cloudtak/` (M4) as the eventual authoritative compose stack.
2. **README's milestone table calls out M6's sidecar SDK as already existing.** Reading the table
   literally ("Not started") beside `docs/plugins.md`'s own "Status (M0). The harness in this
   document is real and runs today" would have been a contradiction between two docs in the same
   change set, so I added one clause rather than leave that inconsistency for a reader to notice.
3. **`docs/ci.md`'s rewritten section adds a small ASCII diagram** of the two schedules against the
   three jobs. The prose alone kept reading like the workflow has one schedule feeding three jobs
   equally, which is not what `if: github.event.schedule == '0 3 * * 1'` actually gates.
4. **CONTRIBUTING.md's GitButler section is deliberately short and reframed**, not a copy of
   `conventions.md`'s Version control section. That section is written for the orchestrator/agent
   fleet (branch-per-brief, stacking, "agents do not push"); a human contributor reading
   CONTRIBUTING.md needs the branch-naming and commit-message shape, not the agent choreography, so
   I kept the former and pointed at `conventions.md` for the latter rather than duplicating it.

## What I could not verify, and left open

- Whether `ghcr.io/sierrasoftworks/rustak:latest` actually exists yet — no CI run result was
  available to check against; `docs/deployment.md` and `README.md` both say so rather than assuming
  either way.
- The exact wall-clock state of `M1-01` (`rustak-cot`'s CoT model) — this session's `git status`
  shows uncommitted work under `rustak-cot/src/{detail/,error.rs,event.rs,time.rs,types.rs,xml/}`
  and `rustak-cot/fixtures/`, `rustak-cot/tests/`, consistent with `M1-01`'s brief but with no
  status file to confirm completion or scope. README's M1 row says "under active development with
  no status file yet" rather than guessing further.

## Exit checks

Documentation-only brief; no code exit checks apply. Verified instead:

- Every internal link added (`docs/deployment.md`, `CONTRIBUTING.md`, `README.md`,
  `docs/ci.md`) points at a path that exists in this repository — checked each one against
  `find`/`ls` output gathered while researching, not just written on faith.
- `docs/deployment.md`'s config walkthrough was checked key-by-key against the current
  `config.example.toml` rather than against `plan.md`'s narrative description of it, since the
  file is newer and `M0-06`'s status confirms it is tested (`config.example.toml` is loaded in a
  test).
- `docs/ci.md`'s rewritten section was checked line-by-line against `.github/workflows/nightly.yml`
  as it stands today (schedules, `if:` conditions, job names, the image name and tag pattern).

## Open items for the orchestrator

- Once `M0-12` lands, `docs/deployment.md`'s status callout and the "As a native process" /
  "First-run setup and sign-in" sections should be revisited — most of the hedging in this document
  exists specifically because that brief has no status file yet.
- Once a real CI run confirms `ghcr.io/sierrasoftworks/rustak:latest` is published, the Docker
  section's "has been checked structurally... not yet confirmed" sentence should be replaced with a
  plain statement.
- `interop/cloudtak/` (M4) should eventually replace `docs/deployment.md`'s CloudTAK compose snippet
  with a link to the real, tested stack, per the note already left in that section.
