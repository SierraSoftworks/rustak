/**
 * Talking to CloudTAK itself.
 *
 * Plain HTTP on the loopback: CloudTAK's own API is what the browser talks to,
 * and the compose stack publishes it without TLS exactly as its documented
 * local deployment does. The TLS that matters in this suite is the one between
 * CloudTAK and rustak, inside the network, which is not this client's business.
 *
 * Errors are unwrapped rather than left as status codes, because CloudTAK
 * reports a rustak failure *through* itself: a mission call that rustak refuses
 * comes back as a CloudTAK 400 whose `message` is the sentence a maintainer
 * needs. Losing that in favour of "400" would make every failure in this suite
 * a debugging session.
 */

import type { Call } from "./api.js";

/** A signed-in (or not yet signed-in) CloudTAK caller. */
export class CloudTak {
  readonly base: string;

  #token: string | undefined;

  constructor(base: string, token?: string) {
    this.base = base.replace(/\/$/, "");
    this.#token = token;
  }

  /** The bearer token later calls carry, once a sign-in has produced one. */
  authenticate(token: string): void {
    this.#token = token;
  }

  /** Whether this caller holds a session. */
  get authenticated(): boolean {
    return this.#token !== undefined;
  }

  /** Runs one described call and hands back the parsed answer. */
  async run(call: Call, timeoutMs = 120_000): Promise<unknown> {
    const headers: Record<string, string> = { Accept: "application/json" };

    if (this.#token !== undefined) headers.Authorization = `Bearer ${this.#token}`;

    let body: string | Uint8Array | undefined;

    if (call.raw !== undefined) {
      // Only when the call asks for one. Defaulting to `application/octet-stream`
      // is actively harmful here: CloudTAK's router consumes exactly that type
      // with `bodyparser.raw`, and a handler that streams `req` onward then
      // forwards nothing. See `uploadFile` in `missions.ts`.
      if (call.contentType !== undefined) headers["Content-Type"] = call.contentType;

      body = new Uint8Array(call.raw);
    } else if (call.body !== undefined) {
      headers["Content-Type"] = "application/json";
      body = JSON.stringify(call.body);
    }

    const answer = await fetch(`${this.base}${call.path}`, {
      method: call.method,
      headers,
      body,
      signal: AbortSignal.timeout(timeoutMs),
    });

    const text = await answer.text();
    const parsed = parse(text);

    if (!answer.ok) {
      throw new Error(`${call.method} ${call.path} answered ${answer.status}: ${explain(parsed, text)}`);
    }

    return parsed;
  }

  /** `GET /api`, the version endpoint, which is also the readiness check. */
  async version(): Promise<number> {
    const answer = await fetch(`${this.base}/api`, { signal: AbortSignal.timeout(10_000) });

    await answer.text();

    return answer.status;
  }
}

/** JSON if it is JSON, the raw text otherwise. */
function parse(text: string): unknown {
  if (text.length === 0) return undefined;

  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

/** CloudTAK's own error sentence, or the first of the body if it has none. */
function explain(parsed: unknown, text: string): string {
  if (typeof parsed === "object" && parsed !== null) {
    const fields = parsed as Record<string, unknown>;

    if (typeof fields.message === "string" && fields.message.length > 0) return fields.message;
  }

  return text.slice(0, 400) || "(no body)";
}
