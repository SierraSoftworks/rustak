# Interop suites

rustak's compatibility with the TAK ecosystem is proven by automated suites
rather than ad-hoc testing. There are four, each owning a different slice of
the compatibility contract in `.claude/plan/plan.md` → Appendix A.

| Suite | Drives | Owns | Runs |
|---|---|---|---|
| `interop/rust` | `rustak-client` fake EUDs, in-process | negotiation, routing by group, disconnect | every PR (part of `cargo test --workspace`; lives in `rustak-server/tests/stream_*.rs`) |
| `interop/node-tak` | `@tak-ps/node-tak` + `@tak-ps/node-cot` — the libraries CloudTAK uses | the HTTP/JSON half: `/oauth/token`, `tls/*`, groups, contacts, missions, files | every PR — **lands in M2** |
| `interop/eud` | ATAK's own `commoncommo`, via its stock `commotest` CLI | the transport/crypto/CoT half: enrollment, TLS, protobuf negotiation, ping/pong, SA and chat routing, mission packages | nightly |
| `interop/cloudtak` | CloudTAK's own docker-compose stack and REST API | full-stack Data Sync round-trip, plus a Playwright UI smoke | nightly — **lands in M4** |

`interop/node-tak` and `interop/eud` are complements, not alternatives:
`commoncommo` does not implement the Marti HTTP surfaces (device profiles,
channels, the mission API), and node-tak does not implement ATAK's TLS and
negotiation behaviour. Neither covers the ATAK UI, which stays a manual
checklist.

`interop/node-tak` runs with one command and no fixtures: `npm test` starts a
throwaway rustak with an internal CA in a scratch directory, walks it through
`/api/v1` to an administrator and a client password (registering a passkey with
a software authenticator on the way, because that is the only route to a bearer
token from a cold start), puts the CA it generated into `NODE_EXTRA_CA_CERTS`
the way a CloudTAK operator must, and then drives the result with node-tak
itself. Because rustak's Marti surface arrives one brief at a time, the runner
**probes** which endpoints exist and skips each scenario it cannot run with a
reason naming the brief that will serve it — `TODO(M2-03): POST /oauth/token is
not served yet` — so the suite reports the gap instead of hiding it, and a
scenario starts running the moment its milestone lands without anybody editing
a skip list. The CI job stays `if: false` until M2-03 makes login and enrollment
real gates; see [`interop/node-tak/README.md`](../interop/node-tak/README.md)
for the wire details it asserts and the one gap it cannot close yet.

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

### The scenarios

`interop/eud/scenarios/*.toml` are declarative — rustak's configuration
overrides, each EUD's `commotest` argv with placeholders the runner fills, and
the expectations on `commo-log.txt`, `commo-xml.txt` and the issued
`commo-enroll-cert.p12` — and `interop/eud/src/*.ts` is the runner that starts
rustak, bootstraps it through `/api/v1`, mints a one-time enrolment token per
EUD, runs each of them as a container and asserts afterwards. The night-one set
is `enroll-basic`, `two-eud-routing`, `chat-direct`, `disconnect`,
`negotiate-refused`, `negotiate-silent`, `enroll-revoked`, `mp-upload` and
`mp-download`; `interop/eud/README.md` → Scenarios has the table and the schema.

Three things worth knowing from outside the directory:

- **The launcher and the bootstrap are shared.** `interop/shared/` holds the
  throwaway-server launcher, the `/api/v1` cold-start walk (setup token → first
  administrator → passkey → wizard) and the surface probe; `interop/node-tak`
  and `interop/eud` both drive them, so the two suites cannot drift about what
  "a rustak with an internal CA in a scratch directory" means.
- **Scenarios skip rather than fail when a surface is missing**, naming the
  brief that will serve it — the same convention `interop/node-tak` uses.
- **`[stream] negotiation = "accept" | "refuse" | "silent"`** is a
  compatibility-testing switch, documented in `config.example.toml`, and exists
  so `negotiate-refused` and `negotiate-silent` can drive ATAK's own two
  fallback paths against a server that is otherwise working. Installations leave
  it alone; to turn protobuf off, `[stream.limits] negotiate_protobuf = false`
  is the supported way.

Without Docker the runner still validates every scenario file and probes the
server, and reports everything as a skip. The nightly job sets
`RUSTAK_EUD_REQUIRE_DOCKER=1` so that a runner without a container runtime fails
there instead.

## Nightly workflow (`.github/workflows/nightly.yml`)

| Job | Trigger | State |
|---|---|---|
| `interop-eud-image` | 03:00 UTC Mondays, and `workflow_dispatch` | **active** — builds and pushes the commoncommo image, caching layers in ghcr |
| `interop-eud` | 04:00 UTC daily, and `workflow_dispatch` | **active** — builds the UI and the server, pulls the pinned image tag (falling back to `:latest`) and runs `npm test` in `interop/eud`, keeping `commo-log.txt`/`commo-xml.txt` and the run log as artefacts on failure |
| `interop-cloudtak` | 04:00 UTC daily | `if: false` placeholder until M4 |

The image build is weekly rather than nightly because a cold build is ~25-45
minutes and the result only changes when the upstream pin or the build recipe
changes; scenario runs consume the published tag and never rebuild it, which is
why `interop-eud` does not `needs:` `interop-eud-image` — a night on which the
image job did not run is a night the scenarios still have an image to pull. The
scenario job needs `packages: read` for the same reason the image job needs
`packages: write`: the package is private to the organisation on purpose. The
pin itself lives in exactly one place — the `ARG ATAK_CIV_SHA` line in
`interop/eud/Dockerfile` — and the workflow derives the image tags from it, so
a bump cannot leave a tag pointing at the wrong upstream revision.

The job needs `packages: write`, which the default `GITHUB_TOKEN` already has
under the repository's existing "Read and write permissions" setting (see
[`ci.md`](ci.md) → Required repository secrets and variables). No new secret is
required.
