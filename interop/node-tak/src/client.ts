/**
 * The three ways CloudTAK talks to a TAK server, built out of node-tak.
 *
 * CloudTAK stores one server as three independent base URLs and authenticates
 * differently against each (`compat/cloudtak.md` §1, §3):
 *
 * | Role | URL | Credential | Verifies the chain? |
 * |---|---|---|---|
 * | `webtak` | this suite's `session.urls.webtak` | username + client password, then the JWT | **yes**, with no override |
 * | `api` | `session.urls.api` | the enrolled client certificate | no (`rejectUnauthorized: false`) |
 * | `url` | `session.urls.stream` | the same certificate | no |
 *
 * That asymmetry is the single most common CloudTAK bring-up failure, so the
 * suite reproduces it rather than trusting everything everywhere: the password
 * clients here go through the global `fetch` and only work because the runner
 * put rustak's authority in `NODE_EXTRA_CA_CERTS`.
 */

import { TAKAPI, APIAuthCertificate, APIAuthPassword, APIAuthToken } from "@tak-ps/node-tak";

import type { Session } from "./session.js";

/** An enrolled identity: what `Credentials.generate()` hands back. */
export interface Enrolled {
  readonly cert: string;
  readonly key: string;
  readonly ca: string[];
}

/**
 * `webtak` under the administrator's bearer token.
 *
 * Not something CloudTAK does — it has only the password — but it is the one
 * client that works before `/oauth/token` exists, so it is what the scenarios
 * that only need a Marti route use.
 */
export function tokenClient(session: Session): TAKAPI {
  return new TAKAPI(new URL(session.urls.webtak), new APIAuthToken(session.admin.token));
}

/**
 * `webtak` under a username and client password, exactly as CloudTAK does.
 *
 * `TAKAPI.init` performs the `/oauth/token` password grant as part of
 * constructing this, so it fails if that endpoint does not answer.
 */
export function passwordClient(session: Session): Promise<TAKAPI> {
  return TAKAPI.init(
    new URL(session.urls.webtak),
    new APIAuthPassword(session.client.username, session.client.password),
  );
}

/**
 * Enrolls a client certificate the way CloudTAK's connection setup does.
 *
 * `Credentials.generate()` is the whole flow: `GET /Marti/api/tls/config` for
 * the name entries, a CSR built from them, then
 * `POST /Marti/api/tls/signClient/v2`.
 */
export async function enroll(session: Session): Promise<Enrolled> {
  const api = await passwordClient(session);

  return await api.Credentials.generate();
}

/** `api` — the mutually authenticated Marti listener — under a client certificate. */
export function certificateClient(session: Session, enrolled: Enrolled): TAKAPI {
  return new TAKAPI(
    new URL(session.urls.api),
    new APIAuthCertificate(enrolled.cert, enrolled.key),
  );
}
