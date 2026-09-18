/**
 * The one HTTP client the suites share, in the two flavours they need.
 *
 * `interop/node-tak` has to reach the server through the global `fetch`,
 * because the libraries it drives do, and it trusts rustak's authority through
 * `NODE_EXTRA_CA_CERTS` — which Node reads once at process start, hence that
 * suite's separate bootstrap process.
 *
 * `interop/eud` has no such constraint: the client under test is `commotest`,
 * not Node, so the runner's own calls can name the authority explicitly with
 * `node:https` and stay in one process. What it must *not* do is turn
 * verification off: the authority the EUD is told to trust and the authority
 * the runner trusts have to be the same one, or a scenario could pass against a
 * server whose certificate nobody checked.
 */

import fs from "node:fs";
import https from "node:https";

/** What a request answered. */
export interface HttpResponse {
  readonly status: number;
  readonly contentType: string;
  readonly body: string;
}

/** How a request is made. */
export interface HttpRequest {
  readonly method?: string;
  readonly body?: unknown;
  readonly token?: string;
  readonly timeoutMs?: number;
}

/** Something that can talk to one server. */
export interface HttpClient {
  /** The base URL every path is resolved against. */
  readonly base: string;

  request(route: string, init?: HttpRequest): Promise<HttpResponse>;
}

/** How long a call waits before it is a failure rather than a slow answer. */
const DEFAULT_TIMEOUT_MS = 15_000;

/** The headers a request carries, given what it is sending. */
function headersFor(init: HttpRequest): Record<string, string> {
  const headers: Record<string, string> = { Accept: "application/json" };

  if (init.body !== undefined) headers["Content-Type"] = "application/json";
  if (init.token) headers.Authorization = `Bearer ${init.token}`;

  return headers;
}

/** A client that goes through the global `fetch`, trusting `NODE_EXTRA_CA_CERTS`. */
export function fetchClient(base: string): HttpClient {
  return {
    base,
    async request(route, init = {}) {
      const response = await fetch(new URL(route, base), {
        method: init.method ?? (init.body === undefined ? "GET" : "POST"),
        headers: headersFor(init),
        body: init.body === undefined ? undefined : JSON.stringify(init.body),
        signal: AbortSignal.timeout(init.timeoutMs ?? DEFAULT_TIMEOUT_MS),
      });

      return {
        status: response.status,
        contentType: response.headers.get("content-type") ?? "",
        body: await response.text(),
      };
    },
  };
}

/** A client that verifies the chain against one authority file, in-process. */
export function httpsClient(base: string, caFile: string): HttpClient {
  const ca = fs.readFileSync(caFile);

  return {
    base,
    request(route, init = {}) {
      const url = new URL(route, base);
      const payload = init.body === undefined ? undefined : JSON.stringify(init.body);

      return new Promise((resolve, reject) => {
        const request = https.request(
          url,
          {
            method: init.method ?? (payload === undefined ? "GET" : "POST"),
            headers: headersFor(init),
            ca,
            // The name in the certificate is the one the suite asked for; the
            // socket is a loopback address either way.
            servername: url.hostname,
            timeout: init.timeoutMs ?? DEFAULT_TIMEOUT_MS,
          },
          (response) => {
            const chunks: Buffer[] = [];

            response.on("data", (chunk: Buffer) => chunks.push(chunk));
            response.on("end", () =>
              resolve({
                status: response.statusCode ?? 0,
                contentType: response.headers["content-type"] ?? "",
                body: Buffer.concat(chunks).toString("utf8"),
              }),
            );
          },
        );

        request.once("timeout", () => request.destroy(new Error(`${url} timed out`)));
        request.once("error", reject);
        request.end(payload);
      });
    },
  };
}

/** One `/api/v1` call, with the failure a reader can act on. */
export async function api<T>(
  client: HttpClient,
  route: string,
  init: HttpRequest = {},
): Promise<T> {
  const response = await client.request(`/api/v1${route}`, init);

  if (response.status < 200 || response.status >= 300) {
    throw new Error(
      `${init.method ?? (init.body === undefined ? "GET" : "POST")} /api/v1${route} answered ${response.status}: ${response.body}`,
    );
  }

  if (response.status === 204 || response.body.length === 0) return undefined as T;

  return JSON.parse(response.body) as T;
}
