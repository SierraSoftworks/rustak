/**
 * Everything one scenario needs on the rustak side, before any EUD starts.
 *
 * A scenario gets its own server, because a scenario's whole point can be a
 * configuration — `[stream] negotiation = "refuse"` is a different installation
 * from the default one, and running both against one server would mean testing
 * neither. The walk is the same every time:
 *
 *     start rustak with the scenario's configuration overrides
 *     bootstrap an administrator (setup token → passkey → wizard)
 *     create the channels the scenario names
 *     create one account per EUD, and grant it those channels
 *     mint a one-time enrolment token for each
 *     export rustak's CA as the PKCS#12 truststore `estream:` wants
 *
 * The token is what ATAK carries out of a QR code, and ATAK sends it as the
 * Basic **password** with the account name as the user — not as a bearer token
 * (M1-00 §3.1). The scenarios encode it that way, and the Bearer path is only
 * ever tested deliberately.
 */

import fs from "node:fs";
import path from "node:path";

import {
  bootstrap,
  createGroup,
  createUser,
  mintCredential,
  setGroups,
  type Admin,
} from "../../shared/src/bootstrap.js";
import { httpsClient, type HttpClient } from "../../shared/src/http.js";
import {
  startServer,
  waitForPort,
  waitForServer,
  type RunningServer,
} from "../../shared/src/launch.js";

import { exportTruststore } from "./openssl.js";
import type { EudSpec, Scenario } from "./scenario.js";
import type { Substitutions } from "./template.js";

/** The scratch-directory prefix this suite's sweeper recognises. */
const SCRATCH_PREFIX = "rustak-interop-eud-";

/** The password every PKCS#12 in a run carries. ATAK's own default, and no secret. */
export const P12_PASSWORD = "atakatak";

/** The mission-package payload every EUD's directory carries, at `/work/payload.dat`. */
const PAYLOAD = `rustak EUD interop payload\n${"0123456789abcdef".repeat(64)}\n`;

/** One EUD's account, token and mounted directory. */
export interface EudAccount {
  readonly id: string;
  readonly username: string;
  readonly token: string;

  /**
   * The credential the token belongs to.
   *
   * Revoking it cascades to the certificates issued with it, which is the lever
   * `enroll-revoked` pulls until M2-08 serves a per-certificate revoke.
   */
  readonly credentialId: number;

  /** The host directory mounted at `/work`, where its output files land. */
  readonly out: string;
}

/** A started, bootstrapped server with the scenario's accounts on it. */
export interface ScenarioSession {
  readonly server: RunningServer;
  readonly client: HttpClient;
  readonly admin: Admin;
  readonly accounts: ReadonlyMap<string, EudAccount>;

  /** What the placeholders in one EUD's script resolve to. */
  substitutions(eud: EudSpec): Substitutions;

  stop(): void;
}

/** Starts and prepares a server for one scenario. */
export async function openSession(scenario: Scenario): Promise<ScenarioSession> {
  const server = await startServer({
    prefix: SCRATCH_PREFIX,
    name: `rustak-interop-eud-${scenario.name}`,
    host: "localhost",
    config: scenario.config,
    ports: scenario.ports,
  });

  try {
    await waitForServer(server);
    // Every scenario's first command dials the stream, which binds after the
    // public listener does: waiting here keeps a container from racing it.
    await waitForPort(server.ports.stream, 30_000);

    const client = httpsClient(server.webtak, server.caFile);
    const admin = await bootstrap(server, client, {
      adminUsername: "interop-admin",
      displayName: "EUD interop suite",
      serverName: `rustak-interop-eud-${scenario.name}`,
      domains: [server.host],
      baseUrl: server.webtak,
    });

    for (const name of channelsOf(scenario)) {
      await createGroup(client, admin.token, name);
    }

    const accounts = new Map<string, EudAccount>();

    for (const eud of scenario.euds) {
      accounts.set(eud.id, await enrol(server, client, admin, eud));
    }

    return {
      server,
      client,
      admin,
      accounts,
      substitutions(eud) {
        const account = accounts.get(eud.id);

        if (account === undefined) {
          throw new Error(`no account was created for EUD '${eud.id}'`);
        }

        return {
          // An address, not a name: the EUD is in a `--network host` container
          // and resolves nothing, and `commotest`'s normal-enrollment mode does
          // not verify the host name anyway — which is what ATAK does.
          host: "127.0.0.1",
          stream_port: String(server.ports.stream),
          enroll_port: String(server.ports.web),
          marti_port: String(server.ports.marti),
          username: account.username,
          token: account.token,
          truststore: "/work/truststore.p12",
          work: "/work",
          uid: eud.uid,
          callsign: eud.callsign,
        };
      },
      stop: server.stop,
    };
  } catch (error) {
    server.stop();
    throw error;
  }
}

/** Every channel a scenario grants, other than the one every installation has. */
function channelsOf(scenario: Scenario): string[] {
  const names = new Set<string>();

  for (const eud of scenario.euds) {
    for (const grant of eud.channels) {
      if (grant.group !== "__ANON__") names.add(grant.group);
    }
  }

  return [...names];
}

/** Creates one EUD's account, grants its channels and mints its enrolment token. */
async function enrol(
  server: RunningServer,
  client: HttpClient,
  admin: Admin,
  eud: EudSpec,
): Promise<EudAccount> {
  const username = await createUser(client, admin.token, eud.username, `EUD ${eud.callsign}`);

  if (eud.channels.length > 0) {
    await setGroups(client, admin.token, username, eud.channels);
  }

  const minted = await mintCredential(client, admin.token, {
    kind: "enrollment_token",
    label: `EUD ${eud.callsign}`,
    username,
    maxUses: 1,
  });

  const out = path.join(server.directory, "eud", eud.id);

  fs.mkdirSync(out, { recursive: true });
  // Each EUD gets its own copy, because each mounts its own directory at /work
  // and a scenario's `{truststore}` is a path inside the container.
  exportTruststore(server.caFile, path.join(out, "truststore.p12"), P12_PASSWORD);
  // Deterministic bytes for the mission-package scenarios, so `smpsend:` and
  // `mpsend:` always have something to send and a download can be compared.
  fs.writeFileSync(path.join(out, "payload.dat"), PAYLOAD, "utf8");

  return { id: eud.id, username, token: minted.secret, credentialId: minted.id, out };
}

/**
 * Revokes the certificate one EUD enrolled with, while it is connected.
 *
 * Revoking the *credential* was the original lever (M2-09 §5.2) and it does not
 * work for these scenarios: the credential is a **one-time** enrolment token, so
 * by the time the EUD is connected and there is something worth revoking, the
 * token has been spent. `DELETE /api/v1/credentials/{id}` then answers
 * `404 That credential has already gone` — which is what happened on run
 * 35379867680, the first time this scenario ever ran.
 *
 * `POST /api/v1/certificates/{id}/revoke` is the endpoint this scenario's
 * `certificateRevocation` surface is named for; M2-08 landed it. The
 * certificate is found by the account it was issued to, filtered to the active
 * client one, because the listing also carries the CA and the server's own.
 */
export async function revokeEud(session: ScenarioSession, id: string): Promise<void> {
  const account = session.accounts.get(id);

  if (account === undefined) throw new Error(`no account for EUD '${id}'`);

  const query = `username=${encodeURIComponent(account.username)}&state=active&kind=client`;
  const listing = await session.client.request(`/api/v1/certificates?${query}`, {
    token: session.admin.token,
  });

  if (listing.status < 200 || listing.status >= 300) {
    throw new Error(`GET /api/v1/certificates answered ${listing.status}: ${listing.body}`);
  }

  const certificates = JSON.parse(listing.body) as { id: number; subject_cn: string }[];
  const certificate = certificates[0];

  if (certificate === undefined) {
    throw new Error(
      `no active client certificate for '${account.username}' to revoke; the listing was ${listing.body}`,
    );
  }

  const response = await session.client.request(`/api/v1/certificates/${certificate.id}/revoke`, {
    method: "POST",
    token: session.admin.token,
    // `device_lost` is one of the reasons a caller may name; `credential_revoked`
    // and `user_disabled` are the server's own to set.
    body: { reason: "device_lost" },
  });

  if (response.status < 200 || response.status >= 300) {
    throw new Error(
      `POST /api/v1/certificates/${certificate.id}/revoke answered ${response.status}: ${response.body}`,
    );
  }
}

/** The audit trail, as text, for the scenarios that assert the server recorded something. */
export async function auditTrail(session: ScenarioSession): Promise<string> {
  const response = await session.client.request("/api/v1/audit?limit=200", {
    token: session.admin.token,
  });

  return response.body;
}

/** The callsigns rustak says are connected, which is the server half of a scenario. */
export async function connectedCallsigns(session: ScenarioSession): Promise<string[]> {
  const response = await session.client.request(
    "/Marti/api/clientEndPoints?showCurrentlyConnectedClients=true",
    { token: session.admin.token },
  );

  if (response.status < 200 || response.status >= 300) {
    throw new Error(`GET /Marti/api/clientEndPoints answered ${response.status}: ${response.body}`);
  }

  const parsed = JSON.parse(response.body) as { data?: { callsign?: string }[] };

  return (parsed.data ?? []).map((row) => row.callsign ?? "").filter((entry) => entry.length > 0);
}
