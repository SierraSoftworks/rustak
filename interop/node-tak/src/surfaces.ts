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
 * - **M3** data packages (`/Marti/sync/*`, files metadata).
 * - **M4** Data Sync (the mission API).
 */

/** How a surface is looked for. */
export interface SurfaceProbe {
  /** Which of the three CloudTAK base URLs it is served on. */
  readonly on: "webtak" | "stream";

  /** The path to probe, for an HTTP surface. */
  readonly path?: string;

  /** What is missing, and what will flip this skip. */
  readonly todo: string;
}

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
