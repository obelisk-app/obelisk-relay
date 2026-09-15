/**
 * Method icons for the login widget.
 *
 * `@nostr-wot/ui` ships emoji defaults (🔌 🔐 ✨ 🔑). They render at wildly
 * different sizes and weights across platforms and do not inherit theme
 * colour, so the sign-in screen looked unlike the rest of the console.
 *
 * Same set obelisk-dex uses, so an operator moving between the app and the
 * relay's admin panel sees one sign-in screen rather than two.
 */
import type { JSX } from 'preact'

const base = {
  fill: 'none' as const,
  stroke: 'currentColor',
  'stroke-width': 2,
  'stroke-linecap': 'round' as const,
  'stroke-linejoin': 'round' as const,
  viewBox: '0 0 24 24',
  width: 20,
  height: 20,
  'aria-hidden': 'true' as const,
}

type IconProps = JSX.SVGAttributes<SVGSVGElement>

/** NIP-07 browser extension. */
export const LockIcon = (p: IconProps) => (
  <svg {...base} {...p}>
    <rect x="3" y="11" width="18" height="11" rx="2" ry="2" />
    <path d="M7 11V7a5 5 0 0110 0v4" />
  </svg>
)

/** NIP-46 remote signer — the key never leaves the other device. */
export const ShieldIcon = (p: IconProps) => (
  <svg {...base} {...p}>
    <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
  </svg>
)

/** Generate a new key. */
export const SparkleIcon = (p: IconProps) => (
  <svg {...base} {...p}>
    <path d="M12 3v4M12 17v4M3 12h4M17 12h4M5.6 5.6l2.8 2.8M15.6 15.6l2.8 2.8M18.4 5.6l-2.8 2.8M8.4 15.6l-2.8 2.8" />
  </svg>
)

/** Paste an nsec or hex key. */
export const KeyIcon = (p: IconProps) => (
  <svg {...base} {...p}>
    <circle cx="7.5" cy="15.5" r="4.5" />
    <path d="M10.7 12.3L21 2M17 6l3 3M14 9l3 3" />
  </svg>
)

/** Ready to spread onto `<LoginWidget methodIcons={…}>`. */
export const LOGIN_METHOD_ICONS = {
  nip07: <LockIcon />,
  nip46: <ShieldIcon />,
  generate: <SparkleIcon />,
  import: <KeyIcon />,
}
