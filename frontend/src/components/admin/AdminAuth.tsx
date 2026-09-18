import { useEffect, useRef, useState } from "preact/hooks"
import {
  LoginWidget,
  useLogout,
  useSigner,
} from "@nostr-wot/ui"
import type { NostrSigner } from "@nostr-wot/signers"
import { NIP46_RELAYS } from "../../constants"
import { adminApi } from "../../services/AdminApiClient"
import { Nip46SignerDeepLink } from "./Nip46SignerDeepLink"
import {
  clearStoredSigners,
  isDeadSignerError,
  restoreNip46SignerWithoutConnectReplay,
  signAdminAuthEvent,
  withTimeout,
} from "./adminSigner"
import { LOGIN_METHOD_ICONS } from "./loginIcons"

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
  /**
   * The operator asked to switch identity, and has not picked a new one yet.
   *
   * Logging out is not enough on its own. A NIP-07 extension is always present
   * in `window.nostr`, so the SDK re-derives a signer for it immediately — and
   * the auto-authenticate effect below would sign straight back in with the key
   * we were just asked to abandon. "Use another signer" appeared to need two
   * presses because the first one was silently undone.
   *
   * While this is set, nothing authenticates without an explicit choice.
   */
  const [awaitingChoice, setAwaitingChoice] = useState(false)

  const authenticateWithSigner = async (activeSigner: NostrSigner) => {
    setError(null)
    setLoading(true)

    try {
      // Challenge first, then sign immediately. Logging in needs exactly one
      // signature, so the less time spent holding a remote signer open between
      // acquiring it and using it, the fewer ways it can die mid-flight.
      const { challenge } = await adminApi.getChallenge()

      let signedEvent
      try {
        signedEvent = await withTimeout(
          signAdminAuthEvent(activeSigner, challenge),
          "Signer did not respond. Use another signer or reconnect your Nostr app.",
        )
      } catch (signError) {
        // A NIP-46 connection that has been closed cannot be revived, but the
        // stored session can build a fresh one paired with the same client
        // identity. Rebuild and retry once rather than dead-ending the operator
        // on "create a new one" when we can create it for them.
        if (!isDeadSignerError(signError)) throw signError

        const rebuilt = await restoreNip46SignerWithoutConnectReplay()
        if (!rebuilt) throw signError

        restoredSignerRef.current = rebuilt
        signedEvent = await withTimeout(
          signAdminAuthEvent(rebuilt, challenge),
          "Signer did not respond. Use another signer or reconnect your Nostr app.",
        )
      }

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
    if (!signer || widgetAuthInProgressRef.current || awaitingChoice) return

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
        // Not `.catch(() => undefined)`: swallowing here meant a dead NIP-46
        // session never reached the handler below, so the raw "this signer is
        // not open anymore" surfaced and the stale session was kept -- failing
        // identically on every reload.
        try {
          await authenticateWithSigner(signer)
        } catch (authError) {
          if (cancelled) return
          if (isDeadSignerError(authError)) {
            await discardDeadSigner(signer)
          }
          // Other failures already showed their message via
          // authenticateWithSigner; the widget stays available.
        }
      } catch (e) {
        if (cancelled) return
        if (isDeadSignerError(e)) {
          await discardDeadSigner(signer)
          return
        }
        const message = e instanceof Error ? e.message : "Signer is unavailable"
        setError(message)
        setLoading(false)
      }
    })()

    return () => {
      cancelled = true
    }
  }, [signer, awaitingChoice])

  useEffect(() => {
    if (signer || restoredSignerRef.current || awaitingChoice) return

    let cancelled = false
    void (async () => {
      let restored: NostrSigner | null = null
      try {
        restored = await restoreNip46SignerWithoutConnectReplay()
        if (cancelled || !restored) return
        restoredSignerRef.current = restored
        await authenticateWithSigner(restored)
      } catch (e) {
        if (cancelled) return
        // A restored session pointing at a bunker that no longer exists would
        // otherwise be retried on every load, failing identically each time.
        if (isDeadSignerError(e)) {
          await discardDeadSigner(restored)
          return
        }
        // Anything else: the regular login widget remains available.
      }
    })()

    return () => {
      cancelled = true
    }
  }, [signer, awaitingChoice])

  const handleWidgetLogin = async ({ signer: sdkSigner }: { signer: NostrSigner }) => {
    widgetAuthInProgressRef.current = true
    // An explicit pick from the widget is the choice we were waiting for.
    setAwaitingChoice(false)
    try {
      attemptedPubkeyRef.current = await withTimeout(
        sdkSigner.getPublicKey(),
        "Signer did not respond. Use another signer or reconnect your Nostr app.",
      )
      await authenticateWithSigner(sdkSigner)
    } catch (e) {
      // The restore paths handled this; the widget path did not, so picking a
      // bunker that then died left the raw library error on screen with the
      // dead session still stored.
      if (isDeadSignerError(e)) {
        await discardDeadSigner(sdkSigner)
        return
      }
      throw e
    } finally {
      widgetAuthInProgressRef.current = false
    }
  }

  const switchIdentity = async () => {
    // Set first, so the auto-authenticate effect is already suppressed by the
    // time logging out causes the SDK to re-derive an extension signer.
    setAwaitingChoice(true)
    setError(null)
    setLoading(false)
    attemptedPubkeyRef.current = null
    await clearStoredSigners(restoredSignerRef.current ?? signer)
    restoredSignerRef.current = null
    // Awaited: leaving this dangling let the component settle mid-logout.
    await logout()
  }

  /**
   * A stored NIP-46 session whose bunker connection is gone can never succeed,
   * and it is restored again on every load -- so surfacing the raw error just
   * dead-ends the operator on a screen that says "create a new one" while
   * holding the dead one. Drop it and show the login widget instead.
   */
  const discardDeadSigner = async (deadSigner: NostrSigner | null) => {
    await clearStoredSigners(deadSigner)
    restoredSignerRef.current = null
    attemptedPubkeyRef.current = null
    setError("Your saved signer connection expired. Sign in again.")
    setLoading(false)
    // A dead signer is not a choice either -- wait for a deliberate one rather
    // than letting an extension signer be picked up automatically.
    setAwaitingChoice(true)
    await logout()
  }

  return (
    <div class="min-h-screen flex items-center justify-center px-4" style={{ background: "var(--color-bg-primary)" }}>
      <Nip46SignerDeepLink />
      <div class="max-w-md w-full lc-card lc-glow p-8">
        <h1 class="text-2xl font-bold mb-2 text-center lc-glow-text" style={{ color: "var(--color-accent)" }}>Admin Panel</h1>
        <p class="text-sm text-center mb-6" style={{ color: "var(--color-text-secondary)" }}>
          Sign in with your Nostr identity to manage the relay.
        </p>

        {error && (
          <div class="mb-4 p-3 rounded-lg text-sm bg-red-500/10 text-red-400 border border-red-500/20">
            {error}
          </div>
        )}

        {!signer || awaitingChoice ? (
          <div class="obelisk-login">
          <LoginWidget
            title="Sign in as relay admin"
            subtitle="Use the operator Nostr key authorized for this relay."
            methods={["nip07", "nip46", "import"]}
            methodIcons={LOGIN_METHOD_ICONS}
            flatLayout
            showRememberToggle
            nip46Mode="qr"
            nip46Relays={[...NIP46_RELAYS]}
            nip46Metadata={{
              name: "Obelisk Relay Admin",
              url: window.location.origin,
              description: "Admin login for Obelisk relay",
            }}
            onLogin={handleWidgetLogin}
          />
          </div>
        ) : (
          <div class="space-y-3">
            {!error && (
              <div class="flex items-center justify-center gap-2 py-3 text-sm" style={{ color: "var(--color-text-secondary)" }}>
                <span class="lc-spinner" style={{ width: "16px", height: "16px", borderTopColor: "var(--color-accent)" }} />
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
