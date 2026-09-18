/**
 * CloudTAK's REST API, as request builders and response parsers.
 *
 * Everything here is pure — a call is described, an answer is inspected — so
 * `tests/api.test.ts` can assert the whole contract against fixtures on a
 * machine with no Docker. The runner in `src/steps.ts` supplies the transport.
 *
 * The parsers are deliberately opinionated about *why* something is wrong,
 * because the failure that matters most here has a confusing symptom: when
 * rustak answers a Marti call with `application/json; charset=UTF-8`, node-tak
 * compares the header with `===` against `application/json`, misses, and hands
 * CloudTAK the raw text — which CloudTAK then serialises back out as a JSON
 * *string*. The route looks like it worked. `expectObject` names that.
 * `compat/cloudtak.md` §4 is the rule; this is what it looks like from outside.
 */

/** One call to make: a method, a path (query string included) and a payload. */
export interface Call {
  readonly method: string;
  readonly path: string;

  /** A JSON body, if any. */
  readonly body?: unknown;

  /** A raw body, for the one endpoint that takes bytes rather than JSON. */
  readonly raw?: Buffer;

  /** The `Content-Type` a raw body is sent with. */
  readonly contentType?: string;
}

/** The TAK envelope CloudTAK passes straight through from the TAK Server. */
export interface TakList<T> {
  readonly version: string;
  readonly type: string;
  readonly data: readonly T[];
}

/** One channel, in the shape `GET/PUT /api/marti/group` uses. */
export interface Group {
  readonly name: string;
  readonly direction: string;
  readonly created: string;
  readonly type: string;
  readonly bitpos: number;
  readonly active: boolean;
  readonly description?: string;
}

/** What CloudTAK knows about the TAK server it is pointed at. */
export interface ServerState {
  readonly status: string;
  readonly version: string;
  readonly url: string;
  readonly api: string;
  readonly webtak: string;
  readonly auth: boolean;
  readonly connection_status?: string;
  readonly certificate?: { readonly subject: string; readonly validFrom: string; readonly validTo: string };
}

/** A signed-in CloudTAK session. */
export interface Login {
  readonly token: string;
  readonly access: string;
  readonly email: string;
  readonly session: string;
}

/** A Data Sync, as CloudTAK returns it from the mission helpers. */
export interface Mission {
  readonly name: string;
  readonly guid: string;
  readonly keywords: readonly string[];
  readonly createTime: string;
  readonly token?: string;
  readonly contents: readonly { readonly data: { readonly hash: string; readonly name?: string } }[];
}

/** One entry in a mission's change log. */
export interface MissionChange {
  readonly type: string;
  readonly missionName: string;
  readonly timestamp: string;
  readonly contentUid?: string;
  readonly creatorUid?: string;
  readonly details?: { readonly type: string; readonly callsign?: string };
  readonly contentResource?: { readonly hash: string; readonly name?: string };
}

/** What the TAK Server says about a file it stored. */
export interface Content {
  readonly UID: string;
  readonly Hash: string;
  readonly Name: string;
  readonly SubmissionUser?: string;
  readonly CreatorUid?: string;
}

/** Asserts that an answer is a JSON object, and explains it when it is not. */
export function expectObject(what: string, body: unknown): Record<string, unknown> {
  if (typeof body === "string") {
    throw new Error(
      `${what} came back as a JSON string rather than an object. That is what CloudTAK emits when node-tak could not parse rustak's answer: node-tak tests the header with \`content-type === 'application/json'\`, so any parameter — a '; charset=UTF-8' suffix — makes it hand the raw text back untouched (compat/cloudtak.md §4). The text was: ${body.slice(0, 200)}`,
    );
  }

  if (typeof body !== "object" || body === null || Array.isArray(body)) {
    throw new Error(`${what} came back as ${Array.isArray(body) ? "an array" : typeof body}, not an object.`);
  }

  return body as Record<string, unknown>;
}

/** Asserts the TAK envelope and hands back its `data` array. */
export function expectTakList<T>(what: string, body: unknown): T[] {
  const object = expectObject(what, body);

  if (!Array.isArray(object.data)) {
    throw new Error(
      `${what} answered without a 'data' array — the TAK envelope is {version, type, data} and CloudTAK passes it through verbatim. Keys: ${Object.keys(object).join(", ") || "(none)"}`,
    );
  }

  return object.data as T[];
}

/** Reads a string field, or says which one was missing. */
function text(what: string, object: Record<string, unknown>, field: string): string {
  const value = object[field];

  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`${what} answered without a '${field}' string. Keys: ${Object.keys(object).join(", ")}`);
  }

  return value;
}

/**
 * The first call a CloudTAK operator makes: point it at a TAK server.
 *
 * On an *unconfigured* CloudTAK this needs no authentication, must carry a
 * username and password (the first successful pair becomes CloudTAK's system
 * administrator, enrolled a certificate of its own on the spot through
 * `webtak`), and validates the `auth` pair it is given by calling
 * `GET /files/api/config` through it — nothing else. Until that answers with an
 * integer `uploadSizeLimit`, CloudTAK cannot be configured at all
 * (`compat/cloudtak.md` §2).
 */
export function configureServer(options: {
  readonly name: string;
  readonly url: string;
  readonly api: string;
  readonly webtak: string;
  readonly username: string;
  readonly password: string;
  readonly cert: string;
  readonly key: string;
}): Call {
  return {
    method: "PATCH",
    path: "/api/server",
    body: {
      name: options.name,
      url: options.url,
      api: options.api,
      webtak: options.webtak,
      username: options.username,
      password: options.password,
      auth: { cert: options.cert, key: options.key },
    },
  };
}

/** Reads back what CloudTAK stored, and whether it considers itself configured. */
export function parseServer(body: unknown): ServerState {
  const object = expectObject("PATCH /api/server", body);
  const status = text("PATCH /api/server", object, "status");

  if (status !== "configured") {
    throw new Error(`CloudTAK still reports its server as '${status}' after a successful PATCH.`);
  }

  const certificate = object.certificate;

  return {
    status,
    version: text("PATCH /api/server", object, "version"),
    url: text("PATCH /api/server", object, "url"),
    api: text("PATCH /api/server", object, "api"),
    webtak: text("PATCH /api/server", object, "webtak"),
    auth: object.auth === true,
    connection_status: typeof object.connection_status === "string" ? object.connection_status : undefined,
    certificate:
      typeof certificate === "object" && certificate !== null
        ? (certificate as ServerState["certificate"])
        : undefined,
  };
}

/** The username/password sign-in, which is CloudTAK's `/oauth/token` password grant. */
export function login(username: string, password: string): Call {
  return { method: "POST", path: "/api/login", body: { username, password } };
}

/** The session a sign-in produced. */
export function parseLogin(body: unknown): Login {
  const object = expectObject("POST /api/login", body);

  return {
    token: text("POST /api/login", object, "token"),
    access: text("POST /api/login", object, "access"),
    email: text("POST /api/login", object, "email"),
    session: text("POST /api/login", object, "session"),
  };
}

/** Channels, as the signed-in user sees them. */
export function listGroups(): Call {
  return { method: "GET", path: "/api/marti/group" };
}

/** Channels, with one of them toggled — the write half of the same surface. */
export function updateGroups(groups: readonly Group[]): Call {
  return {
    method: "PUT",
    path: "/api/marti/group",
    body: groups.map((group) => ({
      name: group.name,
      direction: group.direction,
      created: group.created,
      type: group.type,
      bitpos: group.bitpos,
      active: group.active,
      ...(group.description === undefined ? {} : { description: group.description }),
    })),
  };
}

/** The channel list out of either call. */
export function parseGroups(what: string, body: unknown): Group[] {
  const groups = expectTakList<Group>(what, body);

  for (const group of groups) {
    if (typeof group.name !== "string" || typeof group.active !== "boolean") {
      throw new Error(`${what} returned a channel without a name and an active flag: ${JSON.stringify(group)}`);
    }
  }

  return groups;
}

/** Flips one channel's `active` flag, leaving every other one alone. */
export function toggled(groups: readonly Group[], name: string): Group[] {
  return groups.map((group) => (group.name === name ? { ...group, active: !group.active } : group));
}
