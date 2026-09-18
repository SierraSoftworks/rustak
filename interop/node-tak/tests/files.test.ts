/**
 * `GET /files/api/config` and `/Marti/sync/*` — Enterprise Sync, as CloudTAK's
 * file browser and its data-package uploads use it.
 *
 * `/files/api/config` is first and is the highest-priority endpoint in the
 * whole surface: CloudTAK's own `PATCH /api/server` validates a newly entered
 * server by calling **only** that route, and until it succeeds the setup wizard
 * cannot save a working connection at all (`compat/cloudtak.md` §2). So its
 * scenario is deliberately separate from the rest of this file — a server can
 * be reachable to CloudTAK long before it can store anything.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { tokenClient } from "../src/client.js";
import { loadSession, unless } from "../src/session.js";

const session = loadSession();

test(
  "reports an upload limit CloudTAK's setup wizard can save",
  { skip: unless(session, "filesConfig") },
  async () => {
    const config = await tokenClient(session).Files.config();

    assert.equal(
      typeof config.uploadSizeLimit,
      "number",
      "CloudTAK will not save a server connection until this parses as an integer",
    );
    assert.ok(config.uploadSizeLimit > 0);
  },
);

test(
  "lists stored content in the Resource envelope",
  { skip: unless(session, "files") },
  async () => {
    const list = await tokenClient(session).Files.list();

    assert.ok(Array.isArray(list.data), "`/Marti/api/sync/search` answers with `{version, type, data}`");

    for (const content of list.data) {
      assert.equal(typeof content.uid, "string");
      assert.equal(typeof content.filename, "string");
      assert.equal(typeof content.size, "number");
      assert.ok(Array.isArray(content.keywords));
    }
  },
);

test(
  "stores a file and hands the same bytes back",
  { skip: unless(session, "files") },
  async () => {
    const api = tokenClient(session);
    const body = Buffer.from(`node-tak interop ${Date.now()}\n`, "utf8");
    const name = `interop-${Date.now()}.txt`;

    const stored = await api.Files.upload(
      {
        name,
        contentLength: body.length,
        contentType: "text/plain",
        keywords: ["interop"],
        // The shape CloudTAK uses for content its own connections create.
        creatorUid: `connection-interop-data-${name}`,
      },
      body,
    );

    assert.equal(typeof stored.Hash, "string");
    assert.equal(stored.Name, name);

    const chunks: Buffer[] = [];

    for await (const chunk of await api.Files.download(stored.Hash)) {
      chunks.push(Buffer.from(chunk as Buffer));
    }

    assert.deepEqual(Buffer.concat(chunks), body, "the stored bytes came back unchanged");

    await api.Files.delete(stored.Hash);
  },
);
