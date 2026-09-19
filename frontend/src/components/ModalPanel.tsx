import { useEffect, useRef } from 'preact/hooks'
import type { ComponentChildren } from 'preact'

interface ModalPanelProps {
  children: ComponentChildren
  /** id of the heading that names this dialog, for `aria-labelledby`. */
  labelledBy: string
  onClose: () => void
  class?: string
}

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), textarea:not([disabled]), ' +
  'select:not([disabled]), [tabindex]:not([tabindex="-1"])'

/**
 * The dialog half of a modal: semantics, focus and Escape.
 *
 * None of the app's modals were dialogs. They were `<div>`s that happened to be
 * positioned over the page — no `role`, no `aria-modal`, no accessible name, and
 * nothing done about focus. To a screen reader they did not exist as a distinct
 * thing, and for anyone navigating by keyboard focus stayed behind on the page
 * underneath, so Tab walked the content the dialog was covering.
 *
 * Wrapping the panel supplies all of that without restructuring the contents:
 *
 * - `role="dialog"` + `aria-modal` + `aria-labelledby` so it is announced as a
 *   dialog with a name;
 * - focus moves in on open and returns to the element that opened it on close,
 *   which is what makes a modal usable without a mouse;
 * - Tab cycles within the panel instead of escaping to the page behind;
 * - Escape closes, matching what the backdrop click already did.
 *
 * `ProfileMenu` already implemented a correct trap for its dropdown; this is the
 * same idea, made reusable.
 */
export const ModalPanel = ({ children, labelledBy, onClose, class: className }: ModalPanelProps) => {
  const panelRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    // Remember what had focus so it can be given back on close. Without this,
    // closing a dialog drops focus to <body> and a keyboard user restarts from
    // the top of the page.
    //
    // Captured into a local and restored from that local in the cleanup, rather
    // than read back off the ref: by teardown the ref may already have been
    // reassigned by a later mount, and the cleanup would then return focus to
    // the wrong element.
    const previouslyFocused = document.activeElement as HTMLElement | null

    // Reading the ref here is the point: the effect runs after mount, so this is
    // the node that was just rendered. Captured into a local so the listener and
    // the cleanup both close over that exact node rather than re-reading a ref
    // that a later mount may have reassigned.
     
    const panel = panelRef.current
    const first = panel?.querySelector<HTMLElement>(FOCUSABLE)
    // Prefer the first control; fall back to the panel itself, which is why it
    // carries tabIndex={-1}.
    ;(first ?? panel)?.focus()

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation()
        onClose()
        return
      }
      if (e.key !== 'Tab' || !panel) return

      const focusable = Array.from(
        panel.querySelectorAll<HTMLElement>(FOCUSABLE),
      ).filter(el => el.offsetParent !== null || el === document.activeElement)
      if (focusable.length === 0) {
        e.preventDefault()
        return
      }

      const firstEl = focusable[0]
      const lastEl = focusable[focusable.length - 1]
      if (e.shiftKey && document.activeElement === firstEl) {
        e.preventDefault()
        lastEl.focus()
      } else if (!e.shiftKey && document.activeElement === lastEl) {
        e.preventDefault()
        firstEl.focus()
      }
    }

    document.addEventListener('keydown', onKeyDown, true)
    return () => {
      document.removeEventListener('keydown', onKeyDown, true)
      previouslyFocused?.focus?.()
    }
  }, [onClose])

  return (
    <div
      ref={panelRef}
      role="dialog"
      aria-modal="true"
      aria-labelledby={labelledBy}
      tabIndex={-1}
      class={className}
    >
      {children}
    </div>
  )
}
