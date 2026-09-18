/**
 * `ssl://` on the CoT stream port — connecting the way CloudTAK's own
 * connection worker does, with `TAK.connect`.
 *
 * node-tak sends a `t-x-c-t` ping the moment the TLS handshake completes and
 * again every five seconds, and sets `open` (emitting `ping`) when it sees a
 * `t-x-c-t-r` come back. That round trip is the whole liveness contract: a
 * CloudTAK connection that never sees a pong is one that reconnects forever.
 *
 * `t-x-takp-v` is optional here. CloudTAK never negotiates protobuf — it just
 * stores `serverVersion` off the offer if one arrives (`compat/streaming.md`
 * §5) — so its absence is not a failure, but its presence has to parse.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import TAK from "@tak-ps/node-tak";

import { enroll } from "../src/client.js";
import { loadSession, unlessAll } from "../src/session.js";

const session = loadSession();

/** How long to wait for the pong before calling the connection dead. */
const PONG_TIMEOUT_MS = 20_000;

test(
  "answers a ping with t-x-c-t-r over a mutually authenticated connection",
  { skip: unlessAll(session, "stream", "tlsConfig", "oauthToken"), timeout: 60_000 },
  async () => {
    const enrolled = await enroll(session);

    const tak = await TAK.connect(new URL(session.urls.stream), {
      cert: enrolled.cert,
      key: enrolled.key,
      // The stream and the mTLS Marti listener are the two CloudTAK reaches
      // with `rejectUnauthorized: false`; only `webtak` verifies the chain.
      rejectUnauthorized: false,
    });

    try {
      await new Promise<void>((resolve, reject) => {
        const timer = setTimeout(
          () => reject(new Error(`no t-x-c-t-r within ${PONG_TIMEOUT_MS}ms of connecting`)),
          PONG_TIMEOUT_MS,
        );

        tak.once("ping", () => {
          clearTimeout(timer);
          resolve();
        });

        tak.once("error", (error: Error) => {
          clearTimeout(timer);
          reject(error);
        });

        tak.once("end", () => {
          clearTimeout(timer);
          reject(new Error("the server closed the connection before answering a ping"));
        });
      });

      assert.equal(tak.open, true, "node-tak marks a connection open only once it has seen a pong");

      if (tak.version !== undefined) {
        assert.match(
          tak.version,
          /^rustak-/,
          "the t-x-takp-v offer's serverVersion is what CloudTAK displays for the connection",
        );
      }
    } finally {
      tak.destroy();
    }
  },
);
