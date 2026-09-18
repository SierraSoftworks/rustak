/**
 * The server surfaces this suite's scenarios need, and what each one waits on.
 *
 * Every scenario names the surfaces it cannot run without. The runner probes
 * each of them once against the server it just started, and a scenario whose
 * surface is not served yet is *skipped with the reason below* rather than
 * deleted — so the job keeps saying what is still missing, and the scenario
 * starts running the moment the milestone that owns it lands. Nothing here
 * needs editing when that happens; the probe notices.
 *
 * This is `interop/node-tak/src/surfaces.ts` for the EUD half of the contract.
 * The probing mechanism itself is shared (`interop/shared/src/probe.ts`),
 * including the rule that an unimplemented Marti route answers `200 text/html`
 * rather than `404`, because the admin UI's shell catches it.
 *
 * The milestones are the briefs in `.claude/plan/briefs/`.
 */

import type { SurfaceProbe } from "../../shared/src/probe.js";

export const SURFACES = {
  enrollment: {
    on: "webtak",
    path: "/Marti/api/tls/config",
    todo: "TODO(M2-03): GET /Marti/api/tls/config is not served yet — `estream:` cannot enrol without it (marti/tls.rs).",
  },
  stream: {
    on: "stream",
    todo: "TODO(M1-05): nothing is listening on the CoT stream port — the mutually authenticated listener lands with M1-05 (stream/listener_tls.rs).",
  },
  clientEndPoints: {
    on: "webtak",
    path: "/Marti/api/clientEndPoints",
    todo: "TODO(M2-06): GET /Marti/api/clientEndPoints is not served yet — it is how the runner sees an EUD from the server side (marti/contacts.rs).",
  },
  channels: {
    on: "webtak",
    path: "/api/v1/groups",
    todo: "TODO(M2-02): /api/v1/groups is not served yet — a scenario cannot put two EUDs in disjoint channels without it.",
  },
  certificateRevocation: {
    on: "webtak",
    path: "/api/v1/certificates",
    todo: "TODO(M2-08): POST /api/v1/certificates/{id}/revoke is not served yet — there is no way to revoke an issued certificate from the API (identity API gaps).",
  },
  // Probed at `missionupload` rather than `missionquery`: a served
  // `missionquery` answers `404` for a hash it does not hold, which is exactly
  // what the probe reads as "no such route". `missionupload` is POST-only, so a
  // GET gets `405` when the scope is mounted and `404` when it is not — an
  // unambiguous answer for the whole enterprise-sync surface.
  missionPackages: {
    on: "webtak",
    path: "/Marti/sync/missionupload",
    todo: "TODO(M3-01): /Marti/sync/* is not served yet — mission-package transfer lands with M3-01 (enterprise sync).",
  },
} as const satisfies Record<string, SurfaceProbe>;

/** The name of a surface this suite knows how to look for. */
export type SurfaceName = keyof typeof SURFACES;

/** Every surface name, in probe order. */
export const SURFACE_NAMES = Object.keys(SURFACES) as SurfaceName[];

/** Whether `name` is one of them, for validating a scenario file. */
export function isSurfaceName(name: string): name is SurfaceName {
  return Object.hasOwn(SURFACES, name);
}

/** What to print beside a scenario that cannot run because `name` is missing. */
export function todoFor(name: SurfaceName): string {
  return SURFACES[name].todo;
}
