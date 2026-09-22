/**
 * What a relayed message looks like to the library CloudTAK parses with.
 *
 * The 2026-09-22 outage lived exactly here and nowhere the other scenarios
 * look. rustak stamps a `<_flow-tags_>` marker on every message it relays, and
 * that marker is an XML attribute **named** after `[server] name` — free text
 * an operator types. This installation's was `SierraSoftworks TAK`, so every
 * relayed message went out as
 * `<_flow-tags_ TAK-Server-SierraSoftworks TAK="…">`; CloudTAK's sax parser
 * read it as an attribute with no value and dropped the message off its socket.
 * rustak logged nothing, because rustak had answered fine.
 *
 * This suite's server is deliberately called `Rustak Test & Co. (näme)` (see
 * `src/rustak.ts`), so a relay through it is the production case. Two enrolled
 * connections, one message, and the parser that matters on the receiving end:
 * `@tak-ps/node-cot`, which is the one CloudTAK itself uses.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import TAK, { CoT } from "@tak-ps/node-tak";

import { enroll } from "../src/client.js";
import { SERVER_NAME } from "../src/rustak.js";
import { loadSession, unlessAll } from "../src/session.js";

const session = loadSession();

/** How long to wait for the relay before calling it lost. */
const RELAY_TIMEOUT_MS = 20_000;

/** The uid the sender announces itself under. */
const SENDER_UID = "INTEROP-RELAY-SENDER";

/**
 * The XML 1.0 `Name` production, as a whole-string match.
 *
 * Written out rather than borrowed so that this scenario says what it means by
 * "a name a parser will read" without depending on rustak's own idea of it.
 */
const XML_NAME =
  /^[:_A-Za-zÀ-ÖØ-öø-˿Ͱ-ͽͿ-῿⁰-↏Ⰰ-⿯、-퟿豈-﷏ﷰ-�][-.:_A-Za-z0-9·À-ÖØ-öø-˿̀-ͯͰ-ͽͿ-῿‿-⁀⁰-↏Ⰰ-⿯、-퟿豈-﷏ﷰ-�]*$/u;

/** One `<event>` an EUD would send, as bytes rather than as a builder. */
function situationalAwareness(uid: string): string {
  const now = new Date();
  const stale = new Date(now.getTime() + 120_000);
  const stamp = (at: Date) => at.toISOString().replace(/\.(\d{3})Z$/, ".$1Z");

  return [
    `<event version="2.0" uid="${uid}" type="a-f-G-U-C" how="m-g"`,
    ` time="${stamp(now)}" start="${stamp(now)}" stale="${stamp(stale)}">`,
    `<point lat="51.5" lon="-0.12" hae="0.0" ce="9999999.0" le="9999999.0"/>`,
    `<detail><contact callsign="INTEROP-SENDER" endpoint="*:-1:stcp"/></detail>`,
    `</event>`,
  ].join("");
}

/** Connects one enrolled client the way CloudTAK's connection worker does. */
async function connect(): Promise<TAK> {
  const enrolled = await enroll(session);

  return await TAK.connect(new URL(session.urls.stream), {
    cert: enrolled.cert,
    key: enrolled.key,
    // The stream is one of the two listeners CloudTAK reaches without
    // verifying the chain; only `webtak` verifies.
    rejectUnauthorized: false,
  });
}

/** Waits for the relayed message, or explains what arrived instead. */
function awaitRelay(tak: TAK, uid: string): Promise<CoT> {
  return new Promise<CoT>((resolve, reject) => {
    const seen: string[] = [];
    const timer = setTimeout(() => {
      reject(
        new Error(
          `no relay of ${uid} within ${RELAY_TIMEOUT_MS}ms. Saw: ${seen.join(", ") || "nothing"}`,
        ),
      );
    }, RELAY_TIMEOUT_MS);

    tak.on("cot", (cot: CoT) => {
      const received = String(cot.raw.event._attributes.uid);

      seen.push(received);

      if (received !== uid) return;

      clearTimeout(timer);
      resolve(cot);
    });

    // A parse failure is the failure mode under test: node-cot throwing on
    // what rustak sent is precisely what happened in production, so it has to
    // end this test rather than be swallowed as a socket event.
    tak.on("error", (error: Error) => {
      clearTimeout(timer);
      reject(error);
    });
  });
}

test(
  "a relayed message carries a flow tag node-cot can parse",
  { skip: unlessAll(session, "stream", "tlsConfig", "oauthToken"), timeout: 90_000 },
  async () => {
    const sender = await connect();
    const receiver = await connect();

    try {
      const relayed = awaitRelay(receiver, SENDER_UID);

      // Sent as bytes rather than through node-cot's builder: what is under
      // test is what rustak writes back out, not what this library writes in.
      sender.write_xml(situationalAwareness(SENDER_UID));

      const cot = await relayed;
      const tags = cot.raw.event.detail?.["_flow-tags_"] as
        | { _attributes?: Record<string, string> }
        | undefined;

      assert.ok(tags, `the relay carried no <_flow-tags_>: ${JSON.stringify(cot.raw)}`);

      const names = Object.keys(tags._attributes ?? {});

      assert.equal(names.length, 1, `expected one server's tag, got ${names.join(", ")}`);

      const [name] = names as [string];

      assert.match(
        name,
        /^TAK-Server-/,
        "the flow tag is named after the server that relayed the message",
      );
      assert.match(
        name,
        XML_NAME,
        `'${name}' is not an XML name, so a strict parser drops every message this server relays`,
      );

      // The display name this suite runs under, spelled out: a derivation that
      // passed any of these through is the production bug.
      for (const character of [" ", "&", "(", ")"]) {
        assert.ok(
          SERVER_NAME.includes(character),
          `the suite's server name should carry a '${character}' for this to prove anything`,
        );
        assert.equal(
          name.includes(character),
          false,
          `the flow tag kept a '${character}' from the display name: ${name}`,
        );
      }
    } finally {
      sender.destroy();
      receiver.destroy();
    }
  },
);
