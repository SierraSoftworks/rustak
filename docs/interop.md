# Interop suites

rustak's compatibility with the TAK ecosystem is proven by automated suites
rather than ad-hoc testing. There are four, each owning a different slice of
the compatibility contract in `.claude/plan/plan.md` → Appendix A.

| Suite | Drives | Owns | Runs |
|---|---|---|---|
| `interop/rust` | `rustak-client` fake EUDs, in-process | negotiation, routing by group, disconnect | every PR (part of `cargo test --workspace`; lives in `rustak-server/tests/stream_*.rs`) |
| `interop/node-tak` | `@tak-ps/node-tak` + `@tak-ps/node-cot` — the libraries CloudTAK uses | the HTTP/JSON half: `/oauth/token`, `tls/*`, groups, contacts, missions, files | every PR — **lands in M2** |
| `interop/eud` | ATAK's own `commoncommo`, via its stock `commotest` CLI | the transport/crypto/CoT half: enrollment, TLS, protobuf negotiation, ping/pong, SA and chat routing, mission packages | nightly — **image lands now, scenarios in M2** |
| `interop/cloudtak` | CloudTAK's own docker-compose stack and REST API | full-stack Data Sync round-trip, plus a Playwright UI smoke | nightly — **lands in M4** |

`interop/node-tak` and `interop/eud` are complements, not alternatives:
`commoncommo` does not implement the Marti HTTP surfaces (device profiles,
channels, the mission API), and node-tak does not implement ATAK's TLS and
negotiation behaviour. Neither covers the ATAK UI, which stays a manual
checklist.

## The EUD harness (`interop/eud`)

The one suite that needs a build rather than a package install.
[`interop/eud/README.md`](../interop/eud/README.md) is the reference; the short
version:

- ATAK's networking core, `commoncommo`, is compiled from a **pinned
  `TAK-Product-Center/atak-civ` commit at image-build time**, together with the
  whole `takthirdparty` dependency chain — including the **quictls** fork of
  OpenSSL that ATAK actually links, because the TLS stack is part of what is
  under test.
- The result is `ghcr.io/sierrasoftworks/rustak-interop-commoncommo`, tagged
  `atak-civ-<upstream sha>` plus `latest`.
- `commoncommo` is **GPL-3.0**. Nothing from atak-civ is vendored into this
  repository, and the scenario runner talks to `commotest` across a process
  boundary — never linking it. Keeping the GHCR package private to the
  organisation keeps us out of GPLv3 §6 conveying entirely. See
  `interop/eud/README.md` → Licence posture before touching any of it.
- This harness is the **only** way rustak's `DEFAULT:!ECDH` constraint on
  mission-package transfers gets tested against the real client code. Note the
  constraint applies to the Marti/HTTPS listener (8443), not to the 8089
  stream, which sets no cipher list at all.
- `commotest`'s `estream:` command needs a Basic-auth, no-client-cert
  enrollment listener on **8446**, distinct from the mTLS Marti listener on
  8443.

## Nightly workflow (`.github/workflows/nightly.yml`)

| Job | Trigger | State |
|---|---|---|
| `interop-eud-image` | 03:00 UTC Mondays, and `workflow_dispatch` | **active** — builds and pushes the commoncommo image, caching layers in ghcr |
| `interop-eud` | 04:00 UTC daily | `if: false` placeholder until M2 |
| `interop-cloudtak` | 04:00 UTC daily | `if: false` placeholder until M4 |

The image build is weekly rather than nightly because a cold build is ~25-45
minutes and the result only changes when the upstream pin or the build recipe
changes; scenario runs consume the published tag and never rebuild it. The
pin itself lives in exactly one place — the `ARG ATAK_CIV_SHA` line in
`interop/eud/Dockerfile` — and the workflow derives the image tags from it, so
a bump cannot leave a tag pointing at the wrong upstream revision.

The job needs `packages: write`, which the default `GITHUB_TOKEN` already has
under the repository's existing "Read and write permissions" setting (see
[`ci.md`](ci.md) → Required repository secrets and variables). No new secret is
required.
