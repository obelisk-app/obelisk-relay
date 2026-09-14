import { useEffect, useRef, useState } from 'preact/hooks'

/**
 * Checkbox selection over an ordered list, with shift-click ranges.
 *
 * Shared by the events and members tabs. The range anchor has to be tracked
 * against the *rendered* order — the events list renders oldest-first while the
 * API returns newest-first, so computing a range against the source array
 * selects the wrong rows.
 *
 * Selections for ids that leave the list are dropped, so the count can never
 * claim more than the operator can see and act on.
 */
export const useRowSelection = (orderedIds: string[]) => {
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const lastIndexRef = useRef<number | null>(null)

  useEffect(() => {
    setSelected(prev => {
      if (prev.size === 0) return prev
      const visible = new Set(orderedIds)
      const next = new Set([...prev].filter(id => visible.has(id)))
      return next.size === prev.size ? prev : next
    })
  }, [orderedIds.join(',')])

  const toggle = (id: string, index: number, shiftKey: boolean) => {
    setSelected(prev => {
      const next = new Set(prev)
      if (shiftKey && lastIndexRef.current !== null) {
        const [from, to] = index < lastIndexRef.current
          ? [index, lastIndexRef.current]
          : [lastIndexRef.current, index]
        // The clicked row decides whether the range selects or deselects.
        const select = !prev.has(id)
        for (let i = from; i <= to; i += 1) {
          const rowId = orderedIds[i]
          if (!rowId) continue
          if (select) next.add(rowId)
          else next.delete(rowId)
        }
      } else if (next.has(id)) {
        next.delete(id)
      } else {
        next.add(id)
      }
      return next
    })
    lastIndexRef.current = index
  }

  const selectAll = () => setSelected(new Set(orderedIds))
  const selectMatching = (ids: string[]) => setSelected(new Set(ids))
  const clear = () => {
    setSelected(new Set())
    lastIndexRef.current = null
  }

  return { selected, toggle, selectAll, selectMatching, clear, setSelected }
}
