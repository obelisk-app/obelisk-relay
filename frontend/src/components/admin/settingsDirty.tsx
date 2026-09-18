import { createContext, type ComponentChildren } from 'preact'
import { useCallback, useContext, useEffect, useMemo, useRef, useState } from 'preact/hooks'

/**
 * One editable section's status, as the save bar sees it.
 *
 * Settings used to be committed per section, each with its own button sitting
 * next to its own fields. Change three rate limits near the top of Access and
 * the button that commits them scrolls out of view, taking its consequence
 * warning with it. This context lets every section report upward so a single
 * bar -- pinned to the bottom of the page -- can own the commit.
 */
export interface DirtySnapshot {
  id: string
  /** Shown in the bar when this section has pending changes, e.g. "Access". */
  label: string
  dirty: boolean
  /**
   * Why this section cannot be saved yet, e.g. an unconfirmed destructive
   * action. Dirty-but-blocked still counts as an unsaved change; it just
   * disables the button and says what is missing.
   */
  blocked?: string | null
  /**
   * What saving this will also do, e.g. clearing the whitelist. Surfaced only
   * while the change is actually pending -- a warning about something that has
   * already happened is noise.
   */
  consequence?: string | null
}

interface SectionCallbacks {
  save: () => Promise<void>
  discard: () => void
}

/**
 * The write side, split out from the snapshots on purpose.
 *
 * These four functions never change identity, so a section's register effect
 * runs exactly once. Handing sections a context value that also carried
 * `snapshots` made that effect re-run on every keystroke anywhere in the
 * console -- and since its cleanup unregisters, each rerun deleted the snapshot
 * that had just been added, which is an infinite render loop rather than merely
 * wasted work.
 */
interface SettingsDirtyActions {
  register: (id: string, callbacks: { current: SectionCallbacks }) => void
  unregister: (id: string) => void
  update: (snapshot: DirtySnapshot) => void
  callbacksFor: (id: string) => SectionCallbacks | undefined
}

const noopActions: SettingsDirtyActions = {
  register: () => {},
  unregister: () => {},
  update: () => {},
  callbacksFor: () => undefined,
}

const ActionsContext = createContext<SettingsDirtyActions>(noopActions)
const SnapshotsContext = createContext<Record<string, DirtySnapshot>>({})

const sameSnapshot = (a: DirtySnapshot | undefined, b: DirtySnapshot) =>
  a !== undefined &&
  a.label === b.label &&
  a.dirty === b.dirty &&
  (a.blocked ?? null) === (b.blocked ?? null) &&
  (a.consequence ?? null) === (b.consequence ?? null)

export const SettingsDirtyProvider = ({ children }: { children: ComponentChildren }) => {
  const [snapshots, setSnapshots] = useState<Record<string, DirtySnapshot>>({})
  // Callbacks live in a ref rather than state: they are recreated on every
  // render of the owning section, and storing them in state would re-render
  // the whole console each time.
  const callbacks = useRef(new Map<string, { current: SectionCallbacks }>())

  const register = useCallback((id: string, ref: { current: SectionCallbacks }) => {
    callbacks.current.set(id, ref)
  }, [])

  const unregister = useCallback((id: string) => {
    callbacks.current.delete(id)
    setSnapshots(prev => {
      if (!(id in prev)) return prev
      const next = { ...prev }
      delete next[id]
      return next
    })
  }, [])

  const update = useCallback((snapshot: DirtySnapshot) => {
    setSnapshots(prev =>
      sameSnapshot(prev[snapshot.id], snapshot) ? prev : { ...prev, [snapshot.id]: snapshot },
    )
  }, [])

  const callbacksFor = useCallback((id: string) => callbacks.current.get(id)?.current, [])

  // Stable for the provider's whole lifetime: every member is a `useCallback`
  // with no dependencies.
  const actions = useMemo(
    () => ({ register, unregister, update, callbacksFor }),
    [register, unregister, update, callbacksFor],
  )

  return (
    <ActionsContext.Provider value={actions}>
      <SnapshotsContext.Provider value={snapshots}>{children}</SnapshotsContext.Provider>
    </ActionsContext.Provider>
  )
}

/**
 * Report a section's pending changes to the save bar.
 *
 * The section keeps its own save function and its own validation; this only
 * moves the trigger. Pass `blocked` rather than silently reporting clean, so
 * the bar can explain why Save is unavailable instead of appearing broken.
 */
export const useDirtySection = (
  snapshot: DirtySnapshot,
  callbacks: SectionCallbacks,
) => {
  const actions = useContext(ActionsContext)
  // The section rebuilds these closures every render; the bar needs the latest
  // pair without that counting as a re-registration.
  const ref = useRef(callbacks)
  ref.current = callbacks

  const { id, label, dirty } = snapshot
  const blocked = snapshot.blocked ?? null
  const consequence = snapshot.consequence ?? null

  useEffect(() => {
    actions.register(id, ref)
    return () => actions.unregister(id)
  }, [actions, id])

  useEffect(() => {
    actions.update({ id, label, dirty, blocked, consequence })
  }, [actions, id, label, dirty, blocked, consequence])
}

/** For the save bar: the current snapshots plus the actions to act on them. */
export const useSettingsDirty = () => {
  const snapshots = useContext(SnapshotsContext)
  const actions = useContext(ActionsContext)
  return { snapshots, ...actions }
}
