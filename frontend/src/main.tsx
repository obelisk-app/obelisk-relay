import "preact/debug"
import "preact/devtools"
import "@nostr-wot/ui/styles.css"
import { render } from "preact"
import { useEffect, useRef, useState } from "preact/hooks"
import Router from "preact-router"
import {
  NostrSessionProvider,
  SIGNER_STORAGE_KEY_NIP46,
  SIGNER_STORAGE_KEY_NSEC,
  clearPersistedNip46,
  clearPersistedNsec,
  useLogin,
  useLogout,
  useSigner,
} from "@nostr-wot/ui"
import { PrivateKeySigner, type NostrSigner } from "@nostr-wot/signers"
import { nip19 } from "nostr-tools"
import { NostrClient } from "./api/nostr_client.ts"
import { App } from "./components/App.tsx"
import { LoadingState } from "./components/LoadingState.tsx"
import { ErrorState } from "./components/ErrorState.tsx"
import { AuthPrompt } from "./components/AuthPrompt.tsx"
import { LandingPage } from "./components/LandingPage.tsx"
import { DocsPage } from "./components/DocsPage.tsx"
import { AdminPanel } from "./components/admin/AdminPanel.tsx"
import "./style.css"

const getWebSocketUrl = () => {
  if (import.meta.env.VITE_WEBSOCKET_URL) {
    return import.meta.env.VITE_WEBSOCKET_URL
  }

  return `${window.location.protocol === "https:" ? "wss:" : "ws:"}//${window.location.host}`
}

const wsUrl = getWebSocketUrl()

interface InitializationProps {
  onComplete: (client: NostrClient) => void
}

const toConnectionMessage = (e: unknown): string => {
  if (!(e instanceof Error)) return "Failed to connect"
  if (e.message.includes("timeout")) {
    return "Connection timed out. Please check your network and try again."
  }
  if (e.message.includes("auth failed")) {
    return "Authentication failed. Please check your key and try again."
  }
  if (e.message.includes("Main relay")) {
    return `Cannot connect to the groups relay at ${wsUrl}. Please try again.`
  }
  return e.message
}

const hexToBytes = (hex: string): Uint8Array => {
  const matches = hex.match(/.{1,2}/g)
  if (!matches) return new Uint8Array()
  return new Uint8Array(matches.map((b) => parseInt(b, 16)))
}

const processPrivateKey = (key: string): string | null => {
  const cleanKey = key.trim()

  if (cleanKey.startsWith("nsec")) {
    try {
      const decoded = nip19.decode(cleanKey)
      if (decoded.type !== "nsec") return null
      return Array.from(decoded.data as Uint8Array)
        .map((b) => b.toString(16).padStart(2, "0"))
        .join("")
    } catch {
      return null
    }
  }

  if (!/^[0-9a-fA-F]{64}$/.test(cleanKey)) return null
  return cleanKey.toLowerCase()
}

const hasSdkSignerStorage = (): boolean => {
  return Boolean(
    localStorage.getItem(SIGNER_STORAGE_KEY_NIP46) ||
      localStorage.getItem(SIGNER_STORAGE_KEY_NSEC)
  )
}

const migrateLegacyKey = async (login: (signer: NostrSigner) => Promise<void>) => {
  const storedKey = localStorage.getItem("nostr_key")
  if (!storedKey || hasSdkSignerStorage()) return

  const hexKey = processPrivateKey(storedKey)
  if (!hexKey) {
    localStorage.removeItem("nostr_key")
    throw new Error("Stored private key is invalid. Please sign in again.")
  }

  const nsec = nip19.nsecEncode(hexToBytes(hexKey))
  localStorage.setItem(SIGNER_STORAGE_KEY_NSEC, nsec)
  localStorage.removeItem("nostr_key")
  await login(new PrivateKeySigner(hexKey))
}

const Initialization = ({ onComplete }: InitializationProps) => {
  const [error, setError] = useState<Error | null>(null)
  const [status, setStatus] = useState<"idle" | "connecting">("idle")
  const signer = useSigner() as NostrSigner | null
  const login = useLogin() as (signer: NostrSigner) => Promise<void>
  const completedRef = useRef(false)
  const connectingForRef = useRef<string | null>(null)
  const legacyMigrationStartedRef = useRef(false)

  const connectWithSigner = async (activeSigner: NostrSigner) => {
    const pubkey = await activeSigner.getPublicKey()
    if (completedRef.current || connectingForRef.current === pubkey) return

    connectingForRef.current = pubkey
    try {
      setStatus("connecting")
      const client = await NostrClient.fromNostrSigner(activeSigner, { relayUrl: wsUrl })
      await client.connect()
      localStorage.removeItem("nostr_key")
      completedRef.current = true
      onComplete(client)
    } catch (e) {
      connectingForRef.current = null
      const errorMessage = toConnectionMessage(e)
      setError(new Error(errorMessage))
      throw new Error(errorMessage)
    } finally {
      setStatus("idle")
    }
  }

  useEffect(() => {
    if (!signer) return
    void connectWithSigner(signer).catch(() => undefined)
  }, [signer])

  useEffect(() => {
    if (signer || legacyMigrationStartedRef.current) return
    legacyMigrationStartedRef.current = true

    void migrateLegacyKey(login).catch((e) => {
      setError(new Error(toConnectionMessage(e)))
    })
  }, [login, signer])

  if (error) {
    return (
      <ErrorState
        error={error}
        onRetry={() => {
          setError(null)
          if (signer) void connectWithSigner(signer).catch(() => undefined)
        }}
      />
    )
  }

  if (status === "connecting") {
    return (
      <LoadingState
        title="Connecting"
        message="Establishing connection to relay..."
      />
    )
  }

  return <AuthPrompt />
}

const ChatApp = (_props: { path?: string }) => {
  const [client, setClient] = useState<NostrClient | null>(null)
  const signer = useSigner() as NostrSigner | null
  const logout = useLogout()

  const handleLogout = () => {
    if (client) {
      client.disconnect()
    }
    setClient(null)
    localStorage.removeItem("nostr_key")
    void signer?.close?.()
    void clearPersistedNip46()
    void clearPersistedNsec()
    void logout()
  }

  if (!client) {
    return <Initialization onComplete={setClient} />
  }

  return <App client={client} onLogout={handleLogout} />
}

const Root = () => {
  return (
    <NostrSessionProvider theme="la-crypta" autoRestore>
      <Router>
        <LandingPage path="/" />
        <DocsPage path="/docs" />
        <ChatApp path="/app" />
        <AdminPanel path="/admin" />
      </Router>
    </NostrSessionProvider>
  )
}

render(<Root />, document.getElementById("app")!)
