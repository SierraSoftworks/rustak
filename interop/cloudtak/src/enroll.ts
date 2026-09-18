/**
 * Minting the client certificate CloudTAK stores as its admin connection.
 *
 * `PATCH /api/server` takes `auth: { cert, key }` in PEM and validates it by
 * calling `GET /files/api/config` through it — so the certificate has to exist
 * before CloudTAK can be configured at all, and CloudTAK has no endpoint that
 * mints one. This is therefore the one place the suite talks to rustak's TAK
 * surface directly instead of through CloudTAK, and it performs exactly the
 * flow `node-tak`'s `Credentials.generate()` performs, which is what CloudTAK
 * itself runs for a user's own certificate a moment later:
 *
 *     GET  /Marti/api/tls/config          the name entries the CSR is built from
 *     POST /Marti/api/tls/signClient/v2   the CSR as bare base64, Basic auth
 *
 * Every rule below is one `compat/enrollment.md` records, and each is a
 * separate exported function so the unit tests can assert it against a fixture
 * without a server: the CN that must match the account, the bare-base64 body,
 * and the bare-base64 answer that a client re-armours itself.
 */

import { generateClientRequest } from "./pki.js";
import { request } from "./http.js";

/** The subject fields `GET /Marti/api/tls/config` asks a client to use. */
export interface NameEntries {
  readonly organisation: string;
  readonly unit: string;
}

/** What the enrollment produced: PEM throughout, as CloudTAK stores it. */
export interface Enrolled {
  readonly cert: string;
  readonly key: string;
  readonly ca: readonly string[];
}

/** The fallback subject, for a server whose config document omits one. */
const FALLBACK: NameEntries = { organisation: "rustak", unit: "interop" };

/**
 * Reads the `O` and `OU` a client should put in its CSR.
 *
 * Deliberately a regular expression rather than an XML parser: the document is
 * `ns2:certificateConfig` with a handful of `nameEntry` attributes, the suite
 * has no XML dependency, and `interop/node-tak` already asserts the document's
 * shape properly with the parser CloudTAK itself uses.
 */
export function parseNameEntries(xml: string): NameEntries {
  const entries = new Map<string, string>();

  for (const match of xml.matchAll(/<nameEntry\s[^>]*?>/g)) {
    const name = /name\s*=\s*"([^"]*)"/.exec(match[0]);
    const value = /value\s*=\s*"([^"]*)"/.exec(match[0]);

    if (name && value && value[1].length > 0) entries.set(name[1], value[1]);
  }

  return {
    organisation: entries.get("O") ?? FALLBACK.organisation,
    unit: entries.get("OU") ?? FALLBACK.unit,
  };
}

/** The path a certificate request is posted to, query string and all. */
export function signClientPath(clientUid: string, version = "rustak-interop"): string {
  const query = new URLSearchParams({ clientUid, version });

  return `/Marti/api/tls/signClient/v2?${query.toString()}`;
}

/** `Authorization: Basic …`, which is the only thing this endpoint accepts. */
export function basicHeader(username: string, password: string): string {
  return `Basic ${Buffer.from(`${username}:${password}`, "utf8").toString("base64")}`;
}

/**
 * The request body: the DER of the CSR as bare base64, one line.
 *
 * CloudTAK posts the base64 with no `Content-Type` at all and no PEM banner;
 * rustak strips a banner if it sees one, but sending what CloudTAK sends is the
 * whole point of a compatibility suite.
 */
export function csrBody(pem: string): string {
  return pem
    .split("\n")
    .filter((line) => !line.startsWith("-----") && line.trim().length > 0)
    .join("");
}

/** Re-armours bare base64 DER as PEM, exactly as node-tak does on the way back. */
export function armour(base64: string, label = "CERTIFICATE"): string {
  const body = base64.replace(/\s+/g, "").match(/.{1,64}/g) ?? [];

  return `-----BEGIN ${label}-----\n${body.join("\n")}\n-----END ${label}-----\n`;
}

/**
 * Turns a `signClient/v2` answer into the PEM pair CloudTAK stores.
 *
 * The two failures this names are the ones that break CloudTAK rather than
 * merely disappointing it: a missing `signedCert`, and a `signedCert` that
 * arrives already armoured — node-tak wraps the banner around whatever it is
 * given, so PEM in the response becomes a PEM banner around a PEM banner and
 * every later use of the certificate fails somewhere unrelated
 * (`compat/enrollment.md` §3).
 */
export function parseSignedCertificate(body: string): { cert: string; ca: string[] } {
  let parsed: unknown;

  try {
    parsed = JSON.parse(body);
  } catch {
    throw new Error(
      `POST /Marti/api/tls/signClient/v2 did not answer JSON. It answered: ${body.slice(0, 200)}`,
    );
  }

  if (typeof parsed !== "object" || parsed === null) {
    throw new Error("signClient/v2 answered JSON that is not an object.");
  }

  const fields = parsed as Record<string, unknown>;
  const signed = fields.signedCert;

  if (typeof signed !== "string" || signed.length === 0) {
    throw new Error(
      `signClient/v2 answered without a 'signedCert' field: ${Object.keys(fields).join(", ") || "(no fields)"}`,
    );
  }

  if (signed.includes("BEGIN CERTIFICATE")) {
    throw new Error(
      "signClient/v2 answered a PEM-armoured 'signedCert'. It must be bare base64: node-tak adds the banner itself, so armour here is doubled and nothing can read the result (compat/enrollment.md §3).",
    );
  }

  const ca: string[] = [];

  for (let index = 0; ; index += 1) {
    const value = fields[`ca${index}`];

    if (typeof value !== "string" || value.length === 0) break;

    if (value.includes("BEGIN CERTIFICATE")) {
      throw new Error(`signClient/v2 answered a PEM-armoured 'ca${index}'; each caN must be bare base64.`);
    }

    ca.push(armour(value));
  }

  if (ca.length === 0) {
    throw new Error("signClient/v2 answered no 'ca0', so a client has no chain to trust.");
  }

  return { cert: armour(signed), ca };
}

/** Everything the enrollment needs to reach the server and prove who it is. */
export interface EnrollmentTarget {
  /** The `webtak` base URL, as the runner reaches it. */
  readonly webtak: string;

  /** The authority the runner verifies that listener against. */
  readonly caFile: string;

  /** The account, whose name the CSR's CN must match. */
  readonly username: string;

  /** Its client password — the one credential this endpoint accepts. */
  readonly password: string;

  /** The device identity the certificate is issued to. */
  readonly clientUid: string;
}

/** Performs the whole flow and returns the PEM pair. */
export async function enrol(target: EnrollmentTarget): Promise<Enrolled> {
  const authorization = basicHeader(target.username, target.password);

  const config = await request(`${target.webtak}/Marti/api/tls/config`, target.caFile, {
    headers: { Authorization: authorization },
  });

  if (config.status !== 200) {
    throw new Error(`GET /Marti/api/tls/config answered ${config.status}: ${config.body.slice(0, 200)}`);
  }

  const entries = parseNameEntries(config.body);
  const generated = generateClientRequest(target.username, entries.organisation, entries.unit);

  const signed = await request(
    `${target.webtak}${signClientPath(target.clientUid)}`,
    target.caFile,
    {
      method: "POST",
      headers: { Authorization: authorization, "Content-Type": "application/octet-stream" },
      body: csrBody(generated.csr),
      timeoutMs: 60_000,
    },
  );

  // 201 is a failure signal to ATAK and rustak answers 200; anything else is a
  // refusal whose body is the thing worth reading.
  if (signed.status !== 200) {
    throw new Error(
      `POST /Marti/api/tls/signClient/v2 answered ${signed.status}: ${signed.body.slice(0, 300)}`,
    );
  }

  const { cert, ca } = parseSignedCertificate(signed.body);

  return { cert, key: generated.key, ca };
}
