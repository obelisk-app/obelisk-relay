import { useState, useEffect } from 'preact/hooks'
import { adminApi, type SetupStatus } from '../../services/AdminApiClient'
import { AdminAuth } from './AdminAuth'
import { AdminSetupWizard } from './AdminSetupWizard'
import { Dashboard } from './Dashboard'
import { WhitelistManager } from './WhitelistManager'
import { GroupsOverview } from './GroupsOverview'
import { ReferenceAccountsManager } from './ReferenceAccountsManager'
import { RelaySettings } from './RelaySettings'
import { StorageManager } from './StorageManager'
import { SearchIcon } from './SearchIcon'

type Tab = 'dashboard' | 'whitelist' | 'reference-accounts' | 'groups' | 'storage' | 'settings'

interface NavItem {
  id: Tab
  label: string
  description: string
}

interface SearchTarget {
  id: Tab
  title: string
  description: string
  keywords: string[]
}

const tabs: NavItem[] = [
  { id: 'dashboard', label: 'Overview', description: 'Relay health' },
  { id: 'whitelist', label: 'Access', description: 'Allowlist and blocks' },
  { id: 'reference-accounts', label: 'References', description: 'Follow sync sources' },
  { id: 'groups', label: 'Groups', description: 'Metadata and moderation' },
  { id: 'storage', label: 'Storage', description: 'Database and pruning' },
  { id: 'settings', label: 'Settings', description: 'Reset and recovery' },
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
    description: 'Open relay, whitelist enforcement, rate limits, allowed pubkeys, blocked pubkeys',
    keywords: ['allowlist', 'whitelist', 'blacklist', 'pubkey', 'blocked', 'access', 'open relay', 'rate limits'],
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
    description: 'Database size, LMDB path, pruning, retention, prune event kinds',
    keywords: ['storage', 'database', 'lmdb', 'pruning', 'retention', 'event_retention', 'prune kinds', 'disk'],
  },
  {
    id: 'settings',
    title: 'Settings',
    description: 'Reset access configuration, reopen setup wizard, keep owner pubkey and events',
    keywords: ['reset', 'configuration', 'setup', 'wizard', 'owner', 'npub', 'event data', 'group data', 'backup', 'whitelist required', 'open relay', 'rate limits'],
  },
]

export const AdminPanel = (_props: { path?: string }) => {
  const [authenticated, setAuthenticated] = useState(false)
  const [checking, setChecking] = useState(true)
  const [activeTab, setActiveTab] = useState<Tab>('dashboard')
  const [setupStatus, setSetupStatus] = useState<SetupStatus | null>(null)
  const [globalSearch, setGlobalSearch] = useState('')

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
    <div class="min-h-screen flex flex-col md:flex-row" style={{ background: 'var(--color-bg-primary)' }}>
      <aside class="md:w-72 flex-shrink-0 flex flex-col" style={{ background: 'var(--color-bg-secondary)', borderRight: '1px solid var(--color-border)' }}>
        <div class="p-5" style={{ borderBottom: '1px solid var(--color-border)' }}>
          <a href="/" class="text-lg font-bold block" style={{ color: '#b4f953' }}>Obelisk Relay</a>
          <div class="text-xs mt-1" style={{ color: 'var(--color-text-secondary)' }}>Admin console</div>
        </div>

        <nav class="flex md:flex-col gap-2 overflow-x-auto md:overflow-x-visible p-3 md:flex-1">
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
                  background: selected ? 'rgba(180,249,83,0.09)' : 'transparent',
                  border: selected ? '1px solid rgba(180,249,83,0.26)' : '1px solid transparent',
                  color: selected ? '#b4f953' : 'var(--color-text-primary)',
                }}
              >
                <span class="block text-sm font-semibold">{tab.label}</span>
                <span class="block text-xs mt-0.5" style={{ color: selected ? 'rgba(180,249,83,0.78)' : 'var(--color-text-secondary)' }}>
                  {tab.description}
                </span>
              </button>
            )
          })}
        </nav>

        <div class="p-4 grid grid-cols-1 sm:grid-cols-2 md:grid-cols-1 gap-2" style={{ borderTop: '1px solid var(--color-border)' }}>
          <a
            href="https://dex.obelisk.ar"
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

      <main class="flex-1 overflow-auto">
        <div class="px-5 md:px-8 py-5 md:py-7" style={{ borderBottom: '1px solid var(--color-border)', background: 'rgba(255,255,255,0.015)' }}>
          <div class="flex flex-col lg:flex-row lg:items-center lg:justify-between gap-4">
            <div>
              <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>Admin</div>
              <h1 class="mt-1 text-2xl font-bold">{active.label}</h1>
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
                        <span class="block text-sm font-semibold">{result.title}</span>
                        <span class="block text-xs mt-0.5">{result.description}</span>
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
      </main>
    </div>
  )
}
