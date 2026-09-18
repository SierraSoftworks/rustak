/**
 * Passkeys, in a browser that has no fingerprint reader.
 *
 * rustak has no passwords at all: the first administrator is created by a
 * setup token and immediately registers a passkey, and every sign-in after
 * that is a WebAuthn ceremony. A suite that could not run one could not sign
 * in, so it could not test anything behind a session.
 *
 * Chromium's CDP `WebAuthn` domain provides a **virtual authenticator**: a real
 * CTAP2 implementation in software, holding real key pairs and producing real
 * assertions, attached to one browser context. The browser's own code path is
 * unchanged — origin checks, relying-party checks, user verification and the
 * signature counter all behave exactly as they would with a security key — so
 * what this exercises is the ceremony, not a stub of it.
 *
 * # Why the ceremonies are driven from inside the page
 *
 * `navigator.credentials` only exists in a page, and only works for the page's
 * own origin. Driving the ceremony from Node would mean reimplementing the
 * parts of WebAuthn that make it worth having. So the ceremony runs in the
 * page, against the page's origin, and only its result comes back.
 */

import type { CDPSession, Page } from "@playwright/test";

/** A credential as the virtual authenticator stores it. */
export interface VirtualCredential {
  credentialId: string;
  isResidentCredential: boolean;
  rpId?: string;
  privateKey: string;
  userHandle?: string;
  signCount: number;
  largeBlob?: string;
  backupEligibility?: boolean;
  backupState?: boolean;
}

/** A virtual authenticator attached to one browser context. */
export interface VirtualAuthenticator {
  readonly id: string;
  readonly client: CDPSession;

  /** Every credential the authenticator currently holds. */
  credentials(): Promise<VirtualCredential[]>;

  /**
   * Re-stores every credential as a **discoverable** (resident) one.
   *
   * See the note in `attachAuthenticator` for why this is needed, and what it
   * is standing in for.
   */
  makeCredentialsDiscoverable(): Promise<number>;

  /** Detaches it, so the context has no way to sign in any more. */
  remove(): Promise<void>;
}

/**
 * Attaches a virtual authenticator to the page's browser context.
 *
 * `isUserVerified` and `hasUserVerification` are both on because the server
 * asks for `userVerification: "required"` (it is `webauthn-rs`'s default for
 * `start_passkey_registration`), and an authenticator that could not perform
 * user verification would have every ceremony refused before it started.
 *
 * `hasResidentKey` is on so that a *discoverable* credential can exist at all.
 * It does not make one: the server sends `residentKey: "discouraged"`, so the
 * credential this authenticator creates is **not** discoverable, and the admin
 * UI's only passkey sign-in is the discoverable one (it never asks for a
 * username). `makeCredentialsDiscoverable` bridges that gap for the specs that
 * sign in through the UI — see `.claude/plan/status/M0-14-e2e-specs.md`, which
 * records it as a server/UI defect rather than a property of this harness.
 */
export async function attachAuthenticator(
  page: Page,
): Promise<VirtualAuthenticator> {
  const client = await page.context().newCDPSession(page);

  await client.send("WebAuthn.enable", { enableUI: false });

  const { authenticatorId } = await client.send(
    "WebAuthn.addVirtualAuthenticator",
    {
      options: {
        protocol: "ctap2",
        transport: "internal",
        hasResidentKey: true,
        hasUserVerification: true,
        isUserVerified: true,
        automaticPresenceSimulation: true,
      },
    },
  );

  const credentials = async (): Promise<VirtualCredential[]> => {
    const { credentials } = await client.send("WebAuthn.getCredentials", {
      authenticatorId,
    });
    return credentials as VirtualCredential[];
  };

  return {
    id: authenticatorId,
    client,

    credentials,

    async makeCredentialsDiscoverable() {
      const held = await credentials();
      const converted = held.filter(
        (credential) => !credential.isResidentCredential,
      );

      for (const credential of converted) {
        await client.send("WebAuthn.removeCredential", {
          authenticatorId,
          credentialId: credential.credentialId,
        });
        await client.send("WebAuthn.addCredential", {
          authenticatorId,
          credential: {
            ...credential,
            isResidentCredential: true,
          },
        });
      }

      return converted.length;
    },

    async remove() {
      await client.send("WebAuthn.removeVirtualAuthenticator", {
        authenticatorId,
      });
    },
  };
}

/** What a passkey ceremony driven through this module answers with. */
export interface CeremonyResult {
  status: number;
  body: Record<string, unknown> | null;
}

/**
 * Registers a passkey against the page's own origin.
 *
 * Authorised either by `registrationToken` — the short-lived token
 * `POST /api/v1/setup/admin` hands back, which is the only way the first
 * administrator can register anything, because the passkey is what is about to
 * give them a session — or by `token`, an existing bearer session adding
 * another way in.
 *
 * The response is the server's, unread: on the bootstrap path it is a
 * `TokenResponse` (the wizard is signed in by the registration, rather than
 * being asked to prove the same thing twice), and otherwise a
 * `PasskeySummary`.
 */
export async function registerPasskey(
  page: Page,
  options: { label: string; registrationToken?: string; token?: string },
): Promise<CeremonyResult> {
  return page.evaluate(async (input) => {
    // Everything a `page.evaluate` needs has to be inside it: the function is
    // serialised and run in the page, so it closes over nothing from here.
    const fromBase64Url = (value: string): Uint8Array => {
      const padded = value.replace(/-/g, "+").replace(/_/g, "/");
      const binary = atob(padded + "=".repeat((4 - (padded.length % 4)) % 4));
      return Uint8Array.from(binary, (character) => character.charCodeAt(0));
    };

    const toBase64Url = (buffer: ArrayBuffer): string => {
      let binary = "";
      for (const byte of new Uint8Array(buffer)) {
        binary += String.fromCharCode(byte);
      }
      return btoa(binary)
        .replace(/\+/g, "-")
        .replace(/\//g, "_")
        .replace(/=+$/, "");
    };

    const post = async (path: string, body: unknown) => {
      const headers: Record<string, string> = {
        "Content-Type": "application/json",
      };
      if (input.token) {
        headers.Authorization = `Bearer ${input.token}`;
      }

      const response = await fetch(`/api/v1${path}`, {
        method: "POST",
        headers,
        body: JSON.stringify(body),
      });
      const text = await response.text();
      return {
        status: response.status,
        body: text ? (JSON.parse(text) as Record<string, unknown>) : null,
      };
    };

    const start = await post("/auth/passkey/register/start", {
      label: input.label,
      registration_token: input.registrationToken,
    });
    if (start.status !== 200 || !start.body) {
      return start;
    }

    // `webauthn-rs` wraps its options in `publicKey`; rustak sends the inner
    // dictionary on its own. Accept either, the same way the admin UI does.
    const wrapper = start.body.options as Record<string, any>;
    const creation = (wrapper.publicKey ?? wrapper) as Record<string, any>;

    creation.challenge = fromBase64Url(creation.challenge);
    creation.user.id = fromBase64Url(creation.user.id);
    for (const entry of creation.excludeCredentials ?? []) {
      entry.id = fromBase64Url(entry.id);
    }

    const credential = (await navigator.credentials.create({
      publicKey: creation as PublicKeyCredentialCreationOptions,
    })) as PublicKeyCredential | null;

    if (!credential) {
      return { status: 0, body: { error: "the browser created no credential" } };
    }

    const attestation = credential.response as AuthenticatorAttestationResponse;

    return post("/auth/passkey/register/finish", {
      challenge_id: start.body.challenge_id,
      label: input.label,
      credential: {
        id: credential.id,
        rawId: toBase64Url(credential.rawId),
        type: credential.type,
        response: {
          clientDataJSON: toBase64Url(attestation.clientDataJSON),
          attestationObject: toBase64Url(attestation.attestationObject),
          transports: attestation.getTransports ? attestation.getTransports() : [],
        },
        extensions: credential.getClientExtensionResults(),
      },
    });
  }, options);
}
