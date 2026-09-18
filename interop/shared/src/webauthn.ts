/**
 * A software WebAuthn authenticator, so the suite can obtain an administrator
 * bearer token without a browser.
 *
 * rustak has no local passwords: the first administrator is created by the
 * setup token and then has to register a passkey, and the token that ceremony
 * returns is the only bearer credential an installation can produce from a
 * cold start (`.claude/plan/status/M0-11-web-api-auth.md`). A Node test suite
 * therefore has to be an authenticator, exactly as
 * `rustak-server/src/testing/authenticator.rs` is for the Rust tests. This is
 * that file's ceremony, written again in TypeScript against `node:crypto`.
 *
 * It is a genuine authenticator — a real RSA key pair, real CBOR, a real
 * RSASSA-PKCS1-v1_5 signature over `authData || sha256(clientDataJSON)`. The
 * server under test cannot tell it is not a phone, which is the point: the
 * bootstrap this suite performs is the one an operator performs.
 *
 * `RS256` rather than the usual `ES256` because it is what the Rust
 * authenticator uses, so the two agree about what rustak accepts.
 */

import crypto from "node:crypto";

/** Base64 as WebAuthn uses it on the wire. */
function b64url(bytes: Buffer | Uint8Array): string {
  return Buffer.from(bytes).toString("base64url");
}

function unb64url(value: string): Buffer {
  return Buffer.from(value, "base64url");
}

/** User present. */
const FLAG_UP = 0x01;

/** User verified. Passkeys are registered with verification required. */
const FLAG_UV = 0x04;

/** Backup eligible. */
const FLAG_BE = 0x08;

/** Backup state. */
const FLAG_BS = 0x10;

/** Attested credential data is present. Registration only. */
const FLAG_AT = 0x40;

/** The COSE identifier for RS256. */
const COSE_RS256 = -257;

/** The COSE key type for RSA. */
const COSE_KTY_RSA = 3;

/** The WebAuthn options a relying party sends, as far as this file reads them. */
export interface CredentialOptions {
  challenge: string;
  user?: { id: string };
  allowCredentials?: Array<{ id: string }>;
}

/** A credential this authenticator holds. */
interface Stored {
  userHandle: Buffer;
  counter: number;
}

/** An authenticator a test can plug in. */
export class SoftAuthenticator {
  private readonly rpId: string;
  private readonly origin: string;
  private readonly keys: { publicKey: crypto.KeyObject; privateKey: crypto.KeyObject };
  private readonly credentials = new Map<string, Stored>();

  constructor(origin: string) {
    const parsed = new URL(origin);

    this.rpId = parsed.hostname;
    this.origin = origin.replace(/\/+$/, "");
    this.keys = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
  }

  /** Performs a registration, as `navigator.credentials.create` would. */
  create(options: CredentialOptions): unknown {
    const challenge = unb64url(options.challenge);
    const userHandle = unb64url(options.user?.id ?? "");
    const credentialId = crypto.randomBytes(32);
    const clientData = clientDataJSON("webauthn.create", challenge, this.origin);

    const attested = Buffer.concat([
      Buffer.alloc(16), // No attestation, so no AAGUID.
      u16(credentialId.length),
      credentialId,
      this.coseKey(),
    ]);

    const authData = this.authData(
      FLAG_UP | FLAG_UV | FLAG_BE | FLAG_BS | FLAG_AT,
      0,
      attested,
    );

    this.credentials.set(b64url(credentialId), { userHandle, counter: 0 });

    return {
      id: b64url(credentialId),
      rawId: b64url(credentialId),
      type: "public-key",
      response: {
        attestationObject: b64url(attestationObject(authData)),
        clientDataJSON: b64url(clientData),
        transports: ["internal", "hybrid"],
      },
      extensions: {},
    };
  }

  /** Performs an assertion, as `navigator.credentials.get` would. */
  get(options: CredentialOptions): unknown {
    const challenge = unb64url(options.challenge);
    const credentialId = this.pick(options);
    const stored = this.credentials.get(credentialId);

    if (!stored) {
      throw new Error("the relying party offered no credential this authenticator holds");
    }

    // A real authenticator increments on every assertion, and the server
    // treats a counter that does not move as a cloned device.
    stored.counter += 1;

    const clientData = clientDataJSON("webauthn.get", challenge, this.origin);
    const authData = this.authData(
      FLAG_UP | FLAG_UV | FLAG_BE | FLAG_BS,
      stored.counter,
      Buffer.alloc(0),
    );

    const signed = Buffer.concat([authData, crypto.createHash("sha256").update(clientData).digest()]);

    return {
      id: credentialId,
      rawId: credentialId,
      type: "public-key",
      response: {
        authenticatorData: b64url(authData),
        clientDataJSON: b64url(clientData),
        signature: b64url(crypto.sign("sha256", signed, this.keys.privateKey)),
        userHandle: b64url(stored.userHandle),
      },
      extensions: {},
    };
  }

  /** The authenticator data for one ceremony. */
  private authData(flags: number, counter: number, attested: Buffer): Buffer {
    const head = Buffer.alloc(37);

    crypto.createHash("sha256").update(this.rpId).digest().copy(head, 0);
    head.writeUInt8(flags, 32);
    head.writeUInt32BE(counter, 33);

    return Buffer.concat([head, attested]);
  }

  /** The device's public key, as COSE. */
  private coseKey(): Buffer {
    const jwk = this.keys.publicKey.export({ format: "jwk" }) as { n: string; e: string };

    return Buffer.concat([
      mapHeader(4),
      uint(0, 1), // 1: key type
      uint(0, COSE_KTY_RSA),
      uint(0, 3), // 3: algorithm
      negative(COSE_RS256),
      negative(-1), // -1: modulus
      byteString(unb64url(jwk.n)),
      negative(-2), // -2: exponent
      byteString(unb64url(jwk.e)),
    ]);
  }

  /** Which credential to assert with. */
  private pick(options: CredentialOptions): string {
    const allowed = options.allowCredentials ?? [];

    if (allowed.length > 0) {
      for (const entry of allowed) {
        const id = b64url(unb64url(entry.id));

        if (this.credentials.has(id)) {
          return id;
        }
      }

      throw new Error("the relying party offered no credential this authenticator holds");
    }

    const first = this.credentials.keys().next();

    if (first.done) {
      throw new Error("this authenticator holds no credential to sign in with");
    }

    return first.value;
  }
}

/** The client data a browser would produce. */
function clientDataJSON(kind: string, challenge: Buffer, origin: string): Buffer {
  return Buffer.from(
    JSON.stringify({
      type: kind,
      challenge: b64url(challenge),
      origin,
      crossOrigin: false,
    }),
    "utf8",
  );
}

/** The attestation object, in the `none` format. */
function attestationObject(authData: Buffer): Buffer {
  return Buffer.concat([
    mapHeader(3),
    textString("fmt"),
    textString("none"),
    textString("attStmt"),
    mapHeader(0),
    textString("authData"),
    byteString(authData),
  ]);
}

/** A CBOR head: the major type and an unsigned argument. */
function uint(major: number, value: number): Buffer {
  const head = major << 5;

  if (value <= 23) return Buffer.from([head | value]);
  if (value <= 0xff) return Buffer.from([head | 24, value]);

  if (value <= 0xffff) {
    const out = Buffer.alloc(3);
    out.writeUInt8(head | 25, 0);
    out.writeUInt16BE(value, 1);
    return out;
  }

  const out = Buffer.alloc(5);
  out.writeUInt8(head | 26, 0);
  out.writeUInt32BE(value, 1);
  return out;
}

/** A CBOR negative integer. */
function negative(value: number): Buffer {
  return uint(1, -1 - value);
}

/** A CBOR byte string. */
function byteString(value: Buffer): Buffer {
  return Buffer.concat([uint(2, value.length), value]);
}

/** A CBOR text string. */
function textString(value: string): Buffer {
  const bytes = Buffer.from(value, "utf8");

  return Buffer.concat([uint(3, bytes.length), bytes]);
}

/** A CBOR map header. */
function mapHeader(entries: number): Buffer {
  return uint(5, entries);
}

/** A big-endian `uint16`, for the credential-id length. */
function u16(value: number): Buffer {
  const out = Buffer.alloc(2);
  out.writeUInt16BE(value, 0);
  return out;
}
