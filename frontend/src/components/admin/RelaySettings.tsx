import { useEffect, useState } from 'preact/hooks'
import {
  adminApi,
  type AdminPubkeyEntry,
  type BackupEntry,
  type ConfigResetResult,
  type ObeliskIndexSettings,
  type ConnectionSettings,
  type RelayIdentity,
  type UpdateStatus,
} from '../../services/AdminApiClient'
import { confirmMatches } from './confirmPhrase'
import { AdminEmptyState } from './AdminEmptyState'
import { fetchProfiles, getDisplayName, type NostrProfile } from '../../services/ProfileFetcher'
import { ProfileCard, CopyNpubButton } from './ProfileCard'
import { SettingsIcon } from './icons'
import { useDirtySection } from './settingsDirty'

type SettingsSection = 'whitelist' | 'storage' | 'groups'

interface RelaySettingsProps {
  onResetToSetup: (result: ConfigResetResult) => void
  onNavigate?: (section: SettingsSection) => void
}

const shortKey = (value: string) => (
  value.length > 20 ? `${value.slice(0, 10)}...${value.slice(-10)}` : value
)

const formatBytes = (bytes: number) => {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  return `${(bytes / Math.pow(1024, index)).toFixed(index === 0 ? 0 : 1)} ${units[index]}`
}

const formatUnix = (unix: number) => {
  if (!unix) return 'Unknown'
  return new Date(unix * 1000).toLocaleString()
}

/**
 * Where the update runbook lives.
 *
 * The console is a browser page and the relay serves only its own bundle, so a
 * bare `docs/updating.md` in an error string pointed at nothing an operator
 * could open. The repo is public, so link it there.
 */
const DOC_UPDATING_URL =
  'https://github.com/obelisk-app/obelisk-relay/blob/main/docs/updating.md'

/**
 * The date out of a `v2026.09.18-something` tag, as a sortable string.
 *
 * Release tags here are dated, which is the only ordering available — the
 * registry returns tags lexically, and `latest`, `main` and bare SHAs carry no
 * order at all. Returns null for those rather than guessing: an unknown
 * ordering must not be presented as a known one.
 */
const releaseDate = (tag: string): string | null => {
  const m = /^v(\d{4})\.(\d{2})\.(\d{2})/.exec(tag)
  return m ? `${m[1]}${m[2]}${m[3]}` : null
}

const relativeAge = (unix: number) => {
  if (!unix) return 'never'
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - unix)
  if (secs < 60) return 'just now'
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`
  if (secs < 86400) return `${Math.floor(secs / 3600)}h ago`
  return `${Math.floor(secs / 86400)}d ago`
}

/**
 * Which version is running, and moving to another published one.
 *
 * The relay cannot update itself: compose does not re-resolve the image tag on a
 * restart, and the container has no Docker socket — on purpose, since that is
 * root on the host handed to a process serving the open internet. So this queues
 * a request that a host-side agent carries out. If that agent is not installed,
 * the card says so and the button stays disabled: a request nothing reads is
 * worse than a refusal, because it looks like it worked.
 */
const UpdateCard = () => {
  const [status, setStatus] = useState<UpdateStatus | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [tag, setTag] = useState('')
  const [confirm, setConfirm] = useState('')
  const [busy, setBusy] = useState(false)
  const [waiting, setWaiting] = useState(false)

  const load = async (refresh = false) => {
    setLoading(true)
    try {
      const fresh = await adminApi.getUpdateStatus(refresh)
      setStatus(fresh)
      // Preselect the newest published version, which is what an operator
      // opening this card almost always wants.
      setTag(current => current || fresh.available_tags[0] || '')
      setError(null)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Could not read update status')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { void load() }, [])

  /**
   * Watch for the relay to come back on the new version.
   *
   * Failures here are the expected middle of an update — the container is being
   * recreated — so they are not surfaced. Only running out of patience is.
   */
  const waitForUpdate = async (expected: string) => {
    const deadline = Date.now() + 5 * 60 * 1000
    while (Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 4000))
      try {
        const fresh = await adminApi.getUpdateStatus(false)
        setStatus(fresh)
        // Done when the tag has changed over, or when the agent has reported
        // back — a rollback is an outcome too, not a reason to keep waiting.
        const settled = fresh.running.image_tag === expected
          || (fresh.last_result !== null && fresh.pending === null && fresh.last_result.requested_tag === expected)
        if (settled) {
          setWaiting(false)
          return
        }
      } catch {
        // Relay is down mid-recreate. Keep waiting.
      }
    }
    setWaiting(false)
    setError('The relay did not report back within five minutes. Check the container logs.')
  }

  const applyUpdate = async () => {
    setBusy(true)
    setError(null)
    try {
      const result = await adminApi.updateRelay(tag, confirm)
      setConfirm('')
      setWaiting(true)
      void waitForUpdate(result.requested_tag)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Could not queue the update')
    } finally {
      setBusy(false)
    }
  }

  const running = status?.running
  const newest = status?.available_tags[0]
  const unpublished = Boolean(status && running?.image_tag && !status.running_is_published)
  const upToDate = Boolean(running?.image_tag && newest && running.image_tag === newest)
  const ready = confirmMatches(confirm, 'UPDATE') && tag.length > 0
  const last = status?.last_result
  // Warn before a downgrade rather than after. Only dated tags can be ordered;
  // anything else compares as unknown and gets no claim either way.
  const selectedIsOlder = Boolean(
    running?.image_tag && tag && releaseDate(tag) && releaseDate(running.image_tag)
      && releaseDate(tag)! < releaseDate(running.image_tag)!
  )

  return (
    <section class="admin-settings-card">
      <div class="admin-settings-card-header">
        <div>
          <h3>Relay Version</h3>
          <p>What this relay is running, and moving it to another published build.</p>
        </div>
        {running?.image_tag && (
          <span class={`admin-status-badge ${upToDate ? 'admin-status-badge-ok' : 'admin-status-badge-warn'}`}>
            {/* An unpublished build is not "out of date" — it is newer than
                anything the registry has. Saying otherwise invites an operator
                to "update" their way backwards. */}
            {unpublished ? 'Unpublished build' : upToDate ? 'Up to date' : 'Update available'}
          </span>
        )}
      </div>

      {loading && !status && <div class="lc-skeleton h-24 w-full" />}

      {running && (
        <div class="admin-storage-stats">
          {/* Not "latest" when unset: the container genuinely cannot discover
              its own tag, so an absent one is unknown rather than assumed. */}
          <div
            class="admin-stat-card"
            title={running.image_tag ? 'The image tag this container was started under.' : 'Add RELAY_IMAGE_TAG to the service environment in compose.yml.'}
          >
            <span>Running version</span>
            <strong style={{ fontSize: '16px', wordBreak: 'break-all' }}>
              {running.image_tag ?? 'Unknown'}
            </strong>
          </div>
          <div class="admin-stat-card" title={`Built ${running.build_time}`}>
            <span>Built from</span>
            <strong style={{ fontSize: '16px', wordBreak: 'break-all' }}>{running.git_sha}</strong>
          </div>
          <div class="admin-stat-card">
            <span>Newest published</span>
            <strong style={{ fontSize: '16px', wordBreak: 'break-all' }}>{newest ?? '—'}</strong>
          </div>
        </div>
      )}

      {status?.tags_error && (
        <p class="text-sm mt-2" style={{ color: '#fcd34d' }}>
          The published version list may be out of date: {status.tags_error}
        </p>
      )}

      {waiting ? (
        <div class="mt-3 p-3 rounded-lg text-sm bg-amber-500/10 text-amber-300 border border-amber-500/20">
          Updating. The relay is being recreated and will be unreachable for a
          moment — this page is waiting and will report the result.
        </div>
      ) : status?.pending ? (
        <div class="mt-3 p-3 rounded-lg text-sm bg-amber-500/10 text-amber-300 border border-amber-500/20">
          An update to {status.pending.requested_tag} is queued and waiting for
          the host agent.
        </div>
      ) : (
        <div class="admin-danger-card">
          <div>
            <h4>Update to another version</h4>
            {status?.blocked_reason
              ? <>
                  <p>{status.blocked_reason}</p>
                  {/* An operator told "install the agent" needs the command,
                      not a filename they have to go and locate. */}
                  {!status.agent_live && (
                    <p class="text-sm mt-1" style={{ color: 'var(--color-text-secondary)' }}>
                      <a
                        href={DOC_UPDATING_URL}
                        target="_blank"
                        rel="noopener noreferrer"
                        class="underline"
                      >
                        How to install the update agent
                      </a>
                    </p>
                  )}
                </>
              : unpublished
                ? <p>
                    This relay runs <strong>{running?.image_tag}</strong>, which is not in
                    the registry — a build made on the host. Nothing published is newer
                    than it, so updating would <strong>replace it with an older
                    version</strong> and lose whatever that build carries. Push it to the
                    registry instead, unless going back is what you want.
                  </p>
                : <p>Pulls the selected image and recreates the container. If it does not come up healthy, the previous version is restored automatically.</p>}
            {selectedIsOlder && !status?.blocked_reason && (
              <p style={{ color: '#fcd34d' }}>
                {tag} is older than {running?.image_tag}. This is a downgrade.
              </p>
            )}
          </div>
          <div class="admin-danger-controls">
            <select
              value={tag}
              onChange={e => setTag((e.target as HTMLSelectElement).value)}
              disabled={!status?.can_update || busy}
              aria-label="Version to update to"
            >
              {(status?.available_tags ?? []).map(t => {
                const rd = releaseDate(t)
                const cur = running?.image_tag ? releaseDate(running.image_tag) : null
                const older = Boolean(rd && cur && rd < cur)
                return (
                  <option key={t} value={t}>
                    {t}
                    {t === running?.image_tag ? ' (running)' : older ? ' (older)' : ''}
                  </option>
                )
              })}
            </select>
            <input
              type="text"
              value={confirm}
              onInput={e => setConfirm((e.target as HTMLInputElement).value)}
              placeholder="Type UPDATE"
              aria-label="Confirm update by typing UPDATE"
              disabled={!status?.can_update || busy}
            />
            <button
              type="button"
              onClick={applyUpdate}
              disabled={!status?.can_update || !ready || busy}
              class="admin-danger-button"
            >
              {busy ? 'Queueing...' : 'Update relay'}
            </button>
          </div>
        </div>
      )}

      {last && (
        <p class="text-sm mt-2" style={{ color: 'var(--color-text-secondary)' }}>
          Last update {relativeAge(last.finished_at)}:{' '}
          {last.status === 'ok'
            ? `moved to ${last.requested_tag} from ${last.previous_tag ?? 'an unrecorded version'}.`
            : `${last.status} — ${last.detail ?? 'no detail recorded'}`}
        </p>
      )}

      <p class="text-sm mt-1" style={{ color: 'var(--color-text-secondary)' }}>
        {status?.agent_live
          ? `Update agent last seen ${relativeAge(status.agent_last_seen ?? 0)}.`
          : 'No update agent is running on this host.'}
        {' '}
        <button
          type="button"
          onClick={() => load(true)}
          disabled={loading || waiting}
          class="underline"
          style={{ color: 'var(--color-text-secondary)' }}
        >
          {loading ? 'Checking...' : 'Check for updates'}
        </button>
      </p>

      {error && (
        <div class="mt-3 p-3 rounded-lg text-sm bg-red-500/10 text-red-400 border border-red-500/20">
          {error}
        </div>
      )}
    </section>
  )
}

export const RelaySettings = ({ onResetToSetup, onNavigate }: RelaySettingsProps) => {
  const [identity, setIdentity] = useState<RelayIdentity | null>(null)
  const [obeliskIndex, setObeliskIndex] = useState<ObeliskIndexSettings | null>(null)
  const [limits, setLimits] = useState<ConnectionSettings | null>(null)
  const [identityForm, setIdentityForm] = useState({
    relay_name: '',
    relay_description: '',
    relay_url: '',
    relay_icon: '',
  })
  const [iconError, setIconError] = useState<string | null>(null)
  const [admins, setAdmins] = useState<AdminPubkeyEntry[]>([])
  const [newAdminPubkey, setNewAdminPubkey] = useState('')
  const [backups, setBackups] = useState<BackupEntry[]>([])
  const [confirmRemoveAdmin, setConfirmRemoveAdmin] = useState<string | null>(null)
  const [restoreTarget, setRestoreTarget] = useState<string | null>(null)
  const [restoreConfirm, setRestoreConfirm] = useState('')
  const [restartConfirm, setRestartConfirm] = useState('')
  const [rotateConfirm, setRotateConfirm] = useState('')
  const [resetConfirm, setResetConfirm] = useState('')
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [toast, setToast] = useState<string | null>(null)
  // Last-saved copies, so the save bar can tell an edit from a reload.
  const [obeliskBaseline, setObeliskBaseline] = useState<ObeliskIndexSettings | null>(null)
  const [limitsBaseline, setLimitsBaseline] = useState<ConnectionSettings | null>(null)
  // Typed confirmation for force_public_groups; see the card below.
  const [forcePublicConfirm, setForcePublicConfirm] = useState('')
  // Faces for the admin list. An npub identifies a key but not a person, and
  // "who has the keys to this relay" is exactly the question worth answering.
  const [adminProfiles, setAdminProfiles] = useState<Map<string, NostrProfile>>(new Map())
  const [profileTarget, setProfileTarget] = useState<{ hex: string; npub: string } | null>(null)

  const showToast = (message: string) => {
    setToast(message)
    setTimeout(() => setToast(null), 5000)
  }

  const loadSettings = async () => {
    setLoading(true)
    setError(null)
    try {
      const [identityData, adminData, backupData, obeliskIndexData, limitsData] = await Promise.all([
        adminApi.getRelayIdentity(),
        adminApi.getAdminPubkeys(),
        adminApi.getConfigBackups(),
        adminApi.getObeliskIndexSettings(),
        adminApi.getConnectionSettings(),
      ])
      setIdentity(identityData)
      setIdentityForm({
        relay_name: identityData.relay_name,
        relay_description: identityData.relay_description,
        relay_url: identityData.relay_url,
        relay_icon: identityData.relay_icon ?? '',
      })
      setAdmins(adminData)
      if (adminData.length > 0) {
        fetchProfiles(adminData.map(a => a.hex))
          .then(setAdminProfiles)
          // Decoration; a slow profile relay must not blank the settings page.
          .catch(() => undefined)
      }
      setBackups(backupData)
      setObeliskIndex(obeliskIndexData)
      setObeliskBaseline(obeliskIndexData)
      setLimits(limitsData)
      setLimitsBaseline(limitsData)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load relay settings')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void loadSettings()
  }, [])

  const saveIdentity = async () => {
    setBusy('identity')
    setError(null)
    try {
      const response = await adminApi.updateRelayIdentity(identityForm)
      setIdentity(response)
      showToast('Relay identity saved. Restart the relay to apply it.')
    } catch (e) {
      // Rethrown so the save bar can name this section in its failure list.
      setError(e instanceof Error ? e.message : 'Failed to save relay identity')
      throw e
    } finally {
      setBusy(null)
    }
  }

  const rotateRelayKey = async () => {
    setBusy('rotate-key')
    setError(null)
    try {
      const response = await adminApi.rotateRelayKey('ROTATE')
      setRotateConfirm('')
      setIdentity(prev => prev ? { ...prev, relay_pubkey: response.relay_pubkey, restart_required: true } : prev)
      showToast(response.message)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to rotate relay key')
    } finally {
      setBusy(null)
    }
  }

  const updateLimits = (patch: Partial<ConnectionSettings>) => {
    setLimits(prev => prev ? { ...prev, ...patch } : prev)
  }

  const saveLimits = async () => {
    if (!limits) return
    setBusy('limits')
    setError(null)
    try {
      const response = await adminApi.updateConnectionSettings({
        max_connections: limits.max_connections,
        max_connections_per_ip: limits.max_connections_per_ip,
        max_connection_duration_minutes: limits.max_connection_duration_minutes,
        idle_timeout_minutes: limits.idle_timeout_minutes,
        max_subscriptions: limits.max_subscriptions,
        max_limit: limits.max_limit,
        force_public_groups: limits.force_public_groups,
        force_public_confirm: forcePublicConfirm,
      })
      setLimits(response)
      setLimitsBaseline(response)
      setForcePublicConfirm('')
      showToast('Connection limits saved. Restart relay to apply them.')
    } catch (e) {
      // Rethrown so the save bar can name this section in its failure list.
      setError(e instanceof Error ? e.message : 'Failed to save connection limits')
      throw e
    } finally {
      setBusy(null)
    }
  }

  // Compare only the fields that are actually sent. The response echoes derived
  // values (running_*, active_connections, restart_required) that change on
  // their own, and including them would report the section as permanently dirty.
  const limitsDirty = Boolean(
    limits && limitsBaseline && (
      limits.max_connections !== limitsBaseline.max_connections ||
      limits.max_connections_per_ip !== limitsBaseline.max_connections_per_ip ||
      limits.max_connection_duration_minutes !== limitsBaseline.max_connection_duration_minutes ||
      limits.idle_timeout_minutes !== limitsBaseline.idle_timeout_minutes ||
      limits.max_subscriptions !== limitsBaseline.max_subscriptions ||
      limits.max_limit !== limitsBaseline.max_limit ||
      limits.force_public_groups !== limitsBaseline.force_public_groups
    ),
  )

  // Only switching the flag *on* destroys information, so only that direction
  // is gated. Turning it back off is safe and needs no phrase.
  const turningForcePublicOn = Boolean(
    limits && limitsBaseline && limits.force_public_groups && !limitsBaseline.force_public_groups,
  )

  // Caught here as well as server-side so the save bar can explain the block
  // instead of the user discovering it as a failed request.
  const limitsBlocked = limits && limits.max_connections_per_ip > limits.max_connections
    ? 'The per-IP limit cannot exceed the total connection limit.'
    : turningForcePublicOn && !confirmMatches(forcePublicConfirm, 'FORCE PUBLIC')
      ? 'Type FORCE PUBLIC to confirm making every existing group public.'
      : undefined

  useDirtySection(
    {
      id: 'limits',
      label: 'Connection limits',
      dirty: limitsDirty,
      blocked: limitsBlocked,
      consequence: turningForcePublicOn
        ? 'On restart this clears private and hidden on every existing group. It cannot be undone.'
        : limitsDirty
          ? 'Applies after the relay restarts.'
          : undefined,
    },
    {
      save: saveLimits,
      discard: () => setLimits(limitsBaseline),
    },
  )

  const updateObeliskIndex = (patch: Partial<ObeliskIndexSettings>) => {
    setObeliskIndex(prev => prev ? { ...prev, ...patch } : prev)
  }

  const saveObeliskIndex = async () => {
    if (!obeliskIndex) return
    setBusy('obelisk-index')
    setError(null)
    try {
      const response = await adminApi.updateObeliskIndexSettings({
        enabled: obeliskIndex.enabled,
        recent_per_group: obeliskIndex.recent_per_group,
        max_bootstrap_groups: obeliskIndex.max_bootstrap_groups,
        max_page_limit: obeliskIndex.max_page_limit,
        bootstrap_requests_per_minute: obeliskIndex.bootstrap_requests_per_minute,
        message_requests_per_minute: obeliskIndex.message_requests_per_minute,
        reconcile_interval_minutes: obeliskIndex.reconcile_interval_minutes,
      })
      setObeliskIndex(response)
      setObeliskBaseline(response)
      showToast('Obelisk bootstrap settings saved. Restart relay to apply them.')
    } catch (e) {
      // Rethrown so the save bar can name this section in its failure list.
      setError(e instanceof Error ? e.message : 'Failed to save Obelisk bootstrap settings')
      throw e
    } finally {
      setBusy(null)
    }
  }

  // `identity` holds what the server last returned; `identityForm` is the draft.
  const identityDirty = Boolean(
    identity &&
    (identityForm.relay_name !== identity.relay_name ||
      identityForm.relay_description !== identity.relay_description ||
      identityForm.relay_url !== identity.relay_url ||
      identityForm.relay_icon !== (identity.relay_icon ?? '')),
  )

  useDirtySection(
    {
      id: 'identity',
      label: 'Relay identity',
      dirty: identityDirty,
      blocked: identityDirty && iconError ? iconError : null,
    },
    {
      save: saveIdentity,
      discard: () => {
        if (!identity) return
        setIdentityForm({
          relay_name: identity.relay_name,
          relay_description: identity.relay_description,
          relay_url: identity.relay_url,
          relay_icon: identity.relay_icon ?? '',
        })
        setIconError(null)
      },
    },
  )

  const obeliskDirty = Boolean(
    obeliskIndex && obeliskBaseline && JSON.stringify(obeliskIndex) !== JSON.stringify(obeliskBaseline),
  )

  useDirtySection(
    { id: 'obelisk-index', label: 'Bootstrap index', dirty: obeliskDirty },
    {
      save: saveObeliskIndex,
      discard: () => setObeliskIndex(obeliskBaseline),
    },
  )

  const addAdmin = async () => {
    if (!newAdminPubkey.trim()) return
    setBusy('add-admin')
    setError(null)
    try {
      const entry = await adminApi.addAdminPubkey(newAdminPubkey.trim())
      setAdmins(prev => [...prev.filter(item => item.hex !== entry.hex), entry])
      setNewAdminPubkey('')
      showToast('Admin pubkey added')
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to add admin')
    } finally {
      setBusy(null)
    }
  }

  const removeAdmin = async (hex: string) => {
    setBusy(`remove-admin-${hex}`)
    setError(null)
    try {
      await adminApi.removeAdminPubkey(hex)
      setAdmins(prev => prev.filter(item => item.hex !== hex))
      setConfirmRemoveAdmin(null)
      showToast('Admin pubkey removed')
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to remove admin')
    } finally {
      setBusy(null)
    }
  }

  const downloadBackup = async (backup: BackupEntry) => {
    setBusy(`download-${backup.id}`)
    setError(null)
    try {
      const payload = await adminApi.downloadConfigBackup(backup.id)
      const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' })
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = `${backup.id}.json`
      a.click()
      URL.revokeObjectURL(url)
      showToast('Backup downloaded')
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to download backup')
    } finally {
      setBusy(null)
    }
  }

  const restoreBackup = async (backup: BackupEntry) => {
    setBusy(`restore-${backup.id}`)
    setError(null)
    try {
      const response = await adminApi.restoreConfigBackup(backup.id, restoreConfirm)
      setRestoreConfirm('')
      setRestoreTarget(null)
      showToast(response.message)
      await loadSettings()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to restore backup')
    } finally {
      setBusy(null)
    }
  }

  const restartRelay = async () => {
    setBusy('restart')
    setError(null)
    try {
      const response = await adminApi.restartRelay(restartConfirm)
      setRestartConfirm('')
      showToast(`${response.message}. The admin page may disconnect briefly.`)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to restart relay')
      setBusy(null)
    }
  }

  const resetConfig = async () => {
    setBusy('reset')
    setError(null)

    try {
      const response = await adminApi.resetRelayConfig({ confirm: 'RESET' })
      setResetConfirm('')
      onResetToSetup(response)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Reset failed')
    } finally {
      setBusy(null)
    }
  }

  return (
    <div class="space-y-6">
      <div>
      </div>

      {toast && (
        <div class="p-3 rounded-lg text-sm border" style={{ background: 'rgba(var(--color-accent-rgb), 0.08)', color: 'var(--color-accent)', borderColor: 'rgba(var(--color-accent-rgb), 0.2)' }}>
          {toast}
        </div>
      )}

      {error && (
        <div class="p-3 rounded-lg text-sm bg-red-500/10 text-red-400 border border-red-500/20">
          {error}
        </div>
      )}

      <section class="admin-settings-card">
        <div class="admin-settings-card-header">
          <div>
            <h3>Where Settings Live</h3>
            <p>Use these sections for routine access, storage, and moderation changes.</p>
          </div>
        </div>

        <div class="admin-settings-link-grid mt-4">
          <button type="button" onClick={() => onNavigate?.('whitelist')} class="admin-mode-option">
            <span class="admin-mode-title">Access</span>
            <span class="admin-mode-copy">Open relay mode, whitelist enforcement, rate limits, allowed pubkeys, and blocked pubkeys.</span>
          </button>
          <button type="button" onClick={() => onNavigate?.('storage')} class="admin-mode-option">
            <span class="admin-mode-title">Storage</span>
            <span class="admin-mode-copy">Database size, pruning status, retention window, prune interval, and prune event kinds.</span>
          </button>
          <button type="button" onClick={() => onNavigate?.('groups')} class="admin-mode-option">
            <span class="admin-mode-title">Groups</span>
            <span class="admin-mode-copy">Group metadata, members, moderation actions, event browsing, and group deletion.</span>
          </button>
        </div>
      </section>

      {loading ? (
        <div class="space-y-3">
          <div class="lc-skeleton h-36 w-full" />
          <div class="lc-skeleton h-36 w-full" />
          <div class="lc-skeleton h-36 w-full" />
        </div>
      ) : (
        <>
          <section class="admin-settings-card">
            <div class="admin-settings-card-header">
              <div>
                <h3>Relay Identity</h3>
                <p>Controls NIP-11 name, description, advertised URL, and relay signing identity.</p>
              </div>
              {identity?.restart_required && <span class="admin-status-badge admin-status-badge-warn">Restart required</span>}
            </div>

            <div class="admin-rate-grid mt-4">
              <label>
                <span>Relay name</span>
                <input
                  type="text"
                  value={identityForm.relay_name}
                  onInput={e => setIdentityForm(prev => ({ ...prev, relay_name: (e.target as HTMLInputElement).value }))}
                />
              </label>
              <label>
                <span>Relay URL</span>
                <input
                  type="text"
                  value={identityForm.relay_url}
                  onInput={e => setIdentityForm(prev => ({ ...prev, relay_url: (e.target as HTMLInputElement).value }))}
                />
              </label>
              <label>
                <span>Active relay pubkey</span>
                <input type="text" value={identity?.relay_pubkey ?? ''} readonly />
              </label>
            </div>

            <label class="admin-textarea-field">
              <span>Relay description</span>
              <textarea
                value={identityForm.relay_description}
                onInput={e => setIdentityForm(prev => ({ ...prev, relay_description: (e.target as HTMLTextAreaElement).value }))}
              />
            </label>

            {/* Relay icon: advertised as NIP-11 `icon`, shown top-left in this
                console, and used as the browser favicon so operators running
                several instances can tell them apart at a glance. */}
            <div class="mt-4">
              <span class="block text-sm font-semibold mb-2">Relay icon</span>
              <div class="flex items-start gap-4">
                <div
                  class="flex-shrink-0 flex items-center justify-center overflow-hidden"
                  style={{
                    width: '56px',
                    height: '56px',
                    borderRadius: '10px',
                    border: '1px solid var(--color-border)',
                    background: 'var(--color-bg-primary)',
                  }}
                >
                  {identityForm.relay_icon
                    ? <img src={identityForm.relay_icon} alt="" class="w-full h-full object-cover" />
                    : <span class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>none</span>}
                </div>
                <div class="flex-1 min-w-0 space-y-2">
                  <input
                    type="text"
                    class="w-full"
                    placeholder="https://example.com/icon.png"
                    value={identityForm.relay_icon.startsWith('data:') ? '' : identityForm.relay_icon}
                    onInput={e => {
                      setIconError(null)
                      setIdentityForm(prev => ({ ...prev, relay_icon: (e.target as HTMLInputElement).value }))
                    }}
                  />
                  <div class="flex items-center gap-2 flex-wrap">
                    <input
                      type="file"
                      accept="image/png,image/jpeg,image/svg+xml,image/webp,image/x-icon"
                      class="text-xs"
                      onChange={e => {
                        const file = (e.target as HTMLInputElement).files?.[0]
                        if (!file) return
                        // Embedded as a data URI rather than uploaded: the relay
                        // is not an image host, and this keeps the icon inside
                        // the config the operator already backs up.
                        if (file.size > 180 * 1024) {
                          setIconError('Image is too large. Use one under 180 KB, or link to a URL.')
                          return
                        }
                        const reader = new FileReader()
                        reader.onload = () => {
                          setIconError(null)
                          setIdentityForm(prev => ({ ...prev, relay_icon: String(reader.result) }))
                        }
                        reader.onerror = () => setIconError('Could not read that file.')
                        reader.readAsDataURL(file)
                      }}
                    />
                    {identityForm.relay_icon && (
                      <button
                        type="button"
                        class="lc-pill text-xs"
                        style={{ borderRadius: '6px', padding: '4px 10px' }}
                        onClick={() => { setIconError(null); setIdentityForm(prev => ({ ...prev, relay_icon: '' })) }}
                      >
                        Remove
                      </button>
                    )}
                  </div>
                  <p class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>
                    Paste an image URL or upload a small image. Applies after restart.
                  </p>
                  {iconError && <p class="text-xs text-red-400">{iconError}</p>}
                </div>
              </div>
            </div>

            {/* Committing is the shared save bar's job. */}
            <div class="admin-settings-actions">
              <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                Identity changes affect NIP-11 metadata and auth URL validation after restart.
                {busy === 'identity' && ' Saving…'}
              </div>
            </div>

            <div class="admin-danger-card">
              <div>
                <h4>Rotate Relay Key</h4>
                <p>Generates a new relay secret key and stores it in config. Existing relay-signed state uses the old pubkey until you restart.</p>
              </div>
              <div class="admin-danger-controls">
                <input
                  type="text"
                  value={rotateConfirm}
                  onInput={e => setRotateConfirm((e.target as HTMLInputElement).value)}
                  placeholder="Type ROTATE"
                  aria-label="Confirm key rotation by typing ROTATE"
                />
                <button
                  type="button"
                  onClick={rotateRelayKey}
                  disabled={!confirmMatches(rotateConfirm, 'ROTATE') || busy === 'rotate-key'}
                  class="admin-danger-button"
                >
                  {busy === 'rotate-key' ? 'Rotating...' : 'Rotate key'}
                </button>
              </div>
            </div>
          </section>

          {limits && (
            <section class="admin-settings-card">
              <div class="admin-settings-card-header">
                <div>
                  <h3>Connection limits</h3>
                  <p>
                    How much of this relay one client, or everyone together, can occupy.
                    These are the only limits that apply before a client authenticates.
                  </p>
                </div>
                <div class="admin-row-actions">
                  <span class="admin-status-badge">
                    {limits.active_connections} connected
                  </span>
                  {limits.restart_required && (
                    <span class="admin-status-badge admin-status-badge-warn">Restart required</span>
                  )}
                </div>
              </div>

              <div class="admin-rate-grid mt-4">
                <label>
                  <span>Max connections</span>
                  <input
                    type="number"
                    min="1"
                    value={limits.max_connections}
                    onInput={e =>
                      updateLimits({
                        max_connections: Number((e.target as HTMLInputElement).value),
                      })
                    }
                  />
                  <small>Currently enforcing {limits.running_max_connections}.</small>
                </label>
                <label>
                  <span>Max per IP</span>
                  <input
                    type="number"
                    min="1"
                    value={limits.max_connections_per_ip}
                    onInput={e =>
                      updateLimits({
                        max_connections_per_ip: Number((e.target as HTMLInputElement).value),
                      })
                    }
                  />
                  <small>Currently enforcing {limits.running_max_connections_per_ip}.</small>
                </label>
                <label>
                  <span>Max connection age (min)</span>
                  <input
                    type="number"
                    min="1"
                    value={limits.max_connection_duration_minutes}
                    onInput={e =>
                      updateLimits({
                        max_connection_duration_minutes: Number((e.target as HTMLInputElement).value),
                      })
                    }
                  />
                  <small>A client is disconnected after this and must reconnect.</small>
                </label>
                <label>
                  <span>Idle timeout (min)</span>
                  <input
                    type="number"
                    min="1"
                    value={limits.idle_timeout_minutes}
                    onInput={e =>
                      updateLimits({
                        idle_timeout_minutes: Number((e.target as HTMLInputElement).value),
                      })
                    }
                  />
                  <small>Reading is not traffic: a client sitting in a channel looks idle.</small>
                </label>
                <label>
                  <span>Max subscriptions</span>
                  <input
                    type="number"
                    min="1"
                    value={limits.max_subscriptions}
                    onInput={e =>
                      updateLimits({
                        max_subscriptions: Number((e.target as HTMLInputElement).value),
                      })
                    }
                  />
                  <small>Concurrent REQs allowed on one connection.</small>
                </label>
                <label>
                  <span>Max events per query</span>
                  <input
                    type="number"
                    min="1"
                    value={limits.max_limit}
                    onInput={e =>
                      updateLimits({
                        max_limit: Number((e.target as HTMLInputElement).value),
                      })
                    }
                  />
                  <small>Ceiling on a filter&apos;s own limit.</small>
                </label>
              </div>

              <label class="admin-toggle-row mt-4">
                <input
                  type="checkbox"
                  checked={limits.force_public_groups}
                  onChange={e =>
                    updateLimits({
                      force_public_groups: (e.target as HTMLInputElement).checked,
                    })
                  }
                />
                <span>
                  <strong>Force all groups public</strong>
                  <small>
                    Currently {limits.running_force_public_groups ? 'on' : 'off'}. On restart this
                    clears <code>private</code> and <code>hidden</code> on every stored group —
                    including groups other people created. There is no undo.
                  </small>
                </span>
              </label>

              {turningForcePublicOn && (
                <label class="mt-2 block">
                  <span class="block text-sm mb-1">
                    Type <strong>FORCE PUBLIC</strong> to confirm
                  </span>
                  <input
                    type="text"
                    value={forcePublicConfirm}
                    onInput={e => setForcePublicConfirm((e.target as HTMLInputElement).value)}
                    placeholder="FORCE PUBLIC"
                    autocomplete="off"
                  />
                </label>
              )}

              {limitsBlocked && (
                <p class="admin-access-hint" role="alert">{limitsBlocked}</p>
              )}

              <p class="admin-access-hint">
                Saved values take effect when the relay restarts; until then the relay keeps
                enforcing the &quot;currently enforcing&quot; figures above. Setting the per-IP
                limit too low will disconnect people sharing an address, such as an office or a
                household.
              </p>
            </section>
          )}

          {obeliskIndex && (
            <section class="admin-settings-card">
              <div class="admin-settings-card-header">
                <div>
                  <h3>Obelisk Bootstrap</h3>
                  <p>Controls the optimized HTTP snapshot and message pagination used by compatible clients.</p>
                </div>
                <div class="admin-row-actions">
                  <span class={`admin-status-badge ${obeliskIndex.active_enabled ? 'admin-status-badge-ok' : ''}`}>
                    {obeliskIndex.active_enabled ? 'Active' : 'Inactive'}
                  </span>
                  {obeliskIndex.restart_required && (
                    <span class="admin-status-badge admin-status-badge-warn">Restart required</span>
                  )}
                </div>
              </div>

              <label class="admin-toggle-row">
                <input
                  type="checkbox"
                  checked={obeliskIndex.enabled}
                  onChange={e => updateObeliskIndex({ enabled: (e.target as HTMLInputElement).checked })}
                />
                <span>
                  <strong>Advertise indexed bootstrap after restart</strong>
                  <small>Compatible clients use one HTTP bootstrap read, then one live WebSocket subscription.</small>
                </span>
              </label>

              <div class="admin-rate-grid mt-4">
                <label>
                  <span>Recent / group</span>
                  <input
                    type="number"
                    min="1"
                    value={obeliskIndex.recent_per_group}
                    onInput={e => updateObeliskIndex({ recent_per_group: Number((e.target as HTMLInputElement).value) })}
                  />
                </label>
                <label>
                  <span>Max groups</span>
                  <input
                    type="number"
                    min="1"
                    value={obeliskIndex.max_bootstrap_groups}
                    onInput={e => updateObeliskIndex({ max_bootstrap_groups: Number((e.target as HTMLInputElement).value) })}
                  />
                </label>
                <label>
                  <span>Page limit</span>
                  <input
                    type="number"
                    min="1"
                    value={obeliskIndex.max_page_limit}
                    onInput={e => updateObeliskIndex({ max_page_limit: Number((e.target as HTMLInputElement).value) })}
                  />
                </label>
                <label>
                  <span>Bootstraps / min</span>
                  <input
                    type="number"
                    min="1"
                    value={obeliskIndex.bootstrap_requests_per_minute}
                    onInput={e => updateObeliskIndex({ bootstrap_requests_per_minute: Number((e.target as HTMLInputElement).value) })}
                  />
                </label>
                <label>
                  <span>Pages / min</span>
                  <input
                    type="number"
                    min="1"
                    value={obeliskIndex.message_requests_per_minute}
                    onInput={e => updateObeliskIndex({ message_requests_per_minute: Number((e.target as HTMLInputElement).value) })}
                  />
                </label>
                <label>
                  <span>Reconcile minutes</span>
                  <input
                    type="number"
                    min="1"
                    value={obeliskIndex.reconcile_interval_minutes}
                    onInput={e => updateObeliskIndex({ reconcile_interval_minutes: Number((e.target as HTMLInputElement).value) })}
                  />
                </label>
              </div>

              <div class="admin-settings-actions">
                <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                  Changes apply after relay restart because the index and HTTP quotas are initialized at startup.
                  {busy === 'obelisk-index' && ' Saving…'}
                </div>
              </div>
            </section>
          )}

          <section class="admin-settings-card">
            <div class="admin-settings-card-header">
              <div>
                <h3>Admin Pubkeys</h3>
                <p>Admins can sign into this panel and perform privileged relay operations.</p>
              </div>
              <span class="admin-status-badge">{admins.length}</span>
            </div>

            <div class="admin-access-input-row">
              <input
                type="text"
                value={newAdminPubkey}
                onInput={e => setNewAdminPubkey((e.target as HTMLInputElement).value)}
                onKeyDown={e => e.key === 'Enter' && addAdmin()}
                placeholder="npub1... or hex pubkey"
                class="admin-access-input"
              />
              <button
                type="button"
                onClick={addAdmin}
                disabled={!newAdminPubkey.trim() || busy === 'add-admin'}
                class="admin-access-button admin-access-button-allow"
              >
                {busy === 'add-admin' ? 'Adding...' : 'Add admin'}
              </button>
            </div>

            <div class="admin-list mt-4">
              {admins.map(admin => {
                const profile = adminProfiles.get(admin.hex)
                return (
                <div class="admin-list-row" key={admin.hex}>
                  {/* Clickable identity, as everywhere else an npub appears. */}
                  <button
                    type="button"
                    class="admin-author-identity"
                    onClick={() => setProfileTarget({ hex: admin.hex, npub: admin.npub })}
                    title="Show profile"
                  >
                    {profile?.picture ? (
                      <img
                        src={profile.picture}
                        alt=""
                        class="w-7 h-7 rounded-full object-cover flex-shrink-0"
                        style={{ border: '1px solid var(--color-border)' }}
                        onError={e => { (e.target as HTMLImageElement).style.display = 'none' }}
                      />
                    ) : (
                      <span
                        class="w-7 h-7 rounded-full flex-shrink-0 flex items-center justify-center text-[10px] font-bold"
                        style={{ background: 'rgba(var(--color-accent-rgb), 0.1)', color: 'var(--color-accent)' }}
                      >
                        {(profile?.name || admin.npub.slice(5, 7) || '??').slice(0, 2).toUpperCase()}
                      </span>
                    )}
                    <span class="min-w-0 text-left">
                      <span class="block text-sm truncate">
                        {profile ? getDisplayName(profile, admin.npub) : shortKey(admin.npub || admin.hex)}
                      </span>
                      <span class="block text-[11px] font-mono" style={{ color: 'var(--color-text-secondary)' }}>
                        {shortKey(admin.npub || admin.hex)}
                      </span>
                    </span>
                  </button>
                  <CopyNpubButton npub={admin.npub} />
                  {/* The person who hosts the relay, distinct from the admins
                      they granted access to. */}
                  {admin.owner && <span class="admin-status-badge">Owner</span>}
                  {admin.current_session && <span class="admin-status-badge">This session</span>}
                  <span class="flex-1" />
                  {confirmRemoveAdmin === admin.hex ? (
                    <div class="admin-row-actions">
                      <button type="button" onClick={() => removeAdmin(admin.hex)} class="admin-text-danger">Confirm</button>
                      <button type="button" onClick={() => setConfirmRemoveAdmin(null)}>Cancel</button>
                    </div>
                  ) : (
                    <button
                      type="button"
                      onClick={() => setConfirmRemoveAdmin(admin.hex)}
                      disabled={admin.current_session || admin.owner}
                      class="admin-text-danger"
                      title={admin.owner ? 'The relay owner cannot be removed' : undefined}
                    >
                      Remove
                    </button>
                  )}
                </div>
                )
              })}
            </div>
          </section>

          <section class="admin-settings-card">
            <div class="admin-settings-card-header">
              <div>
                <h3>Config Backups</h3>
                <p>Download or restore timestamped config backups created before reset or restore operations.</p>
              </div>
              {/* A "0" badge is noise: it draws the eye to a count that means
                  nothing happened. Show it only once there is something to count. */}
              {backups.length > 0 && (
                <span class="admin-status-badge">{backups.length}</span>
              )}
            </div>

            {backups.length === 0 ? (
              /* An empty state should say when the section will have something
                 in it, not just assert that it does not. "None found" reads as
                 a failure; this reads as "nothing has needed one yet". */
              <div class="mt-4">
                <AdminEmptyState icon={SettingsIcon} headline="Nothing here yet — and that is the expected state">
                  The relay snapshots this config automatically just before it
                  overwrites it, which happens when you reset settings or restore
                  a previous version. One will appear here the first time that
                  runs, and you can download or roll back to it from here.
                </AdminEmptyState>
              </div>
            ) : (
              <div class="admin-list mt-4">
                {backups.map(backup => (
                  <div class="admin-list-row admin-list-row-tall" key={backup.id}>
                    <div>
                      <strong>{backup.id}</strong>
                      <p>{formatUnix(backup.created_unix)} | {backup.file_count} files | {formatBytes(backup.size_bytes)}</p>
                      <p class="font-mono">{backup.path}</p>
                    </div>
                    <div class="admin-row-actions">
                      <button type="button" onClick={() => downloadBackup(backup)}>Download</button>
                      {restoreTarget === backup.id ? (
                        <>
                          <input
                            type="text"
                            value={restoreConfirm}
                            onInput={e => setRestoreConfirm((e.target as HTMLInputElement).value)}
                            placeholder="RESTORE"
                            class="admin-inline-input"
                          />
                          <button
                            type="button"
                            onClick={() => restoreBackup(backup)}
                            disabled={restoreConfirm !== 'RESTORE'}
                            class="admin-text-danger"
                          >
                            Restore
                          </button>
                          <button type="button" onClick={() => { setRestoreTarget(null); setRestoreConfirm('') }}>Cancel</button>
                        </>
                      ) : (
                        <button type="button" onClick={() => setRestoreTarget(backup.id)} class="admin-text-danger">
                          Restore
                        </button>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </section>

          <UpdateCard />

          <section class="admin-settings-card">
            <div class="admin-settings-card-header">
              <div>
                <h3>Restart Relay</h3>
                <p>Stops the relay process so Docker can restart it. Use after startup-only settings change.</p>
              </div>
              <span class="admin-status-badge admin-status-badge-warn">Disconnects clients</span>
            </div>

            <div class="admin-danger-card">
              <div>
                <h4>Confirm Restart</h4>
                <p>The admin panel may disconnect while the container exits and starts again.</p>
              </div>
              <div class="admin-danger-controls">
                <input
                  type="text"
                  value={restartConfirm}
                  onInput={e => setRestartConfirm((e.target as HTMLInputElement).value)}
                  placeholder="Type RESTART"
                  aria-label="Confirm restart by typing RESTART"
                />
                <button
                  type="button"
                  onClick={restartRelay}
                  disabled={restartConfirm !== 'RESTART' || busy === 'restart'}
                  class="admin-danger-button"
                >
                  {busy === 'restart' ? 'Restarting...' : 'Restart relay'}
                </button>
              </div>
            </div>
          </section>
        </>
      )}

      <section class="admin-settings-card">
        <div class="admin-settings-card-header">
          <div>
            <h3>Recovery Reset</h3>
            <p>Use this only when the relay access/admin configuration is broken or you want to rerun first setup.</p>
          </div>
          <span class="admin-status-badge admin-status-badge-warn">Destructive config change</span>
        </div>

        <div class="admin-reset-flow">
          <div class="admin-reset-step">
            <span>1</span>
            <strong>Back up current config</strong>
            <small>Creates a timestamped backup folder under the relay config directory.</small>
          </div>
          <div class="admin-reset-step">
            <span>2</span>
            <strong>Clear access runtime state</strong>
            <small>Clears sessions, runtime admins, manual whitelist, follow-derived whitelist, references, and blacklist.</small>
          </div>
          <div class="admin-reset-step">
            <span>3</span>
            <strong>Reopen first-run setup</strong>
            <small>Keeps the current owner pubkey as the only identity allowed to finish setup.</small>
          </div>
        </div>

        <div class="admin-reset-impact">
          <div>
            <div class="font-semibold">Kept</div>
            <p>Relay event history, group data, relay secret key, database contents, and current owner pubkey.</p>
          </div>
          <div>
            <div class="font-semibold">Cleared</div>
            <p>Admin sessions, runtime admins, whitelist entries, follow-derived entries, reference accounts, and blocked pubkeys.</p>
          </div>
        </div>

        <div class="admin-danger-card">
          <div>
            <h4>Confirm Recovery Reset</h4>
            <p>After reset, setup asks you to choose whitelist enforcement or open relay mode again.</p>
          </div>
          <div class="admin-danger-controls">
            <input
              type="text"
              value={resetConfirm}
              onInput={e => setResetConfirm((e.target as HTMLInputElement).value)}
              placeholder="Type RESET"
              aria-label="Confirm reset by typing RESET"
            />
            <button
              type="button"
              onClick={resetConfig}
              disabled={!confirmMatches(resetConfirm, 'RESET') || busy === 'reset'}
              class="admin-danger-button"
            >
              {busy === 'reset' ? 'Resetting...' : 'Reset and reopen setup'}
            </button>
          </div>
        </div>
      </section>

      {profileTarget && (
        <ProfileCard
          hex={profileTarget.hex}
          npub={profileTarget.npub}
          profile={adminProfiles.get(profileTarget.hex)}
          onClose={() => setProfileTarget(null)}
        />
      )}
    </div>
  )
}
