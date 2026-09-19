/**
 * Getting CloudTAK's admin identity out of rustak in one call.
 *
 * `src/enroll.ts` is the long way round, and it is what a real CloudTAK does
 * for every user it signs in: `GET /Marti/api/tls/config`, a CSR built from the
 * name entries it reports, `POST /Marti/api/tls/signClient/v2`. That path stays
 * — it is the contract every TAK client uses — but it is not how an *operator*
 * sets CloudTAK up, because CloudTAK's *Configure Server* page takes an
 * administrator identity as an uploaded `.p12` and cannot enrol for it.
 *
 * `POST /api/v1/users/{username}/cloudtak-onboarding` (M5-03) is the one action
 * that produces it: a client password, a certificate, the three URLs, and a
 * PKCS#12 that downloads exactly once. This module drives it and converts the
 * bundle into the PEM pair CloudTAK's `PATCH /api/server` wants, so the rest of
 * the suite sees the same [`Enrolled`] either way.
 *
 * # `openssl` rather than a library
 *
 * The bundle is deliberately written with the legacy algorithms CloudTAK's own
 * parser reads — PBES1 with 3DES and a SHA-1 MAC — and this suite has no
 * PKCS#12 reader. `openssl` is already a hard requirement of the runner
 * (`src/run.ts`) and `interop/eud/src/openssl.ts` already shells out to it for
 * exactly this family of file, so a new dependency and a new lock file would
 * buy nothing. The parser CloudTAK *itself* uses, `@tak-ps/node-p12`, is
 * exercised in `interop/node-tak/tests/cloudtak-onboarding.test.ts`, where it
 * is already a dependency.
 */

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

import type { Enrolled } from "./enroll.js";
import { request } from "./http.js";
import { PKI_DIR } from "./settings.js";

/** The route, which takes the account in its path. */
export function onboardingPath(username: string): string {
  return `/api/v1/users/${encodeURIComponent(username)}/cloudtak-onboarding`;
}

/** What the server answers with, as `rustak_api::CloudTakOnboarding`. */
export interface Onboarding {
  readonly username: string;

  /** Shown once. Absent when an existing credential was reused. */
  readonly password?: string;

  /** Works exactly once; a second fetch is a `410`. */
  readonly p12DownloadUrl: string;

  readonly p12Password: string;
  readonly urls: { readonly stream: string; readonly api: string; readonly webtak: string };
}

/**
 * Reads the hand-over out of the response body.
 *
 * Named failures for the two fields the rest of this suite cannot proceed
 * without, because "undefined is not a string" three calls later is not a bug
 * report anybody can act on.
 */
export function parseOnboarding(body: string): Onboarding {
  let parsed: unknown;

  try {
    parsed = JSON.parse(body);
  } catch {
    throw new Error(`the onboarding endpoint did not answer JSON. It answered: ${body.slice(0, 200)}`);
  }

  // An array is an object to `typeof`, and an array here means the endpoint
  // answered something else entirely — saying "no fields" would send a reader
  // looking for a missing field instead.
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error("the onboarding endpoint answered JSON that is not an object.");
  }

  const fields = parsed as Record<string, unknown>;
  const url = fields.p12_download_url;
  const passphrase = fields.p12_password;

  if (typeof url !== "string" || url.length === 0) {
    throw new Error(
      `onboarding answered without a 'p12_download_url': ${Object.keys(fields).join(", ") || "(no fields)"}`,
    );
  }

  if (typeof passphrase !== "string" || passphrase.length === 0) {
    throw new Error("onboarding answered without a 'p12_password', so the keystore cannot be opened.");
  }

  const urls = (fields.urls ?? {}) as Record<string, unknown>;

  for (const name of ["stream", "api", "webtak"]) {
    if (typeof urls[name] !== "string" || (urls[name] as string).length === 0) {
      throw new Error(`onboarding answered without a '${name}' URL, which CloudTAK's setup requires.`);
    }
  }

  return {
    username: String(fields.username ?? ""),
    password: typeof fields.password === "string" ? fields.password : undefined,
    p12DownloadUrl: url,
    p12Password: passphrase,
    urls: {
      stream: urls.stream as string,
      api: urls.api as string,
      webtak: urls.webtak as string,
    },
  };
}

/**
 * The `openssl` arguments that read a legacy bundle into PEM.
 *
 * `-legacy` because the file is deliberately PBES1/3DES — that is what
 * CloudTAK's parser reads — and OpenSSL 3 will not touch those without it.
 * `-nodes` because the PEM has to go into CloudTAK's `auth.key` unencrypted.
 */
export function p12ReadArgs(file: string, passphrase: string): string[] {
  return ["pkcs12", "-legacy", "-in", file, "-nodes", "-passin", `pass:${passphrase}`];
}

/**
 * Splits `openssl pkcs12` output into the leaf, the key and the chain.
 *
 * OpenSSL prints bag attributes between the blocks, and prints the leaf before
 * the authorities, so the first certificate is the client's and the rest are
 * the chain — which is the same order the bundle stores them in and the order
 * CloudTAK's own parser depends on.
 */
export function splitPem(output: string): Enrolled {
  const blocks = output.match(/-----BEGIN [^-]+-----[\s\S]*?-----END [^-]+-----/g) ?? [];
  const certificates = blocks.filter((block) => block.includes("BEGIN CERTIFICATE"));
  const keys = blocks.filter((block) => block.includes("PRIVATE KEY"));

  if (certificates.length === 0) throw new Error("the keystore held no certificate.");
  if (keys.length === 0) throw new Error("the keystore held no private key.");

  return {
    cert: `${certificates[0]!}\n`,
    key: `${keys[0]!}\n`,
    ca: certificates.slice(1).map((block) => `${block}\n`),
  };
}

/** Everything one hand-over needs to reach the server and prove who is asking. */
export interface OnboardTarget {
  /** The `webtak` base URL, as the runner reaches it. */
  readonly webtak: string;

  /** The authority the runner verifies that listener against. */
  readonly caFile: string;

  /** The administrator's bearer token: this route is administrative. */
  readonly token: string;

  /** The account CloudTAK will sign in as. */
  readonly username: string;

  /** The host CloudTAK reaches rustak by, and the ports it is published on. */
  readonly host?: string;
  readonly ports?: { readonly stream: number; readonly marti: number; readonly public: number };
}

/** Prepares a hand-over and turns its keystore into the PEM pair CloudTAK stores. */
export async function onboardCloudTak(target: OnboardTarget): Promise<Enrolled & { onboarding: Onboarding }> {
  const authorization = `Bearer ${target.token}`;

  const prepared = await request(`${target.webtak}${onboardingPath(target.username)}`, target.caFile, {
    method: "POST",
    headers: { Authorization: authorization, "Content-Type": "application/json" },
    body: JSON.stringify({ credential: "mint", host: target.host, ports: target.ports }),
    timeoutMs: 60_000,
  });

  if (prepared.status !== 200) {
    throw new Error(
      `POST ${onboardingPath(target.username)} answered ${prepared.status}: ${prepared.body.slice(0, 300)}`,
    );
  }

  const onboarding = parseOnboarding(prepared.body);
  const file = path.join(fs.mkdtempSync(path.join(PKI_DIR, "cloudtak-")), "admin.p12");

  fs.writeFileSync(file, await collect(target, onboarding.p12DownloadUrl, authorization), { mode: 0o600 });

  const read = spawnSync("openssl", p12ReadArgs(file, onboarding.p12Password), {
    encoding: "utf8",
    timeout: 60_000,
  });

  if (read.status !== 0) {
    throw new Error(`openssl could not read the keystore (${String(read.status)}): ${read.stderr}`);
  }

  return { ...splitPem(read.stdout), onboarding };
}

/**
 * Fetches the keystore itself, which is binary and downloads once.
 *
 * `request` in `src/http.ts` decodes as UTF-8, which would corrupt a DER file
 * beyond recognition, so this is the one call in the suite that reads raw
 * bytes.
 */
async function collect(target: OnboardTarget, url: string, authorization: string): Promise<Buffer> {
  const { default: https } = await import("node:https");
  const parsed = new URL(url, target.webtak);
  const ca = fs.readFileSync(target.caFile);

  return await new Promise<Buffer>((resolve, reject) => {
    const call = https.request(
      parsed,
      { headers: { Authorization: authorization }, ca, servername: parsed.hostname, timeout: 60_000 },
      (answer) => {
        const chunks: Buffer[] = [];

        answer.on("data", (chunk: Buffer) => chunks.push(chunk));
        answer.on("end", () => {
          if (answer.statusCode !== 200) {
            reject(new Error(`GET ${url} answered ${String(answer.statusCode)} rather than the keystore.`));
            return;
          }

          resolve(Buffer.concat(chunks));
        });
      },
    );

    call.once("timeout", () => call.destroy(new Error(`GET ${url} timed out`)));
    call.once("error", reject);
    call.end();
  });
}
