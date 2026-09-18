import { useEffect, useRef, useState } from 'preact/hooks'
import {
  LoginWidget,
  useLogout,
  useSigner,
} from '@nostr-wot/ui'
import type { NostrSigner } from '@nostr-wot/signers'
import { NIP46_RELAYS } from '../../constants'
import { adminApi, type SetupStatus } from '../../services/AdminApiClient'
import { Nip46SignerDeepLink } from './Nip46SignerDeepLink'
import { LOGIN_METHOD_ICONS } from './loginIcons'
import {
  clearStoredSigners,
  restoreNip46SignerWithoutConnectReplay,
  signAdminAuthEvent,
  withTimeout,
} from './adminSigner'

interface AdminSetupWizardProps {
  status: SetupStatus
  onCompleted: () => void
}

type Step = 'owner' | 'access' | 'launch'
type AccessPolicy = 'owner_only' | 'open'

const shortKey = (value: string) => (
  value.length > 20 ? `${value.slice(0, 10)}...${value.slice(-10)}` : value
)

const normalizePubkey = (value: string | null | undefined) => value?.toLowerCase() ?? null
const accessPolicyTitle = (policy: AccessPolicy) => (
  policy === 'owner_only' ? 'Whitelist required' : 'Open relay'
)

export const AdminSetupWizard = ({ status, onCompleted }: AdminSetupWizardProps) => {
  const signer = useSigner() as NostrSigner | null
  const logout = useLogout()
  const [step, setStep] = useState<Step>('owner')
  const [ownerPubkey, setOwnerPubkey] = useState<string | null>(null)
  const [accessPolicy, setAccessPolicy] = useState<AccessPolicy>('owner_only')
  const [addOwnerReference, setAddOwnerReference] = useState(true)
  const [loadingSigner, setLoadingSigner] = useState(false)
  const [launching, setLaunching] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const fallbackSigner = useRef<NostrSigner | null>(null)
  const setupOwnerPubkey = normalizePubkey(status.setup_owner_pubkey)
  const setupOwnerLabel = status.setup_owner_npub ?? status.setup_owner_pubkey

  const ownerMatchesSetup = (pubkey: string) => (
    !setupOwnerPubkey || normalizePubkey(pubkey) === setupOwnerPubkey
  )

  const acceptOwnerPubkey = (pubkey: string) => {
    setOwnerPubkey(pubkey)
    if (!ownerMatchesSetup(pubkey)) {
      setError(
        `Setup is locked to ${setupOwnerLabel ? shortKey(setupOwnerLabel) : 'the retained owner'}. Use that owner signer to continue.`,
      )
      setStep('owner')
      return
    }
    setError(null)
    setStep('access')
  }

  useEffect(() => {
    if (!signer) {
      return
    }

    let cancelled = false
    setLoadingSigner(true)
    setError(null)

    void withTimeout(
      signer.getPublicKey(),
      'Signer did not respond. Use another signer or reconnect your Nostr app.',
    )
      .then(pubkey => {
        if (cancelled) return
        acceptOwnerPubkey(pubkey)
      })
      .catch(e => {
        if (cancelled) return
        setOwnerPubkey(null)
        setError(e instanceof Error ? e.message : 'Signer is unavailable')
      })
      .finally(() => {
        if (!cancelled) setLoadingSigner(false)
      })

    return () => {
      cancelled = true
    }
  }, [signer])

  useEffect(() => {
    if (signer || fallbackSigner.current) return

    let cancelled = false
    setLoadingSigner(true)
    void restoreNip46SignerWithoutConnectReplay()
      .then(async restored => {
        if (cancelled || !restored) return
        fallbackSigner.current = restored
        const pubkey = await withTimeout(
          restored.getPublicKey(),
          'Signer did not respond. Use another signer or reconnect your Nostr app.',
        )
        if (cancelled) return
        acceptOwnerPubkey(pubkey)
      })
      .catch(() => undefined)
      .finally(() => {
        if (!cancelled) setLoadingSigner(false)
      })

    return () => {
      cancelled = true
    }
  }, [signer])

  const handleWidgetLogin = async ({ signer: sdkSigner, pubkey }: { signer: NostrSigner; pubkey: string }) => {
    fallbackSigner.current = sdkSigner
    acceptOwnerPubkey(pubkey)
  }

  const switchIdentity = async () => {
    setOwnerPubkey(null)
    setError(null)
    setStep('owner')
    await clearStoredSigners(fallbackSigner.current ?? signer)
    fallbackSigner.current = null
    void logout()
  }

  const launchRelay = async () => {
    const activeSigner = signer ?? fallbackSigner.current
    if (!activeSigner || !ownerPubkey) return
    if (!ownerMatchesSetup(ownerPubkey)) {
      setError(
        `Setup is locked to ${setupOwnerLabel ? shortKey(setupOwnerLabel) : 'the retained owner'}. Use that owner signer to continue.`,
      )
      setStep('owner')
      return
    }

    setLaunching(true)
    setError(null)

    try {
      const { challenge } = await adminApi.getChallenge()
      const signedEvent = await withTimeout(
        signAdminAuthEvent(activeSigner, challenge),
        'Signer did not respond. Confirm the request or reconnect your signer.',
      )

      await adminApi.bootstrapSetup({
        signed_event: signedEvent,
        access_policy: accessPolicy,
        add_owner_reference: addOwnerReference,
      })
      onCompleted()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Setup failed')
    } finally {
      setLaunching(false)
    }
  }

  const steps: Array<{ id: Step; label: string; meta: string }> = [
    { id: 'owner', label: 'Owner', meta: ownerPubkey ? shortKey(ownerPubkey) : 'Connect signer' },
    { id: 'access', label: 'Access', meta: accessPolicyTitle(accessPolicy) },
    { id: 'launch', label: 'Launch', meta: status.relay_url || window.location.origin },
  ]

  const stepIndex = steps.findIndex(item => item.id === step)

  const canOpenAccess = Boolean(ownerPubkey && ownerMatchesSetup(ownerPubkey))
  const canOpenLaunch = Boolean(ownerPubkey && ownerMatchesSetup(ownerPubkey))

  // `admin-shell` is a styling scope, not a layout -- it is what gives the
  // console its form controls. The wizard renders before the shell exists, so
  // it opts in here rather than shipping browser-default inputs on the one
  // screen a new operator sees first.
  return (
    <div class="admin-shell min-h-screen lc-grid-bg flex items-center justify-center px-4 py-8" style={{ backgroundColor: 'var(--color-bg-primary)' }}>
      <Nip46SignerDeepLink />
      <div class="w-full max-w-5xl overflow-hidden" style={{ background: '#121212', border: '1px solid var(--color-border)', borderRadius: '8px', boxShadow: '0 24px 80px rgba(0,0,0,0.45)' }}>
        <div class="grid md:grid-cols-[280px_1fr]">
          <aside class="p-5 md:p-6" style={{ background: 'rgba(255,255,255,0.025)', borderRight: '1px solid var(--color-border)' }}>
            <a href="/" class="block text-lg font-bold" style={{ color: 'var(--color-accent)' }}>Obelisk Relay</a>
            <div class="mt-1 text-sm" style={{ color: 'var(--color-text-secondary)' }}>First-run setup</div>

            <div class="mt-8 space-y-2">
              {steps.map((item, index) => {
                const active = item.id === step
                const available = item.id === 'owner' || (item.id === 'access' && canOpenAccess) || (item.id === 'launch' && canOpenLaunch)
                return (
                  <button
                    key={item.id}
                    onClick={() => available && setStep(item.id)}
                    disabled={!available}
                    class="w-full text-left p-3 transition-colors"
                    style={{
                      borderRadius: '8px',
                      background: active ? 'rgba(var(--color-accent-rgb), 0.10)' : 'transparent',
                      border: active ? '1px solid rgba(var(--color-accent-rgb), 0.30)' : '1px solid transparent',
                      opacity: available ? 1 : 0.45,
                    }}
                  >
                    <div class="flex items-center gap-3">
                      <span class="inline-flex items-center justify-center text-xs font-bold" style={{
                        width: '26px',
                        height: '26px',
                        borderRadius: '50%',
                        background: active || index < stepIndex ? 'var(--color-accent)' : 'var(--color-bg-tertiary)',
                        color: active || index < stepIndex ? '#0a0a0a' : 'var(--color-text-secondary)',
                      }}>
                        {index + 1}
                      </span>
                      <span>
                        <span class="block text-sm font-semibold" style={{ color: active ? 'var(--color-accent)' : 'var(--color-text-primary)' }}>{item.label}</span>
                        <span class="block text-xs truncate" style={{ color: 'var(--color-text-secondary)', maxWidth: '190px' }}>{item.meta}</span>
                      </span>
                    </div>
                  </button>
                )
              })}
            </div>

            <div class="mt-8 pt-5 text-xs space-y-2" style={{ color: 'var(--color-text-secondary)', borderTop: '1px solid var(--color-border)' }}>
              <div class="flex justify-between gap-3">
                <span>Admins</span>
                <span>{status.admin_count}</span>
              </div>
              <div class="flex justify-between gap-3">
                <span>Whitelist</span>
                <span>{status.whitelisted_count}</span>
              </div>
              <div class="flex justify-between gap-3">
                <span>References</span>
                <span>{status.reference_account_count}</span>
              </div>
            </div>
          </aside>

          <main class="p-5 md:p-8">
            <div class="mb-7">
              <div class="text-xs uppercase" style={{ color: 'var(--color-text-secondary)' }}>
                {setupOwnerPubkey ? 'Relay setup reset' : 'Relay owner setup'}
              </div>
              <h1 class="mt-2 text-2xl md:text-3xl font-bold" style={{ color: 'var(--color-text-primary)' }}>
                {setupOwnerPubkey ? 'Reconnect the owner' : 'Claim this relay'}
              </h1>
            </div>

            {error && (
              <div class="mb-5 p-3 text-sm bg-red-500/10 text-red-400 border border-red-500/20" style={{ borderRadius: '8px' }}>
                {error}
              </div>
            )}

            {step === 'owner' && (
              <section>
                {setupOwnerLabel && (
                  <div class="mb-5 p-4 text-sm" style={{ background: 'rgba(var(--color-accent-rgb), 0.08)', border: '1px solid rgba(var(--color-accent-rgb), 0.22)', borderRadius: '8px' }}>
                    This reset kept the owner pubkey. Only <span class="font-mono">{shortKey(setupOwnerLabel)}</span> can finish setup.
                  </div>
                )}
                {ownerPubkey ? (
                  <div class="space-y-5">
                    <div class="p-4" style={{ background: 'var(--color-bg-secondary)', border: '1px solid var(--color-border)', borderRadius: '8px' }}>
                      <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>Owner pubkey</div>
                      <div class="mt-2 font-mono text-sm break-all">{ownerPubkey}</div>
                    </div>
                    <div class="flex flex-wrap gap-3">
                      <button
                        onClick={() => canOpenAccess && setStep('access')}
                        disabled={!canOpenAccess}
                        class="lc-pill-primary"
                        style={{ borderRadius: '8px' }}
                      >
                        Continue
                      </button>
                      <button onClick={switchIdentity} class="lc-pill-secondary" style={{ borderRadius: '8px' }}>
                        Use another signer
                      </button>
                    </div>
                  </div>
                ) : (
                  <div class="max-w-xl">
                    {loadingSigner && (
                      <div class="mb-4 flex items-center gap-3 text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                        <span class="lc-spinner" />
                        Loading signer...
                      </div>
                    )}
                    <div class="obelisk-login">
                    <LoginWidget
                      title={setupOwnerPubkey ? 'Connect retained owner' : 'Connect relay owner'}
                      subtitle={setupOwnerPubkey ? 'This reset can only be completed by the retained owner.' : 'This identity becomes the first admin.'}
                      methods={['nip07', 'nip46', 'import']}
                      methodIcons={LOGIN_METHOD_ICONS}
                      flatLayout
                      showRememberToggle
                      nip46Mode="qr"
                      nip46Relays={[...NIP46_RELAYS]}
                      nip46Metadata={{
                        name: 'Obelisk Relay Setup',
                        url: window.location.origin,
                        description: 'First-run setup for Obelisk relay',
                      }}
                      onLogin={handleWidgetLogin}
                    />
                    </div>
                  </div>
                )}
              </section>
            )}

            {step === 'access' && (
              <section class="space-y-5">
                <div class="grid md:grid-cols-2 gap-3">
                  {/* "Whitelist required" vs "Open relay" described the
                      mechanism, not the consequence, and gave both the same
                      neutral styling — so the one that lets the entire internet
                      write to your disk looked like an equal choice. */}
                  <button
                    onClick={() => setAccessPolicy('owner_only')}
                    class="text-left p-4 transition-colors"
                    style={{
                      borderRadius: '8px',
                      background: accessPolicy === 'owner_only' ? 'rgba(var(--color-accent-rgb), 0.10)' : 'var(--color-bg-secondary)',
                      border: accessPolicy === 'owner_only' ? '1px solid rgba(var(--color-accent-rgb), 0.35)' : '1px solid var(--color-border)',
                    }}
                  >
                    <div class="font-semibold" style={{ color: accessPolicy === 'owner_only' ? 'var(--color-accent)' : 'var(--color-text-primary)' }}>
                      Restricted · recommended
                    </div>
                    <div class="mt-2 text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                      Only people you allow can connect: you, anyone you add by
                      hand, everyone your account follows, and — if you turn it
                      on later — anyone close to you in the follow graph.
                    </div>
                  </button>
                  <button
                    onClick={() => setAccessPolicy('open')}
                    class="text-left p-4 transition-colors"
                    style={{
                      borderRadius: '8px',
                      background: accessPolicy === 'open' ? 'rgba(239, 68, 68, 0.12)' : 'var(--color-bg-secondary)',
                      border: accessPolicy === 'open' ? '1px solid rgba(239, 68, 68, 0.45)' : '1px solid var(--color-border)',
                    }}
                  >
                    <div class="font-semibold" style={{ color: accessPolicy === 'open' ? '#fca5a5' : 'var(--color-text-primary)' }}>
                      Open to everyone · dangerous
                    </div>
                    <div class="mt-2 text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                      Any pubkey on the internet can publish here. Storage grows
                      until your disk is full, and spam has nothing to stop it.
                      Only sensible for a deliberately public relay you are
                      prepared to babysit.
                    </div>
                  </button>
                </div>

                {accessPolicy === 'open' && (
                  <div class="p-4 text-sm" style={{ borderRadius: '8px', background: 'rgba(239, 68, 68, 0.08)', border: '1px solid rgba(239, 68, 68, 0.3)', color: '#fca5a5' }}>
                    Rate limits are your only protection in this mode, and they
                    slow abuse rather than prevent it. You can switch to
                    Restricted at any time from the Access screen.
                  </div>
                )}

                <label class="flex items-start gap-3 p-4 cursor-pointer" style={{ background: 'var(--color-bg-secondary)', border: '1px solid var(--color-border)', borderRadius: '8px' }}>
                  <input
                    type="checkbox"
                    checked={addOwnerReference}
                    onChange={e => setAddOwnerReference((e.target as HTMLInputElement).checked)}
                    class="mt-1"
                  />
                  <span>
                    {/* This is the difference between a relay that works when
                        you first open it and one that admits nobody but you, so
                        it says what it will actually do. */}
                    <span class="block text-sm font-semibold">
                      Whitelist everyone you follow
                    </span>
                    <span class="block text-sm mt-1" style={{ color: 'var(--color-text-secondary)' }}>
                      As soon as setup finishes, this relay reads your contact
                      list and allows every account in it. Without this, only
                      your own pubkey can connect until you add people by hand.
                      Your follows are re-synced whenever you ask, and your
                      account becomes the starting point for Web-of-Trust
                      admission if you enable it.
                    </span>
                  </span>
                </label>

                <div class="flex flex-wrap gap-3">
                  <button onClick={() => setStep('launch')} class="lc-pill-primary" style={{ borderRadius: '8px' }}>
                    Review
                  </button>
                  <button onClick={() => setStep('owner')} class="lc-pill-secondary" style={{ borderRadius: '8px' }}>
                    Back
                  </button>
                </div>
              </section>
            )}

            {step === 'launch' && (
              <section class="space-y-5">
                <div class="grid sm:grid-cols-2 gap-3">
                  <div class="p-4" style={{ background: 'var(--color-bg-secondary)', border: '1px solid var(--color-border)', borderRadius: '8px' }}>
                    <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>Owner</div>
                    <div class="mt-2 font-mono text-sm break-all">{ownerPubkey ? shortKey(ownerPubkey) : 'Missing signer'}</div>
                  </div>
                  <div class="p-4" style={{ background: 'var(--color-bg-secondary)', border: '1px solid var(--color-border)', borderRadius: '8px' }}>
                    <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>Access</div>
                    <div class="mt-2 font-semibold">{accessPolicyTitle(accessPolicy)}</div>
                  </div>
                  <div class="p-4" style={{ background: 'var(--color-bg-secondary)', border: '1px solid var(--color-border)', borderRadius: '8px' }}>
                    <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>Reference account</div>
                    <div class="mt-2 font-semibold">{addOwnerReference ? 'Owner' : 'None'}</div>
                  </div>
                  <div class="p-4" style={{ background: 'var(--color-bg-secondary)', border: '1px solid var(--color-border)', borderRadius: '8px' }}>
                    <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>Relay URL</div>
                    <div class="mt-2 font-mono text-sm break-all">{status.relay_url || window.location.origin}</div>
                  </div>
                </div>

                <div class="flex flex-wrap gap-3">
                  <button
                    onClick={launchRelay}
                    disabled={!ownerPubkey || launching}
                    class="lc-pill-primary"
                    style={{ borderRadius: '8px' }}
                  >
                    {launching ? 'Launching...' : 'Launch relay admin'}
                  </button>
                  <button onClick={() => setStep('access')} disabled={launching} class="lc-pill-secondary" style={{ borderRadius: '8px' }}>
                    Back
                  </button>
                </div>
              </section>
            )}
          </main>
        </div>
      </div>
    </div>
  )
}
