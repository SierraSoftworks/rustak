/**
 * The one HTTPS call this suite makes for itself.
 *
 * `interop/shared/src/http.ts` covers `/api/v1`: JSON in, JSON out, a bearer
 * token. Enrollment is neither — Basic credentials, a certificate request as a
 * bare base64 string, a JSON answer — so it needs a client that can send
 * arbitrary headers and an arbitrary body while still *verifying* the chain
 * against the test CA. Verification is the point: CloudTAK's own `webtak` calls
 * verify with no override available, so a runner that skipped the check could
 * pass against a server CloudTAK would refuse to talk to.
 */

import fs from "node:fs";
import https from "node:https";

/** What a request answered. */
export interface Answer {
  readonly status: number;
  readonly contentType: string;
  readonly body: string;
}

/** How a request is made. */
export interface Ask {
  readonly method?: string;
  readonly headers?: Readonly<Record<string, string>>;
  readonly body?: string;
  readonly timeoutMs?: number;
}

/** How long a call waits before it is a failure rather than a slow answer. */
const DEFAULT_TIMEOUT_MS = 20_000;

/** Makes one request, verifying the chain against `caFile`. */
export function request(url: string, caFile: string, ask: Ask = {}): Promise<Answer> {
  const target = new URL(url);
  const ca = fs.readFileSync(caFile);

  return new Promise((resolve, reject) => {
    const call = https.request(
      target,
      {
        method: ask.method ?? (ask.body === undefined ? "GET" : "POST"),
        headers: { Accept: "application/json", ...(ask.headers ?? {}) },
        ca,
        servername: target.hostname,
        timeout: ask.timeoutMs ?? DEFAULT_TIMEOUT_MS,
      },
      (answer) => {
        const chunks: Buffer[] = [];

        answer.on("data", (chunk: Buffer) => chunks.push(chunk));
        answer.on("end", () =>
          resolve({
            status: answer.statusCode ?? 0,
            contentType: answer.headers["content-type"] ?? "",
            body: Buffer.concat(chunks).toString("utf8"),
          }),
        );
      },
    );

    call.once("timeout", () => call.destroy(new Error(`${url} timed out`)));
    call.once("error", reject);
    call.end(ask.body);
  });
}

/** Waits until something answers a URL with a status below 500. */
export async function waitFor(
  what: string,
  attempt: () => Promise<number>,
  timeoutMs: number,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let last = "never answered";

  while (Date.now() < deadline) {
    try {
      const status = await attempt();

      if (status < 500) return;

      last = `answered ${status}`;
    } catch (error) {
      last = error instanceof Error ? error.message : String(error);
    }

    await new Promise((resolve) => setTimeout(resolve, 500));
  }

  throw new Error(`${what} was not ready within ${Math.round(timeoutMs / 1000)}s: ${last}`);
}
