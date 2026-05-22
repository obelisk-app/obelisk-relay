import { FunctionComponent } from "preact"
import { LoginWidget, type LoginMethodId } from "@nostr-wot/ui"
import type { NostrSigner } from "@nostr-wot/signers"

export interface AuthLogin {
  signer: NostrSigner
  pubkey: string
  method: LoginMethodId
  nsec?: string
  bunkerUri?: string
  clientNsec?: string
}

interface AuthPromptProps {
  onSubmit?: (login: AuthLogin) => Promise<void> | void
}

export const AuthPrompt: FunctionComponent<AuthPromptProps> = ({ onSubmit }) => {
  return (
    <div class="min-h-screen flex items-center justify-center bg-[var(--color-bg-primary)] px-4">
      <div class="w-full max-w-md">
        <LoginWidget
          title="Sign in to Obelisk Relay"
          subtitle="Connect with a Nostr signer to enter the groups relay."
          methods={["nip07", "nip46", "import"]}
          flatLayout
          showRememberToggle
          nip46Mode="qr"
          nip46Relays={["wss://relay.nsec.app", "wss://relay.damus.io"]}
          nip46Metadata={{
            name: "Obelisk Relay",
            url: window.location.origin,
            description: "Obelisk groups relay login",
          }}
          onLogin={onSubmit}
        />
      </div>
    </div>
  )
}
