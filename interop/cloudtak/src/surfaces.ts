/**
 * The rustak surfaces this suite needs, and what each missing one waits on.
 *
 * Same convention as `interop/node-tak` and `interop/eud`: the runner probes
 * each one against the server the stack just started, and a scenario whose
 * surface is not served yet skips with the reason below rather than failing
 * with a CloudTAK error three layers away from the cause. A scenario starts
 * running the moment the brief that owns it lands, without anybody editing a
 * list.
 *
 * The probing itself is `interop/shared/src/probe.ts`; only the map is ours.
 */

import type { SurfaceProbe } from "../../shared/src/probe.js";

export const SURFACES = {
  filesConfig: {
    on: "webtak",
    path: "/files/api/config",
    todo: "TODO(M2-04): GET /files/api/config is not served yet — CloudTAK's setup wizard validates a server with this call and nothing else, so no part of this suite can run without it.",
  },
  oauthToken: {
    on: "webtak",
    path: "/oauth/token",
    todo: "TODO(M2-03): POST /oauth/token is not served yet — CloudTAK's login is the password grant.",
  },
  tlsConfig: {
    on: "webtak",
    path: "/Marti/api/tls/config",
    todo: "TODO(M2-03): GET /Marti/api/tls/config is not served yet — CloudTAK enrols a certificate for every user it signs in.",
  },
  groups: {
    on: "webtak",
    path: "/Marti/api/groups/all",
    todo: "TODO(M2-06): GET /Marti/api/groups/all is not served yet — channels land with M2-06 (marti/groups.rs).",
  },
  missions: {
    on: "webtak",
    path: "/Marti/api/missions",
    todo: "TODO(M4): the mission API is not served yet — Data Sync lands in M4 (M4-01, M4-02).",
  },
  cloudtakOnboarding: {
    on: "webtak",
    // The *download*, not the POST that creates a hand-over: rustak answers a
    // path it does not recognise with the admin UI's single-page shell, so a
    // GET on a POST-only route is `200 text/html` rather than `405` and the
    // probe would read it as absent. An unknown download id is a clean `410`.
    path: "/api/v1/cloudtak-onboarding/probe.p12",
    todo: "TODO(M5-03): POST /api/v1/users/{username}/cloudtak-onboarding is not served yet — the one-action hand-over lands with M5-03; until then this suite builds CloudTAK's admin identity with a CSR by hand.",
  },
  files: {
    on: "webtak",
    path: "/Marti/api/sync/search",
    todo: "TODO(M3): /Marti/sync/* and the files metadata API are not served yet — data packages land in M3 (M3-01).",
  },
} as const satisfies Record<string, SurfaceProbe>;

/** The name of a surface this suite knows how to look for. */
export type SurfaceName = keyof typeof SURFACES;

/** Every surface name, in probe order. */
export const SURFACE_NAMES = Object.keys(SURFACES) as SurfaceName[];

/** The reason a scenario gives when it skips for a missing surface. */
export function todoFor(name: SurfaceName): string {
  return SURFACES[name].todo;
}
