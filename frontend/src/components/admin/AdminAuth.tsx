import { useEffect, useRef, useState } from "preact/hooks"
import {
  LoginWidget,
  clearPersistedNip46,
  clearPersistedNsec,
  useLogout,
  useSigner,
} from "@nostr-wot/ui"
import type { NostrSigner } from "@nostr-wot/signers"
import { adminApi } from "../../services/AdminApiClient"

interface AdminAuthProps {
  onAuthenticated: () => void
}

export const AdminAuth = ({ onAuthenticated }: AdminAuthProps) => {
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const signer = useSigner() as NostrSigner | null
  const logout = useLogout()
  const attemptedPubkeyRef = useRef<string | null>(null)

  const authenticateWithSigner = async (activeSigner: NostrSigner) => {
    setError(null)
    setLoading(true)

    try {
      const { challenge } = await adminApi.getChallenge()
      const signedEvent = await activeSigner.signEvent({
        kind: 22242,
        created_at: Math.floor(Date.now() / 1000),
        tags: [
          ["relay", window.location.origin.replace("http", "ws")],
          ["challenge", challenge],
        ],
        content: "",
      })

      await adminApi.authenticate(signedEvent)
      onAuthenticated()
    } catch (e) {
      const message = e instanceof Error ? e.message : "Authentication failed"
      setError(message)
      throw new Error(message)
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    if (!signer) return

    let cancelled = false
    void (async () => {
      const pubkey = await signer.getPublicKey()
      if (cancelled || attemptedPubkeyRef.current === pubkey) return
      attemptedPubkeyRef.current = pubkey
      await authenticateWithSigner(signer).catch(() => undefined)
    })()

    return () => {
      cancelled = true
    }
  }, [signer])

  const switchIdentity = () => {
    attemptedPubkeyRef.current = null
    setError(null)
    void signer?.close?.()
    void clearPersistedNip46()
    void clearPersistedNsec()
    void logout()
  }

  return (
    <div class="min-h-screen flex items-center justify-center px-4" style={{ background: "var(--color-bg-primary)" }}>
      <div class="max-w-md w-full lc-card lc-glow p-8">
        <h1 class="text-2xl font-bold mb-2 text-center lc-glow-text" style={{ color: "#b4f953" }}>Admin Panel</h1>
        <p class="text-sm text-center mb-6" style={{ color: "var(--color-text-secondary)" }}>
          Sign in with your Nostr identity to manage the relay.
        </p>

        {error && (
          <div class="mb-4 p-3 rounded-lg text-sm bg-red-500/10 text-red-400 border border-red-500/20">
            {error}
          </div>
        )}

        {!signer ? (
          <LoginWidget
            title="Sign in as relay admin"
            subtitle="Use the operator Nostr key authorized for this relay."
            methods={["nip07", "nip46", "import"]}
            flatLayout
            showRememberToggle
            nip46Mode="qr"
            nip46Relays={["wss://relay.nsec.app", "wss://relay.damus.io"]}
            nip46Metadata={{
              name: "Obelisk Relay Admin",
              url: window.location.origin,
              description: "Admin login for Obelisk relay",
            }}
          />
        ) : (
          <div class="space-y-3">
            {!error && (
              <div class="flex items-center justify-center gap-2 py-3 text-sm" style={{ color: "var(--color-text-secondary)" }}>
                <span class="lc-spinner" style={{ width: "16px", height: "16px", borderTopColor: "#b4f953" }} />
                {loading ? "Authenticating..." : "Preparing signer..."}
              </div>
            )}
            {error && (
              <button
                onClick={switchIdentity}
                class="w-full lc-pill-secondary py-3 text-base"
                style={{ borderRadius: "10px" }}
              >
                Use another signer
              </button>
            )}
            <div class="text-center pt-2">
              <a href="/" class="text-sm hover:underline" style={{ color: "var(--color-text-secondary)" }}>
                Back to home
              </a>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
