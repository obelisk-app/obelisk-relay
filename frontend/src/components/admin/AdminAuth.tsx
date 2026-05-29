import { useEffect, useRef, useState } from "preact/hooks"
import {
  LoginWidget,
  useLogout,
  useSigner,
} from "@nostr-wot/ui"
import type { NostrSigner } from "@nostr-wot/signers"
import { adminApi } from "../../services/AdminApiClient"
import { Nip46SignerDeepLink } from "./Nip46SignerDeepLink"
import {
  clearStoredSigners,
  restoreNip46SignerWithoutConnectReplay,
  signAdminAuthEvent,
  withTimeout,
} from "./adminSigner"

interface AdminAuthProps {
  onAuthenticated: () => void
}

export const AdminAuth = ({ onAuthenticated }: AdminAuthProps) => {
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const signer = useSigner() as NostrSigner | null
  const logout = useLogout()
  const attemptedPubkeyRef = useRef<string | null>(null)
  const widgetAuthInProgressRef = useRef(false)
  const restoredSignerRef = useRef<NostrSigner | null>(null)

  const authenticateWithSigner = async (activeSigner: NostrSigner) => {
    setError(null)
    setLoading(true)

    try {
      const { challenge } = await adminApi.getChallenge()
      const signedEvent = await withTimeout(
        signAdminAuthEvent(activeSigner, challenge),
        "Signer did not respond. Use another signer or reconnect your Nostr app.",
      )

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
    if (!signer || widgetAuthInProgressRef.current) return

    let cancelled = false
    void (async () => {
      setLoading(true)
      try {
        const pubkey = await withTimeout(
          signer.getPublicKey(),
          "Saved signer did not respond. Use another signer to reset admin login.",
        )
        if (cancelled) return
        if (attemptedPubkeyRef.current === pubkey) {
          setLoading(false)
          return
        }
        attemptedPubkeyRef.current = pubkey
        await authenticateWithSigner(signer).catch(() => undefined)
      } catch (e) {
        if (cancelled) return
        const message = e instanceof Error ? e.message : "Signer is unavailable"
        setError(message)
        setLoading(false)
      }
    })()

    return () => {
      cancelled = true
    }
  }, [signer])

  useEffect(() => {
    if (signer || restoredSignerRef.current) return

    let cancelled = false
    void (async () => {
      try {
        const restored = await restoreNip46SignerWithoutConnectReplay()
        if (cancelled || !restored) return
        restoredSignerRef.current = restored
        await authenticateWithSigner(restored).catch(() => undefined)
      } catch {
        // The regular login widget remains available.
      }
    })()

    return () => {
      cancelled = true
    }
  }, [signer])

  const handleWidgetLogin = async ({ signer: sdkSigner }: { signer: NostrSigner }) => {
    widgetAuthInProgressRef.current = true
    try {
      attemptedPubkeyRef.current = await withTimeout(
        sdkSigner.getPublicKey(),
        "Signer did not respond. Use another signer or reconnect your Nostr app.",
      )
      await authenticateWithSigner(sdkSigner)
    } finally {
      widgetAuthInProgressRef.current = false
    }
  }

  const switchIdentity = async () => {
    attemptedPubkeyRef.current = null
    setError(null)
    await clearStoredSigners(restoredSignerRef.current ?? signer)
    restoredSignerRef.current = null
    void logout()
  }

  return (
    <div class="min-h-screen flex items-center justify-center px-4" style={{ background: "var(--color-bg-primary)" }}>
      <Nip46SignerDeepLink />
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
            onLogin={handleWidgetLogin}
          />
        ) : (
          <div class="space-y-3">
            {!error && (
              <div class="flex items-center justify-center gap-2 py-3 text-sm" style={{ color: "var(--color-text-secondary)" }}>
                <span class="lc-spinner" style={{ width: "16px", height: "16px", borderTopColor: "#b4f953" }} />
                {loading ? "Authenticating..." : "Preparing signer..."}
              </div>
            )}
            <button
              onClick={switchIdentity}
              class="w-full lc-pill-secondary py-3 text-base"
              style={{ borderRadius: "10px" }}
            >
              Use another signer
            </button>
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
