# `.claude/plan/` — shared knowledge base for rustak agents

This directory is the single place agents (and the orchestrating session) read from and write to so that
briefs stay short and context is never retyped.

| Path | What it is | Who writes it |
|---|---|---|
| `plan.md` | The approved implementation plan (mirror of the session plan; kept current) | orchestrator |
| `conventions.md` | Code standards every change must satisfy (structure, size, errors, tracing, tests, VCS, licensing) | orchestrator |
| `research/01–07` | Verified research reports. `05`, `06`, `07` were verified against TAK Server 5.7 / ATAK-CIV source and win over `02` wherever they differ. `03` is the CloudTAK/node-tak contract. `04` maps OpenTAKServer (what it gets wrong). `01` maps `../automate` (what to lift). | research agents (read-only afterwards) |
| `design/01–04` | File-level designs: foundations/storage/CI, protocol/streaming, identity/PKI/ACME/auth, Marti API/missions/files/profiles. Apply the deltas listed in `plan.md` → "Design artefacts and reconciled decisions". | design agents (read-only afterwards) |
| `compat/*.md` | Distilled wire contracts per area (streaming, enrollment, missions, files, profiles, groups, oauth, cloudtak). Written from `research/` during implementation; the tests cite them. | implementation agents |
| `briefs/Mx-NN-<slug>.md` | Self-contained task briefs handed to implementation agents | orchestrator |
| `status/Mx-NN-<slug>.md` | What an agent did, verified, could not verify, and left open | implementation / reviewer / interop agents |

Rules: read `plan.md` and `conventions.md` first; cite `research/` and `design/` by section rather than
copying; anything unverified goes in `status/`, never silently assumed; GPL sources (atak-civ, TAK Server,
OpenTAKServer) are facts-only references — never copy code or comment text.
