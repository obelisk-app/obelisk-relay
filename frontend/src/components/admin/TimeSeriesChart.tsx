import { useState } from 'preact/hooks'

export interface SeriesPoint {
  /** Unix seconds. */
  at: number
  value: number
}

interface TimeSeriesChartProps {
  points: SeriesPoint[]
  /** Renders a value for the tooltip, the legend and the accessible label. */
  format: (value: number) => string
  /** What one point means, e.g. "Database size". Used in the aria-label. */
  label: string
  /** Shown instead of a chart until there are at least two points. */
  emptyHint?: string
  /**
   * Anchor the y-axis at zero. True for anything where the distance from zero
   * is the point (disk used); false for a series that hovers in a band and
   * would otherwise be a flat line (connection count).
   */
  zeroBased?: boolean
}

const W = 640
const H = 140
const PAD_L = 8
const PAD_B = 18

const formatUnix = (unix: number) => new Date(unix * 1000).toLocaleString()

/**
 * One series over time, as an inline SVG area chart with a hover readout.
 *
 * Extracted from the storage screen's disk graph so the same shape serves
 * connections and subscriptions too. Hand-drawn rather than pulled from a
 * charting library for the reasons the original carried: the relay serves its
 * frontend under a strict CSP with no external origins, the bundle is already
 * over a megabyte, and this is one series of at most 720 points. An SVG path is
 * a few lines and has no supply chain.
 *
 * The hover is the reason this is a component and not a function. A chart that
 * shows a shape but never a number makes you estimate against an axis, and the
 * question an operator actually has is "how much, and when" — so moving across
 * it snaps to the nearest sample and says both.
 */
export const TimeSeriesChart = ({
  points,
  format,
  label,
  emptyHint,
  zeroBased = true,
}: TimeSeriesChartProps) => {
  const [hover, setHover] = useState<number | null>(null)

  if (points.length < 2) {
    return (
      <p class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>
        {emptyHint ??
          'Collecting — the relay samples itself hourly, so the first points appear over the next few hours.'}
      </p>
    )
  }

  const values = points.map(p => p.value)
  const peak = Math.max(...values)
  // A truncated axis exaggerates growth, so default to anchoring at zero. For a
  // series that never approaches zero, pad below the trough instead of flat-lining.
  const trough = Math.min(...values)
  const floor = zeroBased ? 0 : Math.max(0, trough - (peak - trough) * 0.25)
  const span = peak - floor || 1

  const first = points[0].at
  const last = points[points.length - 1].at
  const timeSpan = last - first || 1

  const x = (at: number) => PAD_L + ((at - first) / timeSpan) * (W - PAD_L * 2)
  const y = (v: number) => H - PAD_B - ((v - floor) / span) * (H - PAD_B - 8)

  const line = points
    .map((p, i) => `${i === 0 ? 'M' : 'L'}${x(p.at).toFixed(1)},${y(p.value).toFixed(1)}`)
    .join(' ')
  const area = `${line} L${x(last).toFixed(1)},${H - PAD_B} L${x(first).toFixed(1)},${H - PAD_B} Z`

  const current = values[values.length - 1]
  const delta = current - values[0]
  const gradientId = `seriesFill-${label.replace(/[^a-z0-9]/gi, '')}`

  /** Nearest sample to the pointer, in data space rather than pixels. */
  const onMove = (e: MouseEvent) => {
    const target = e.currentTarget as SVGSVGElement
    const rect = target.getBoundingClientRect()
    if (rect.width === 0) return
    // The viewBox is stretched by preserveAspectRatio="none", so map the
    // pointer through the rendered width rather than assuming 1:1 with W.
    const vx = ((e.clientX - rect.left) / rect.width) * W
    const at = first + ((vx - PAD_L) / (W - PAD_L * 2)) * timeSpan

    let nearest = 0
    let best = Infinity
    points.forEach((p, i) => {
      const d = Math.abs(p.at - at)
      if (d < best) {
        best = d
        nearest = i
      }
    })
    setHover(nearest)
  }

  const point = hover === null ? null : points[hover]

  return (
    <div>
      <div style={{ position: 'relative' }}>
        <svg
          class="admin-storage-chart"
          viewBox={`0 0 ${W} ${H}`}
          preserveAspectRatio="none"
          role="img"
          aria-label={`${label} over time, currently ${format(current)}`}
          onMouseMove={onMove}
          onMouseLeave={() => setHover(null)}
        >
          <defs>
            <linearGradient id={gradientId} x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stop-color="var(--color-accent)" stop-opacity="0.22" />
              <stop offset="100%" stop-color="var(--color-accent)" stop-opacity="0" />
            </linearGradient>
          </defs>
          <line
            x1={PAD_L}
            y1={H - PAD_B}
            x2={W - PAD_L}
            y2={H - PAD_B}
            class="admin-storage-chart-axis"
          />
          <path d={area} fill={`url(#${gradientId})`} />
          <path d={line} class="admin-storage-chart-line" />
          {point && (
            <>
              <line
                x1={x(point.at)}
                y1={8}
                x2={x(point.at)}
                y2={H - PAD_B}
                class="admin-chart-cursor"
              />
              {/* vector-effect keeps the dot round despite the stretched viewBox */}
              <circle
                cx={x(point.at)}
                cy={y(point.value)}
                r="3"
                class="admin-chart-dot"
                vector-effect="non-scaling-stroke"
              />
            </>
          )}
        </svg>
        {point && (
          <div
            class="admin-chart-tooltip"
            // Clamped so the readout never hangs off either edge.
            style={{ left: `${Math.min(88, Math.max(2, (x(point.at) / W) * 100))}%` }}
          >
            <strong>{format(point.value)}</strong>
            <span>{formatUnix(point.at)}</span>
          </div>
        )}
      </div>
      <div class="admin-storage-chart-legend">
        <span>{formatUnix(first)}</span>
        <span>
          {format(current)} now
          {delta !== 0 && (
            <span style={{ color: delta > 0 ? '#eab308' : 'var(--color-accent)' }}>
              {' '}
              ({delta > 0 ? '+' : '−'}
              {format(Math.abs(delta))} over this window)
            </span>
          )}
        </span>
      </div>
    </div>
  )
}
