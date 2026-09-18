import { useState, useEffect } from 'preact/hooks'
import { adminApi, type PublicRelayInfo, type SetupStatus } from '../../services/AdminApiClient'
import { AdminAuth } from './AdminAuth'
import { AdminSetupWizard } from './AdminSetupWizard'
import { Dashboard } from './Dashboard'
import { WhitelistManager } from './WhitelistManager'
import { GroupsOverview } from './GroupsOverview'
import { ReferenceAccountsManager } from './ReferenceAccountsManager'
import { RelaySettings } from './RelaySettings'
import { StorageManager } from './StorageManager'
import { SearchIcon } from './SearchIcon'
import { AdminSaveBar } from './AdminSaveBar'
import { SettingsDirtyProvider } from './settingsDirty'
import {
  AccessIcon,
  GroupsIcon,
  OverviewIcon,
  ReferencesIcon,
  RelayIcon,
  SettingsIcon,
  StorageIcon,
} from './icons'

type Tab = 'dashboard' | 'whitelist' | 'reference-accounts' | 'groups' | 'storage' | 'settings'

type IconComponent = (props: { class?: string }) => preact.JSX.Element

interface NavItem {
  id: Tab
  label: string
  /** Terse line under the sidebar entry. */
  description: string
  /** Fuller sentence rendered beside the page title in the header. */
  blurb: string
  icon: IconComponent
}

/** Icon per destination, shared by the sidebar nav and the search results. */
const TAB_ICONS: Record<Tab, IconComponent> = {
  dashboard: OverviewIcon,
  whitelist: AccessIcon,
  'reference-accounts': ReferencesIcon,
  groups: GroupsIcon,
  storage: StorageIcon,
  settings: SettingsIcon,
}

interface SearchTarget {
  id: Tab
  title: string
  description: string
  keywords: string[]
}

// `description` is the terse sidebar line; `blurb` is the fuller sentence shown
// in the page header. Both live here so a page is described in exactly one
// place -- every screen used to repeat its own title and description in the
// body, directly under the identical title the header had already rendered.
const tabs: NavItem[] = [
  { id: 'dashboard', label: 'Overview', description: 'Relay health', icon: OverviewIcon,
    blurb: 'Live health, what is configured, and anything that needs attention.' },
  { id: 'whitelist', label: 'Access', description: 'Allowlist and blocks', icon: AccessIcon,
    blurb: 'Every rule that decides who can connect, and who each one lets in.' },
  { id: 'reference-accounts', label: 'References', description: 'Follow sync sources', icon: ReferencesIcon,
    blurb: 'The accounts that seed follow sync and root the Web-of-Trust graph.' },
  { id: 'groups', label: 'Groups', description: 'Metadata and moderation', icon: GroupsIcon,
    blurb: 'Browse relay groups, metadata, members, and stored events.' },
  { id: 'storage', label: 'Storage', description: 'Database and pruning', icon: StorageIcon,
    blurb: 'What this relay has stored, and whether anything is being deleted.' },
  { id: 'settings', label: 'Settings', description: 'Reset and recovery', icon: SettingsIcon,
    blurb: 'Operational controls for identity, admins, backups, version, restart, and recovery.' },
]

const searchTargets: SearchTarget[] = [
  {
    id: 'dashboard',
    title: 'Overview',
    description: 'Relay health, connections, groups, members, whitelist count, uptime',
    keywords: ['dashboard', 'health', 'stats', 'connections', 'uptime'],
  },
  {
    id: 'whitelist',
    title: 'Access',
    description: 'Open relay, whitelist enforcement, web of trust, hop limits, rate limits, allowed and blocked pubkeys',
    keywords: ['allowlist', 'whitelist', 'blacklist', 'pubkey', 'blocked', 'access', 'open relay', 'rate limits', 'web of trust', 'wot', 'hops', 'graph'],
  },
  {
    id: 'reference-accounts',
    title: 'References',
    description: 'Reference accounts and follow sync sources',
    keywords: ['follows', 'sync', 'reference accounts', 'auto whitelist'],
  },
  {
    id: 'groups',
    title: 'Groups',
    description: 'Group metadata, members, events, moderation, delete groups',
    keywords: ['metadata', 'members', 'events', 'moderation', 'delete', 'channels'],
  },
  {
    id: 'storage',
    title: 'Storage',
    description: 'Database size, LMDB path, pruning, retention, prune event kinds, reclaim disk space',
    keywords: ['storage', 'database', 'lmdb', 'pruning', 'retention', 'event_retention', 'prune kinds', 'disk', 'compact', 'compaction', 'shrink', 'reclaim', 'vacuum', 'free space'],
  },
  {
    id: 'settings',
    title: 'Settings',
    description: 'Relay version and updates, reset access configuration, reopen setup wizard, keep owner pubkey and events',
    keywords: ['reset', 'configuration', 'setup', 'wizard', 'owner', 'npub', 'event data', 'group data', 'backup', 'whitelist required', 'open relay', 'rate limits', 'version', 'update', 'upgrade', 'image', 'tag', 'release'],
  },
]

export const AdminPanel = (_props: { path?: string }) => {
  const [authenticated, setAuthenticated] = useState(false)
  const [checking, setChecking] = useState(true)
  const [activeTab, setActiveTab] = useState<Tab>('dashboard')
  const [setupStatus, setSetupStatus] = useState<SetupStatus | null>(null)
  const [globalSearch, setGlobalSearch] = useState('')
  const [relayInfo, setRelayInfo] = useState<PublicRelayInfo | null>(null)
  const [iconBroken, setIconBroken] = useState(false)

  // Brand the console from the relay's own NIP-11 identity, and point the
  // browser tab at the same icon so several deployments stay distinguishable.
  useEffect(() => {
    let cancelled = false
    adminApi.getRelayInfo()
      .then(info => {
        if (cancelled) return
        setRelayInfo(info)
        if (!info.icon) return
        const link = document.querySelector<HTMLLinkElement>('link[rel="icon"]')
          ?? document.head.appendChild(Object.assign(document.createElement('link'), { rel: 'icon' }))
        link.href = info.icon
      })
      .catch(() => undefined)
    return () => { cancelled = true }
  }, [])

  useEffect(() => {
    let cancelled = false

    const checkAccess = async () => {
      try {
        const status = await adminApi.getSetupStatus()
        if (cancelled) return
        setSetupStatus(status)

        if (status.needs_setup) {
          adminApi.clearToken()
          setAuthenticated(false)
          setChecking(false)
          return
        }

        if (!adminApi.hasToken()) {
          setChecking(false)
          return
        }

        const session = await adminApi.checkSession()
        if (cancelled) return
        setAuthenticated(session.valid)
        if (!session.valid) adminApi.clearToken()
      } catch {
        if (!cancelled) {
          adminApi.clearToken()
          setAuthenticated(false)
        }
      } finally {
        if (!cancelled) setChecking(false)
      }
    }

    void checkAccess()

    return () => {
      cancelled = true
    }
  }, [])

  if (checking) {
    return (
      <div class="min-h-screen flex items-center justify-center" style={{ background: 'var(--color-bg-primary)' }}>
        <div class="flex items-center gap-3" style={{ color: 'var(--color-text-secondary)' }}>
          <span class="lc-spinner" />
          Checking relay...
        </div>
      </div>
    )
  }

  if (setupStatus?.needs_setup) {
    return (
      <AdminSetupWizard
        status={setupStatus}
        onCompleted={() => {
          setSetupStatus({ ...setupStatus, needs_setup: false, admin_count: 1 })
          setAuthenticated(true)
        }}
      />
    )
  }

  if (!authenticated) {
    return <AdminAuth onAuthenticated={() => setAuthenticated(true)} />
  }

  const handleLogout = () => {
    adminApi.clearToken()
    setAuthenticated(false)
  }

  const active = tabs.find(tab => tab.id === activeTab) ?? tabs[0]
  const globalQuery = globalSearch.trim().toLowerCase()
  const globalResults = globalQuery
    ? searchTargets.filter(target => {
      const haystack = [
        target.title,
        target.description,
        ...target.keywords,
      ].join(' ').toLowerCase()
      return haystack.includes(globalQuery)
    })
    : []

  const openSearchResult = (tab: Tab) => {
    setActiveTab(tab)
    setGlobalSearch('')
  }

  return (
    <SettingsDirtyProvider>
    {/* h-screen + overflow-hidden, not min-h-screen: `main` below carries
        `overflow-y-auto`, and an overflow container only becomes a scrollport
        if something bounds its height. Stretched to content inside a
        min-h-screen flex row it never scrolled -- the document did -- so the
        sticky header and save bar had no scrollport to stick against and rode
        the page up out of view. */}
    <div class="admin-shell h-screen overflow-hidden flex flex-col md:flex-row" style={{ background: 'var(--color-bg-primary)' }}>
      {/* Sidebar is pinned to the viewport so its footer actions stay reachable.
          Without md:h-screen the aside stretches to the full page height on
          content-heavy tabs, and md:flex-1 on the nav pushes Open Chat / Sign
          out thousands of pixels down, below the fold. */}
      <aside class="md:w-72 flex-shrink-0 flex flex-col md:sticky md:top-0 md:h-screen" style={{ background: 'var(--color-bg-secondary)', borderRight: '1px solid var(--color-border)' }}>
        {/* admin-header-bar gives this the same height as the page header on
            the right, so the two bottom rules meet as one continuous line. */}
        <div class="admin-header-bar px-5" style={{ borderBottom: '1px solid var(--color-border)' }}>
          <a href="/" class="flex items-center gap-2.5 min-w-0">
            {relayInfo?.icon && !iconBroken
              ? <img
                  src={relayInfo.icon}
                  alt=""
                  class="flex-shrink-0 object-cover"
                  style={{ width: '32px', height: '32px', borderRadius: '7px' }}
                  // A configured-but-unreachable URL would otherwise render as
                  // a broken-image glyph in the console header.
                  onError={() => setIconBroken(true)}
                />
              : <RelayIcon class="w-7 h-7 flex-shrink-0" />}
            <span class="min-w-0">
              <span class="block text-lg font-bold truncate" style={{ color: 'var(--color-accent)' }}>
                {relayInfo?.name || 'Obelisk Relay'}
              </span>
              <span class="block text-xs" style={{ color: 'var(--color-text-secondary)' }}>Admin console</span>
            </span>
          </a>
        </div>

        {/* md:min-h-0 lets the nav shrink below its content height (flex items
            default to min-height:auto and refuse to), so it scrolls internally
            instead of overflowing the aside and displacing the footer. */}
        <nav class="flex md:flex-col gap-2 overflow-x-auto md:overflow-x-visible md:overflow-y-auto p-3 md:flex-1 md:min-h-0">
          {tabs.map(tab => {
            const selected = activeTab === tab.id
            return (
              <button
                key={tab.id}
                onClick={() => setActiveTab(tab.id)}
                class="text-left px-3 py-3 transition-colors flex-shrink-0 md:flex-shrink"
                style={{
                  minWidth: '150px',
                  borderRadius: '8px',
                  background: selected ? 'rgba(var(--color-accent-rgb), 0.09)' : 'transparent',
                  border: selected ? '1px solid rgba(var(--color-accent-rgb), 0.26)' : '1px solid transparent',
                  color: selected ? 'var(--color-accent)' : 'var(--color-text-primary)',
                }}
              >
                <span class="flex items-center gap-2.5">
                  <tab.icon class="w-[18px] h-[18px] flex-shrink-0 opacity-80" />
                  <span class="min-w-0">
                    <span class="block text-sm font-semibold">{tab.label}</span>
                    <span class="block text-xs mt-0.5" style={{ color: selected ? 'rgba(var(--color-accent-rgb), 0.78)' : 'var(--color-text-secondary)' }}>
                      {tab.description}
                    </span>
                  </span>
                </span>
              </button>
            )
          })}
        </nav>

        <div class="p-4 grid grid-cols-1 sm:grid-cols-2 md:grid-cols-1 gap-2" style={{ borderTop: '1px solid var(--color-border)' }}>
          {/* Open the client against THIS relay, not a fixed one. The admin
              panel is served by the relay itself, so the host serving this page
              is the relay's own public hostname -- which keeps the link correct
              across public.obelisk.ar, lacrypta, and any other deployment
              without per-host configuration. */}
          <a
            href={`https://obelisk.ar/app?relay=${encodeURIComponent(window.location.host)}`}
            target="_blank"
            rel="noopener noreferrer"
            class="admin-sidebar-action"
          >
            Open Chat
          </a>
          <button
            type="button"
            onClick={handleLogout}
            class="admin-sidebar-action admin-sidebar-action-danger"
          >
            Sign out
          </button>
        </div>
      </aside>

      {/* A three-row column: header, scrolling content, save bar.
          The header and the save bar are siblings of the scroll region rather
          than sticky elements inside it. They were sticky, which silently did
          nothing -- `main` stretched to its content so it never scrolled, and
          an element can only stick against a scrollport that actually scrolls.
          The header rode the page up, and the save bar sat below the fold where
          rate-limit changes could not be committed at all. Out here they cannot
          scroll away no matter what the content does. */}
      <main class="flex-1 min-h-0 flex flex-col">
        <div class="admin-header-bar flex-shrink-0 px-5 md:px-8" style={{ borderBottom: '1px solid var(--color-border)' }}>
          <div class="flex flex-col lg:flex-row lg:items-center lg:justify-between gap-4 w-full">
            <div class="min-w-0">
              <h1 class="text-2xl font-bold">{active.label}</h1>
              {active.blurb && (
                <p class="admin-header-blurb">{active.blurb}</p>
              )}
            </div>
            <div class="admin-global-search">
              <SearchIcon class="admin-search-icon" />
              <input
                type="search"
                value={globalSearch}
                onInput={e => setGlobalSearch((e.target as HTMLInputElement).value)}
                placeholder="Search admin pages and settings"
                class="admin-search-input"
              />
              {globalSearch && (
                <div class="admin-search-results">
                  {globalResults.length > 0 ? (
                    globalResults.map(result => (
                      <button
                        key={result.id}
                        type="button"
                        onClick={() => openSearchResult(result.id)}
                        class="admin-search-result"
                      >
                        <span class="flex items-start gap-2.5">
                          {(() => {
                            const Icon = TAB_ICONS[result.id]
                            return <Icon class="w-4 h-4 flex-shrink-0 mt-0.5 opacity-70" />
                          })()}
                          <span class="min-w-0">
                            <span class="block text-sm font-semibold">{result.title}</span>
                            <span class="block text-xs mt-0.5">{result.description}</span>
                          </span>
                        </span>
                      </button>
                    ))
                  ) : (
                    <div class="admin-search-empty">No admin settings match.</div>
                  )}
                </div>
              )}
            </div>
          </div>
        </div>

        {/* The only thing that scrolls. min-h-0 lets it shrink below its
            content height, which is what allows overflow-y-auto to scroll
            rather than stretch the column. */}
        <div class="flex-1 min-h-0 overflow-y-auto">
        <div class="p-5 md:p-8 max-w-7xl">
          {activeTab === 'dashboard' && <Dashboard />}
          {activeTab === 'whitelist' && <WhitelistManager />}
          {activeTab === 'reference-accounts' && <ReferenceAccountsManager />}
          {activeTab === 'groups' && <GroupsOverview />}
          {activeTab === 'storage' && <StorageManager />}
          {activeTab === 'settings' && (
            <RelaySettings
              onNavigate={tab => setActiveTab(tab)}
              onResetToSetup={result => {
                adminApi.clearToken()
                setSetupStatus({
                  needs_setup: true,
                  admin_count: 0,
                  relay_url: setupStatus?.relay_url ?? window.location.origin,
                  whitelisted_count: result.whitelisted_count,
                  reference_account_count: result.reference_account_count,
                  setup_owner_pubkey: result.setup_owner_pubkey,
                  setup_owner_npub: result.setup_owner_npub,
                })
                setAuthenticated(false)
              }}
            />
          )}
        </div>
        </div>

        {/* Renders nothing until a section reports pending changes. Outside the
            scroll region, so it is reachable from anywhere on the page. */}
        <AdminSaveBar />
      </main>
    </div>
    </SettingsDirtyProvider>
  )
}
