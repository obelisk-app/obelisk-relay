import { useState } from 'preact/hooks'
import { useSettingsDirty } from './settingsDirty'

/**
 * The console's single commit point.
 *
 * Pinned to the bottom of the scrolling content area and hidden entirely until
 * something is actually pending, so a read-only visit looks the same as before.
 * Replaces the per-section Save buttons, which scrolled out of view along with
 * the fields they belonged to.
 *
 * Consequence warnings live here rather than beside the fields because they are
 * only true while a change is pending. "Saving open mode clears active
 * whitelist entries" used to show whenever open mode was *selected*, including
 * when it was already saved and there was nothing left to clear.
 */
export const AdminSaveBar = () => {
  const { snapshots, callbacksFor } = useSettingsDirty()
  const [saving, setSaving] = useState(false)
  const [failures, setFailures] = useState<string[]>([])

  const dirty = Object.values(snapshots).filter(s => s.dirty)
  if (dirty.length === 0) return null

  const blockers = dirty.filter(s => s.blocked)
  const consequences = dirty.filter(s => s.consequence)

  const handleSave = async () => {
    setSaving(true)
    setFailures([])
    const failed: string[] = []
    // Sequential, not parallel: several of these rewrite the same settings
    // file, and concurrent writes would race for the last word.
    for (const section of dirty) {
      if (section.blocked) continue
      try {
        await callbacksFor(section.id)?.save()
      } catch (e) {
        failed.push(`${section.label}: ${(e as Error).message}`)
      }
    }
    setFailures(failed)
    setSaving(false)
  }

  const handleDiscard = () => {
    setFailures([])
    for (const section of dirty) {
      callbacksFor(section.id)?.discard()
    }
  }

  return (
    <div class="admin-save-bar" role="region" aria-label="Unsaved changes">
      <div class="admin-save-bar-inner">
        <div class="admin-save-bar-message">
          <strong>
            {dirty.length} unsaved change{dirty.length === 1 ? '' : 's'}
          </strong>
          <span class="admin-save-bar-sections">{dirty.map(s => s.label).join(' · ')}</span>
          {consequences.map(s => (
            <span key={s.id} class="admin-save-bar-consequence">
              {s.consequence}
            </span>
          ))}
          {blockers.map(s => (
            <span key={s.id} class="admin-save-bar-blocked">
              {s.label}: {s.blocked}
            </span>
          ))}
          {failures.map(f => (
            <span key={f} class="admin-save-bar-error">
              {f}
            </span>
          ))}
        </div>
        <div class="admin-save-bar-actions">
          <button
            type="button"
            onClick={handleDiscard}
            disabled={saving}
            class="admin-save-bar-discard"
          >
            Discard
          </button>
          <button
            type="button"
            onClick={handleSave}
            disabled={saving || blockers.length > 0}
            class="admin-save-bar-save"
          >
            {saving ? 'Saving…' : 'Save changes'}
          </button>
        </div>
      </div>
    </div>
  )
}
