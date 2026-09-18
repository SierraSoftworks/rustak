/**
 * Which of this suite's compatibility surfaces the server serves.
 *
 * The probing itself — what counts as "not served yet", and why an
 * unimplemented Marti route answers `200 text/html` rather than `404` — lives
 * in `interop/shared/src/probe.ts`, which `interop/eud` uses for its own
 * surface list. This file is only the binding to *this* suite's surfaces.
 *
 * The HTML rule has a corollary worth keeping in mind: the Marti scope must be
 * mounted **ahead of** the shell's catch-all, or CloudTAK gets an HTML page
 * where it expects JSON and node-tak's `isHTML` sniffing turns it into a
 * `TAKServerError` about a route that is perfectly well implemented.
 */

import { fetchClient } from "../../shared/src/http.js";
import { probeSurfaces as probe } from "../../shared/src/probe.js";

import type { ServerInfo } from "./rustak.js";
import type { SurfaceName } from "./surfaces.js";
import { SURFACES } from "./surfaces.js";

/** Probes every surface once and reports which ones answered. */
export function probeSurfaces(
  server: ServerInfo,
  token: string,
): Promise<Record<SurfaceName, boolean>> {
  return probe(SURFACES, { client: fetchClient(server.webtak), token, stream: server.stream });
}
