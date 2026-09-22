/**
 * Walks a fresh rustak to the credential CloudTAK authenticates with.
 *
 * The ceremony itself — setup token, first administrator, a passkey registered
 * with a software authenticator, the wizard — lives in
 * `interop/shared/src/bootstrap.ts`, because `interop/eud` performs exactly the
 * same walk before it can mint an enrolment token. What is this suite's own is
 * the pair of accounts it ends up with:
 *
 * - **the administrator**, whose bearer token is the only credential an
 *   installation can produce from a cold start, and
 * - **an ordinary account with a client password**, which is what CloudTAK
 *   actually authenticates as. Deliberately not the administrator: a suite that
 *   only ever exercised an administrator would miss an authorisation bug that
 *   bites everybody else.
 *
 * Every call goes through the global `fetch`, so the chain is verified against
 * `NODE_EXTRA_CA_CERTS` exactly as node-tak's own `webtak` calls are.
 */

import { bootstrap as walk, createUser, mintCredential } from "../../shared/src/bootstrap.js";
import { fetchClient } from "../../shared/src/http.js";

import { SERVER_NAME, type ServerInfo } from "./rustak.js";
import type { Credentials } from "./session.js";

/** The account the wizard creates. */
const ADMIN_USERNAME = "interop-admin";

/** The ordinary account the scenarios enrol and authenticate as. */
const CLIENT_USERNAME = "interop-eud";

/** What the bootstrap produced. */
export interface Bootstrapped {
  readonly admin: { readonly username: string; readonly token: string };
  readonly client: Credentials;
}

/** Performs the whole walk against a server that has just started. */
export async function bootstrap(server: ServerInfo): Promise<Bootstrapped> {
  const client = fetchClient(server.webtak);

  const admin = await walk(server, client, {
    adminUsername: ADMIN_USERNAME,
    displayName: "node-tak interop suite",
    serverName: SERVER_NAME,
    domains: ["localhost"],
    baseUrl: server.webtak,
  });

  const username = await createUser(
    client,
    admin.token,
    CLIENT_USERNAME,
    "node-tak interop EUD",
  );

  const minted = await mintCredential(client, admin.token, {
    kind: "client_password",
    label: "node-tak interop suite",
    username,
    expiresInDays: 1,
  });

  return { admin, client: { username, password: minted.secret } };
}
