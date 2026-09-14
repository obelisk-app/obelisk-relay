/**
 * Admin console icon set.
 *
 * Inline SVG following the house pattern in SearchIcon.tsx: a 20x20 viewBox,
 * `currentColor` strokes so icons inherit theme and selected-state colour,
 * `aria-hidden` because every icon sits next to its own text label, and a
 * `class` passthrough for sizing. Deliberately not an icon library — the
 * frontend is served by the relay itself and this keeps the bundle unchanged.
 */

interface IconProps {
  class?: string
}

const base = (className: string) => ({
  class: className,
  viewBox: '0 0 20 20',
  fill: 'none',
  xmlns: 'http://www.w3.org/2000/svg',
  'aria-hidden': 'true' as const,
})

const stroke = {
  stroke: 'currentColor',
  'stroke-width': '1.6',
  'stroke-linecap': 'round' as const,
  'stroke-linejoin': 'round' as const,
}

/** Overview — a pulse/health line. */
export const OverviewIcon = ({ class: className = '' }: IconProps) => (
  <svg {...base(className)}>
    <path d="M2.5 11h3l2-5 3 8 2-4h5" {...stroke} />
  </svg>
)

/** Access — a shield with a check. */
export const AccessIcon = ({ class: className = '' }: IconProps) => (
  <svg {...base(className)}>
    <path d="M10 2.5 4 5v4.5c0 3.4 2.4 6.6 6 8 3.6-1.4 6-4.6 6-8V5l-6-2.5Z" {...stroke} />
    <path d="m7.5 9.8 1.8 1.8 3.4-3.6" {...stroke} />
  </svg>
)

/** References — linked follow sources. */
export const ReferencesIcon = ({ class: className = '' }: IconProps) => (
  <svg {...base(className)}>
    <path d="M8.5 11.5a3 3 0 0 0 4.3.3l2-2a3 3 0 0 0-4.2-4.3l-1.1 1" {...stroke} />
    <path d="M11.5 8.5a3 3 0 0 0-4.3-.3l-2 2a3 3 0 0 0 4.2 4.3l1.1-1" {...stroke} />
  </svg>
)

/** Groups — people. */
export const GroupsIcon = ({ class: className = '' }: IconProps) => (
  <svg {...base(className)}>
    <circle cx="7.5" cy="7" r="2.5" {...stroke} />
    <path d="M2.5 16c0-2.5 2.2-4.2 5-4.2s5 1.7 5 4.2" {...stroke} />
    <path d="M13 5.2A2.5 2.5 0 0 1 14.8 9" {...stroke} />
    <path d="M14.5 12.2c1.8.5 3 1.9 3 3.8" {...stroke} />
  </svg>
)

/** Storage — stacked database discs. */
export const StorageIcon = ({ class: className = '' }: IconProps) => (
  <svg {...base(className)}>
    <ellipse cx="10" cy="5" rx="6" ry="2.5" {...stroke} />
    <path d="M4 5v5c0 1.4 2.7 2.5 6 2.5s6-1.1 6-2.5V5" {...stroke} />
    <path d="M4 10v5c0 1.4 2.7 2.5 6 2.5s6-1.1 6-2.5v-5" {...stroke} />
  </svg>
)

/** Settings — a gear. */
export const SettingsIcon = ({ class: className = '' }: IconProps) => (
  <svg {...base(className)}>
    <circle cx="10" cy="10" r="2.4" {...stroke} />
    <path
      d="M10 2.5v1.8M10 15.7v1.8M17.5 10h-1.8M4.3 10H2.5M15.3 4.7l-1.3 1.3M6 14l-1.3 1.3M15.3 15.3 14 14M6 6 4.7 4.7"
      {...stroke}
    />
  </svg>
)

/** Relay identity / branding. */
export const RelayIcon = ({ class: className = '' }: IconProps) => (
  <svg {...base(className)}>
    <circle cx="10" cy="10" r="1.8" {...stroke} />
    <path d="M6.1 6.1a5.5 5.5 0 0 0 0 7.8M13.9 13.9a5.5 5.5 0 0 0 0-7.8" {...stroke} />
    <path d="M3.6 3.6a9 9 0 0 0 0 12.8M16.4 16.4a9 9 0 0 0 0-12.8" {...stroke} />
  </svg>
)
