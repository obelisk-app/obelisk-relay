import type { ComponentChildren, JSX } from 'preact'

/**
 * The shared empty state for the admin console.
 *
 * Replaces a dashed box holding two grey paragraphs. That shape gave a serious
 * warning ("anyone can connect") exactly the same weight as a reassurance
 * ("nobody is blocked, this is normal"), and buried the thing you were supposed
 * to do next inside a sentence -- "Add a pubkey above" -- where it was not
 * clickable.
 *
 * So: an icon to anchor it, a headline, one line of body, the relevant number
 * as a figure rather than prose, and the next step as an actual button.
 */
export interface AdminEmptyStateProps {
  /**
   * `neutral` for "nothing here, and that is fine". `caution` for a state the
   * operator should probably change -- it takes the warning colour, so an open
   * relay does not read like an everything-is-fine message.
   */
  tone?: 'neutral' | 'caution'
  icon?: (props: { class?: string }) => JSX.Element
  headline: string
  children?: ComponentChildren
  /** A single number worth pulling out of the sentence, e.g. "0 pubkeys". */
  stat?: string
  action?: {
    label: string
    onClick: () => void
  }
}

export const AdminEmptyState = ({
  tone = 'neutral',
  icon: Icon,
  headline,
  children,
  stat,
  action,
}: AdminEmptyStateProps) => (
  <div class={`admin-empty-state admin-empty-state-${tone}`}>
    {Icon && (
      <span class="admin-empty-state-icon" aria-hidden="true">
        <Icon class="w-5 h-5" />
      </span>
    )}
    <div class="admin-empty-state-body">
      <p class="admin-empty-state-headline">{headline}</p>
      {children && <p class="admin-empty-state-copy">{children}</p>}
      {(stat || action) && (
        <div class="admin-empty-state-footer">
          {stat && <span class="admin-empty-state-stat">{stat}</span>}
          {action && (
            <button
              type="button"
              onClick={action.onClick}
              class="admin-empty-state-action"
            >
              {action.label}
            </button>
          )}
        </div>
      )}
    </div>
  </div>
)
