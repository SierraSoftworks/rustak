/**
 * Asks the server which compatibility surfaces it serves.
 *
 * Each suite is written against a whole contract, but the server is being built
 * one milestone at a time — so every scenario has to know whether the endpoint
 * it exercises exists yet. Probing beats a hard-coded list: a scenario starts
 * running the moment the brief that owns it lands, and nobody has to remember
 * to come back and delete a `skip`.
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
 */

import net from "node:net";

import type { HttpClient } from "./http.js";

/** How a surface is looked for. */
export interface SurfaceProbe {
  /** Which listener it is served on. */
  readonly on: "webtak" | "stream";

  /** The path to probe, for an HTTP surface. */
  readonly path?: string;

  /** What is missing, and what will flip this skip. */
  readonly todo: string;
}

/** Where the probes are sent. */
export interface ProbeTarget {
  /** A client for the browser- and enrollment-facing listener. */
  readonly client: HttpClient;

  /** An administrator's bearer token, so an authorised route is not mistaken for an absent one. */
  readonly token: string;

  /** The CoT stream URL, for the one probe that is a bare TCP connect. */
  readonly stream: string;
}

/** How long a single probe may take before it is called absent. */
const PROBE_TIMEOUT_MS = 5_000;

/** Probes every surface once and reports which ones answered. */
export async function probeSurfaces<S extends Record<string, SurfaceProbe>>(
  surfaces: S,
  target: ProbeTarget,
): Promise<Record<keyof S, boolean>> {
  const found = {} as Record<keyof S, boolean>;

  for (const name of Object.keys(surfaces) as (keyof S)[]) {
    const surface = surfaces[name];

    found[name] =
      surface.on === "stream"
        ? await streamListening(target.stream)
        : await served(target, surface.path ?? "/");
  }

  return found;
}

/** Whether anything answers that path with something other than "no such route". */
async function served(target: ProbeTarget, route: string): Promise<boolean> {
  try {
    // GET everywhere: a POST-only endpoint answers `405`, which is as good a
    // sign of existence as a `200`, and a GET cannot change anything on a route
    // the suite has not yet been taught the shape of.
    const response = await target.client.request(route, {
      method: "GET",
      token: target.token,
      timeoutMs: PROBE_TIMEOUT_MS,
    });

    if (response.status === 404) return false;

    return !(response.status < 400 && response.contentType.startsWith("text/html"));
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
