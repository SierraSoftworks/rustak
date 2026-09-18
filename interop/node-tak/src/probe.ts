/**
 * Asks the server which compatibility surfaces it serves.
 *
 * The suite is written against the whole CloudTAK contract, but the server is
 * being built one milestone at a time — so every scenario has to know whether
 * the endpoint it exercises exists yet. Probing beats a hard-coded list: a
 * scenario starts running the moment the brief that owns it lands, and nobody
 * has to remember to come back and delete a `skip`.
 *
 * Two things mean "not served yet":
 *
 * - **`404`.** An unmatched route inside a mounted scope.
 * - **HTML on a success status.** rustak serves the admin UI from the same
 *   listener and answers anything it does not recognise with the single-page
 *   shell rather than a `404`, because the UI routes on the client and a
 *   reloaded deep link has to reach it (`rustak-server/src/web/ui.rs`). So an
 *   unimplemented Marti route answers `200 text/html`, not `404`.
 *
 * Anything else — `400`, `401`, `403`, `500` — means somebody is listening on
 * that path and the scenario should run and say what it found. A probe is never
 * a pass; it only decides whether to skip.
 *
 * The HTML rule has a corollary worth keeping in mind when the Marti scope
 * lands: it must be mounted **ahead of** the shell's catch-all, or CloudTAK
 * gets an HTML page where it expects JSON and node-tak's `isHTML` sniffing
 * turns it into a `TAKServerError` about a route that is perfectly well
 * implemented.
 */

import net from "node:net";

import type { ServerInfo } from "./rustak.js";
import type { SurfaceName } from "./surfaces.js";
import { SURFACES, SURFACE_NAMES } from "./surfaces.js";

/** How long a single probe may take before it is called absent. */
const PROBE_TIMEOUT_MS = 5_000;

/** Probes every surface once and reports which ones answered. */
export async function probeSurfaces(
  server: ServerInfo,
  token: string,
): Promise<Record<SurfaceName, boolean>> {
  const found = {} as Record<SurfaceName, boolean>;

  for (const name of SURFACE_NAMES) {
    const surface = SURFACES[name];

    found[name] =
      surface.on === "stream"
        ? await streamListening(server.stream)
        : await served(server.webtak, surface.path ?? "/", token);
  }

  return found;
}

/** Whether anything answers that path with something other than "no such route". */
async function served(base: string, route: string, token: string): Promise<boolean> {
  try {
    const response = await fetch(new URL(route, base), {
      // GET everywhere: a POST-only endpoint answers `405`, which is as good a
      // sign of existence as a `200`, and a GET cannot change anything on a
      // route this suite has not yet been taught the shape of.
      method: "GET",
      headers: { Authorization: `Bearer ${token}` },
      signal: AbortSignal.timeout(PROBE_TIMEOUT_MS),
    });

    // Drain, so the connection is not held open behind a keep-alive.
    const type = response.headers.get("content-type") ?? "";
    await response.arrayBuffer();

    if (response.status === 404) return false;

    return !(response.status < 400 && type.startsWith("text/html"));
  } catch {
    // A transport failure is not "absent" in the sense this file means, but it
    // is not something a scenario can run against either, so it skips with the
    // same reason and the scenario's own failure would be noise on top.
    return false;
  }
}

/** Whether something accepts a TCP connection on the CoT stream port. */
function streamListening(url: string): Promise<boolean> {
  const parsed = new URL(url);

  return new Promise((resolve) => {
    const socket = net.connect({ host: parsed.hostname, port: Number(parsed.port) });

    const settle = (answer: boolean) => {
      socket.destroy();
      resolve(answer);
    };

    socket.once("connect", () => settle(true));
    socket.once("error", () => settle(false));
    socket.setTimeout(PROBE_TIMEOUT_MS, () => settle(false));
  });
}
