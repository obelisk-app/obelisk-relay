import type { NostrSigner } from "@nostr-wot/signers";

/**
 * Adapts a `@nostr-wot/signers` NostrSigner to the shape NDK expects.
 *
 * `@nostr-wot/signers` shipped `nostrSignerAsNdkSigner` up to 0.4.x and dropped
 * the whole NDK adapter in 1.0 — the package no longer depends on NDK at all,
 * and no other `@nostr-wot/*` package picked it up. We still drive NDK, so the
 * adapter lives here now. Ported from the 0.4.0 implementation.
 *
 * The result is structurally an NDKSigner but is not an instance of NDK's
 * class, so callers cast it; NDK only ever calls the methods below.
 */

interface NdkUserLike {
  pubkey: string;
}

interface NdkUserCtor {
  new (opts: { pubkey: string }): NdkUserLike;
}

interface NdkEventLike {
  kind?: number;
  created_at?: number;
  tags?: string[][];
  content: string;
}

export interface NostrAsNdkOptions {
  /** NDK's NDKUser constructor. Without it, a bare `{ pubkey }` is returned. */
  NDKUser?: NdkUserCtor;
}

export async function nostrSignerAsNdkSigner(
  signer: NostrSigner,
  opts: NostrAsNdkOptions = {},
) {
  const pubkey = await signer.getPublicKey();
  const user: NdkUserLike = opts.NDKUser
    ? new opts.NDKUser({ pubkey })
    : { pubkey };

  return {
    get pubkey() {
      return pubkey;
    },
    get userSync() {
      return user;
    },
    async user() {
      return user;
    },
    async blockUntilReady() {
      return user;
    },

    // NDK wants only the signature back, not the finalized event.
    async sign(event: NdkEventLike): Promise<string> {
      if (typeof event.kind !== "number") {
        throw new Error("nostrSignerAsNdkSigner: event is missing `kind`");
      }
      const signed = await signer.signEvent({
        kind: event.kind,
        created_at: event.created_at ?? Math.floor(Date.now() / 1000),
        tags: event.tags ?? [],
        content: event.content,
      });
      return signed.sig;
    },

    async encrypt(
      recipient: NdkUserLike,
      value: string,
      scheme?: "nip04" | "nip44",
    ): Promise<string> {
      if (scheme === "nip44") {
        if (!signer.nip44Encrypt) {
          throw new Error("signer does not support NIP-44 encryption");
        }
        return signer.nip44Encrypt(recipient.pubkey, value);
      }
      if (!signer.nip04Encrypt) {
        throw new Error("signer does not support NIP-04 encryption");
      }
      return signer.nip04Encrypt(recipient.pubkey, value);
    },

    async decrypt(
      sender: NdkUserLike,
      value: string,
      scheme?: "nip04" | "nip44",
    ): Promise<string> {
      if (scheme === "nip44") {
        if (!signer.nip44Decrypt) {
          throw new Error("signer does not support NIP-44 decryption");
        }
        return signer.nip44Decrypt(sender.pubkey, value);
      }
      if (!signer.nip04Decrypt) {
        throw new Error("signer does not support NIP-04 decryption");
      }
      return signer.nip04Decrypt(sender.pubkey, value);
    },

    async encryptionEnabled(scheme?: "nip04" | "nip44") {
      const out: string[] = [];
      if (signer.nip04Encrypt) out.push("nip04");
      if (signer.nip44Encrypt) out.push("nip44");
      if (scheme) return out.includes(scheme) ? [scheme] : [];
      return out;
    },

    toPayload() {
      return JSON.stringify({ type: "nostr-as-ndk", pubkey });
    },
  };
}
