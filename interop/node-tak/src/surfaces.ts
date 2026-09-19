/**
 * The compatibility surfaces this suite exercises, and what each one waits on.
 *
 * Every scenario in `tests/` names one surface. The runner probes each of them
 * once against the server it just started, and a scenario whose surface is not
 * served yet is *skipped with the reason below* rather than deleted — so the
 * job graph keeps saying what is still missing, and the scenario starts running
 * the moment the milestone that owns it lands. Nothing here needs editing when
 * that happens; the probe notices.
 *
 * The milestones are the briefs in `.claude/plan/briefs/`:
 *
 * - **M1-05** the mutually authenticated CoT stream listener.
 * - **M2-03** enrollment (`/Marti/api/tls/*`) and `/oauth/token`.
 * - **M2-04** the Marti envelope, `/Marti/api/version`, `/files/api/config`.
 * - **M2-06** channels, contacts and `clientEndPoints`.
 * - **M3-02** the device profiles ATAK fetches on enrolment and on connect.
 * - **M3** data packages (`/Marti/sync/*`, files metadata).
 * - **M4** Data Sync (the mission API).
 */

import type { SurfaceProbe } from "../../shared/src/probe.js";

/** How a surface is looked for; the probing itself is shared. */
export type { SurfaceProbe };

export const SURFACES = {
  martiVersion: {
    on: "webtak",
    path: "/Marti/api/version",
    todo: "TODO(M2-04): GET /Marti/api/version is not served yet — the Marti scope lands with M2-04 (marti/version.rs).",
  },
  filesConfig: {
    on: "webtak",
    path: "/files/api/config",
    todo: "TODO(M2-04): GET /files/api/config is not served yet — CloudTAK's connectivity smoke test lands with M2-04.",
  },
  oauthToken: {
    on: "webtak",
    path: "/oauth/token",
    todo: "TODO(M2-03): POST /oauth/token is not served yet — the password grant lands with M2-03 (marti/oauth.rs).",
  },
  tlsConfig: {
    on: "webtak",
    path: "/Marti/api/tls/config",
    todo: "TODO(M2-03): GET /Marti/api/tls/config is not served yet — enrollment lands with M2-03 (marti/tls.rs).",
  },
  enrollmentProfile: {
    on: "webtak",
    path: "/Marti/api/tls/profile/enrollment",
    todo: "TODO(M3-02): GET /Marti/api/tls/profile/enrollment is not served yet — the device profile ATAK fetches with the credential it enrolled with lands with M3-02 (marti/profiles.rs).",
  },
  groups: {
    on: "webtak",
    path: "/Marti/api/groups/all",
    todo: "TODO(M2-06): GET /Marti/api/groups/all is not served yet — channels land with M2-06 (marti/groups.rs).",
  },
  contacts: {
    on: "webtak",
    path: "/Marti/api/contacts/all",
    todo: "TODO(M2-06): GET /Marti/api/contacts/all is not served yet — contacts land with M2-06 (marti/contacts.rs).",
  },
  clientEndPoints: {
    on: "webtak",
    path: "/Marti/api/clientEndPoints",
    todo: "TODO(M2-06): GET /Marti/api/clientEndPoints is not served yet — it lands with M2-06 (marti/subscriptions.rs).",
  },
  missions: {
    on: "webtak",
    path: "/Marti/api/missions",
    todo: "TODO(M4): the mission API is not served yet — Data Sync lands in M4 (plan.md milestone table).",
  },
  cotQuery: {
    on: "webtak",
    path: "/Marti/api/cot/xml/probe",
    todo: "TODO(M4-04): GET /Marti/api/cot/xml/{uid} is not served yet — node-tak's query.single()/history() read it, and the oversize stream substitution points ATAK at it (marti/cot.rs).",
  },
  files: {
    on: "webtak",
    path: "/Marti/api/sync/search",
    todo: "TODO(M3): /Marti/sync/* and the files metadata API are not served yet — data packages land in M3.",
  },
  stream: {
    on: "stream",
    todo: "TODO(M1-05): nothing is listening on the CoT stream port — the mutually authenticated listener lands with M1-05 (stream/listener_tls.rs).",
  },
} as const satisfies Record<string, SurfaceProbe>;

/** The name of a surface this suite knows how to look for. */
export type SurfaceName = keyof typeof SURFACES;

/** Every surface name, in probe order. */
export const SURFACE_NAMES = Object.keys(SURFACES) as SurfaceName[];
