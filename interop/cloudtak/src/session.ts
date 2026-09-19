/**
 * Getting from "the stack is up" to "CloudTAK has something to log in with".
 *
 * rustak has no local passwords and no create-an-admin flag, so the walk is the
 * same cold start every suite performs — setup token, first administrator, a
 * passkey, the wizard — and it is `interop/shared/src/bootstrap.ts` that
 * performs it. What is this suite's own is what comes after: an ordinary
 * account with a client password (CloudTAK's only credential), a channel to put
 * a Data Sync in, and the client certificate CloudTAK stores as its admin
 * connection.
 *
 * Deliberately *not* the administrator: CloudTAK makes the first account that
 * logs in its own system administrator, and a suite that handed it rustak's
 * administrator would never notice an authorisation bug that bites everybody
 * else.
 */

import {
  bootstrap,
  createGroup,
  createUser,
  mintCredential,
  setGroups,
  type Admin,
} from "../../shared/src/bootstrap.js";
import { httpsClient } from "../../shared/src/http.js";
import { probeSurfaces } from "../../shared/src/probe.js";

import { CloudTak } from "./client.js";
import { enrol, type Enrolled } from "./enroll.js";
import { onboardCloudTak } from "./onboard.js";
import { request, waitFor } from "./http.js";
import { PKI } from "./pki.js";
import { serverInfo, WIZARD } from "./rustak.js";
import { CONTAINER_URLS, HOST_URLS, NAMES } from "./settings.js";
import { SURFACES, type SurfaceName } from "./surfaces.js";

/** Everything a run needs once the stack is up. */
export interface Session {
  readonly admin: Admin;
  readonly operator: { readonly username: string; readonly password: string };
  readonly surfaces: Record<SurfaceName, boolean>;
  readonly cloudtak: CloudTak;
  readonly enrolled?: Enrolled;

  /** Why there is no certificate, when there is none. */
  readonly enrollmentFailure?: string;
}

/** Waits for both halves of the stack to answer. */
export async function waitForStack(timeoutMs = 240_000): Promise<void> {
  await waitFor(
    `rustak at ${HOST_URLS.webtak}`,
    async () => (await request(`${HOST_URLS.webtak}/api/v1/health`, PKI.caCert)).status,
    timeoutMs,
  );

  // CloudTAK runs its database migrations and seeds its iconsets on first
  // start, which is minutes rather than seconds on a cold volume.
  await waitFor(`CloudTAK at ${HOST_URLS.cloudtak}`, () => new CloudTak(HOST_URLS.cloudtak).version(), timeoutMs);
}

/** Performs the cold start and mints everything CloudTAK will be given. */
export async function prepareSession(): Promise<Session> {
  const server = serverInfo();
  const client = httpsClient(server.webtak, server.caFile);

  const admin = await bootstrap(server, client, WIZARD);

  await createGroup(client, admin.token, NAMES.channel);

  const username = await createUser(client, admin.token, NAMES.operator, "CloudTAK interop operator");

  await setGroups(client, admin.token, username, [{ group: NAMES.channel, direction: "BOTH" }]);

  const minted = await mintCredential(client, admin.token, {
    kind: "client_password",
    label: "CloudTAK interop suite",
    username,
    expiresInDays: 1,
  });

  const operator = { username, password: minted.secret };

  const surfaces = await probeSurfaces(SURFACES, {
    client,
    token: admin.token,
    stream: server.stream,
  });

  const session: Session = {
    admin,
    operator,
    surfaces,
    cloudtak: new CloudTak(HOST_URLS.cloudtak),
  };

  // The operator's way round, where the server offers it: one call produces
  // the certificate, and the key never goes near a shell. The CSR path below
  // stays for a server that predates it — and is what every real TAK client
  // still does.
  if (surfaces.cloudtakOnboarding) {
    try {
      const { onboarding, ...enrolled } = await onboardCloudTak({
        webtak: server.webtak,
        caFile: server.caFile,
        token: admin.token,
        username: operator.username,
        // The container's own names and ports, because that is what CloudTAK
        // has to reach rustak by from inside the compose network.
        host: new URL(CONTAINER_URLS.webtak).hostname,
        ports: {
          stream: Number(new URL(CONTAINER_URLS.stream.replace("ssl://", "https://")).port),
          marti: Number(new URL(CONTAINER_URLS.api).port),
          public: Number(new URL(CONTAINER_URLS.webtak).port),
        },
      });

      // The three URLs are the whole other half of what CloudTAK's setup page
      // asks for, and they are what this suite then configures it with. An
      // echo that disagrees with the compose network is a server that would
      // hand a real operator URLs nothing can resolve, so it fails here rather
      // than three steps later as a connection refused.
      for (const [name, expected] of Object.entries(CONTAINER_URLS)) {
        const answered = onboarding.urls[name as keyof typeof CONTAINER_URLS];

        if (answered !== expected) {
          throw new Error(`onboarding echoed ${name} as '${answered}', not '${expected}'.`);
        }
      }

      return { ...session, enrolled };
    } catch (error) {
      return { ...session, enrollmentFailure: error instanceof Error ? error.message : String(error) };
    }
  }

  if (!surfaces.tlsConfig) {
    return { ...session, enrollmentFailure: SURFACES.tlsConfig.todo };
  }

  try {
    const enrolled = await enrol({
      webtak: server.webtak,
      caFile: server.caFile,
      username: operator.username,
      password: operator.password,
      // CloudTAK derives a connection's uid from its certificate subject; the
      // value here is only what the device identity is recorded as.
      clientUid: "cloudtak-interop-admin",
    });

    return { ...session, enrolled };
  } catch (error) {
    return { ...session, enrollmentFailure: error instanceof Error ? error.message : String(error) };
  }
}
