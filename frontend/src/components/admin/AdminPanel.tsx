import { useState, useEffect } from 'preact/hooks'
import { adminApi, type SetupStatus } from '../../services/AdminApiClient'
import { AdminAuth } from './AdminAuth'
import { AdminSetupWizard } from './AdminSetupWizard'
import { Dashboard } from './Dashboard'
import { WhitelistManager } from './WhitelistManager'
import { GroupsOverview } from './GroupsOverview'
import { ReferenceAccountsManager } from './ReferenceAccountsManager'
import { RelaySettings } from './RelaySettings'

type Tab = 'dashboard' | 'whitelist' | 'reference-accounts' | 'groups' | 'settings'

interface NavItem {
  id: Tab
  label: string
  description: string
}

const tabs: NavItem[] = [
  { id: 'dashboard', label: 'Overview', description: 'Relay health' },
  { id: 'whitelist', label: 'Access', description: 'Allowlist and blocks' },
  { id: 'reference-accounts', label: 'References', description: 'Follow sync sources' },
  { id: 'groups', label: 'Groups', description: 'Metadata and moderation' },
  { id: 'settings', label: 'Settings', description: 'Reset and recovery' },
]

export const AdminPanel = (_props: { path?: string }) => {
  const [authenticated, setAuthenticated] = useState(false)
  const [checking, setChecking] = useState(true)
  const [activeTab, setActiveTab] = useState<Tab>('dashboard')
  const [setupStatus, setSetupStatus] = useState<SetupStatus | null>(null)

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

        <div class="p-4 flex md:block items-center justify-between gap-3" style={{ borderTop: '1px solid var(--color-border)' }}>
          <a href="https://dex.obelisk.ar" target="_blank" rel="noopener noreferrer" class="text-sm hover:underline" style={{ color: 'var(--color-text-secondary)' }}>
            Open Chat
          </a>
          <button
            onClick={handleLogout}
            class="text-sm text-red-400 hover:text-red-300 transition-colors"
          >
            Logout
          </button>
        </div>
      </aside>

      <main class="flex-1 overflow-auto">
        <div class="px-5 md:px-8 py-5 md:py-7" style={{ borderBottom: '1px solid var(--color-border)', background: 'rgba(255,255,255,0.015)' }}>
          <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>Admin</div>
          <h1 class="mt-1 text-2xl font-bold">{active.label}</h1>
        </div>

        <div class="p-5 md:p-8 max-w-7xl">
          {activeTab === 'dashboard' && <Dashboard />}
          {activeTab === 'whitelist' && <WhitelistManager />}
          {activeTab === 'reference-accounts' && <ReferenceAccountsManager />}
          {activeTab === 'groups' && <GroupsOverview />}
          {activeTab === 'settings' && <RelaySettings />}
        </div>
      </main>
    </div>
  )
}
