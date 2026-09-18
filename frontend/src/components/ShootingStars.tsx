import { useRef, useEffect } from 'preact/hooks'

interface Star {
  x: number
  y: number
  len: number
  speed: number
  opacity: number
  active: boolean
  timer: number
  interval: number
}

interface ShootingStarsProps {
  contained?: boolean
  count?: number
}

export const ShootingStars = ({ contained = false, count = 5 }: ShootingStarsProps) => {
  const canvasRef = useRef<HTMLCanvasElement>(null)

  useEffect(() => {
    const canvas = canvasRef.current
    if (!canvas) return

    const ctx = canvas.getContext('2d')
    if (!ctx) return

    // Canvas takes colour strings, not CSS -- `var()` means nothing to
    // fillStyle -- so resolve the theme accent once here. Read rather than
    // hardcoded, otherwise the trails stay green on a differently-tinted relay
    // while the rest of the page recolours.
    const accentRgb =
      getComputedStyle(document.documentElement)
        .getPropertyValue('--color-accent-rgb')
        .trim() || '180, 249, 83'

    let animId: number
    const stars: Star[] = []
    const STAR_COUNT = count
    const ANGLE = (155 * Math.PI) / 180
    const cosA = Math.cos(ANGLE)
    const sinA = Math.sin(ANGLE)

    const parent = contained ? canvas.parentElement : null

    function resize() {
      if (contained && parent) {
        canvas!.width = parent.clientWidth
        canvas!.height = parent.clientHeight
      } else {
        canvas!.width = window.innerWidth
        canvas!.height = window.innerHeight
      }
    }
    resize()

    let ro: ResizeObserver | null = null
    if (contained && parent && typeof ResizeObserver !== 'undefined') {
      ro = new ResizeObserver(() => resize())
      ro.observe(parent)
    } else {
      window.addEventListener('resize', resize)
    }

    function resetStar(s: Star) {
      s.active = false
      s.timer = 0
      s.interval = 3000 + Math.random() * 8000
      s.x = Math.random() * canvas!.width * 0.8 + canvas!.width * 0.1
      s.y = Math.random() * canvas!.height * 0.6
      s.len = 60 + Math.random() * 80
      s.speed = 400 + Math.random() * 300
      s.opacity = 0
    }

    for (let i = 0; i < STAR_COUNT; i++) {
      const s: Star = { x: 0, y: 0, len: 0, speed: 0, opacity: 0, active: false, timer: 0, interval: 0 }
      resetStar(s)
      s.interval = Math.random() * 6000
      stars.push(s)
    }

    let lastTime = performance.now()

    function draw(now: number) {
      const dt = (now - lastTime) / 1000
      lastTime = now

      ctx!.clearRect(0, 0, canvas!.width, canvas!.height)

      for (const s of stars) {
        if (!s.active) {
          s.timer += dt * 1000
          if (s.timer >= s.interval) {
            s.active = true
            s.opacity = 0
          }
          continue
        }

        const dx = cosA * s.speed * dt
        const dy = sinA * s.speed * dt
        s.x += dx
        s.y += dy

        if (s.opacity < 0.9) {
          s.opacity = Math.min(s.opacity + dt * 4, 0.9)
        }

        if (s.x < -100 || s.x > canvas!.width + 100 || s.y > canvas!.height + 100) {
          resetStar(s)
          continue
        }

        const tailX = s.x - cosA * s.len
        const tailY = s.y - sinA * s.len

        const grad = ctx!.createLinearGradient(tailX, tailY, s.x, s.y)
        grad.addColorStop(0, `rgba(${accentRgb}, 0)`)
        grad.addColorStop(1, `rgba(${accentRgb}, ${s.opacity})`)

        ctx!.beginPath()
        ctx!.moveTo(tailX, tailY)
        ctx!.lineTo(s.x, s.y)
        ctx!.strokeStyle = grad
        ctx!.lineWidth = 1.5
        ctx!.stroke()

        ctx!.beginPath()
        ctx!.arc(s.x, s.y, 1.5, 0, Math.PI * 2)
        ctx!.fillStyle = `rgba(${accentRgb}, ${s.opacity})`
        ctx!.fill()
      }

      animId = requestAnimationFrame(draw)
    }

    animId = requestAnimationFrame(draw)

    return () => {
      cancelAnimationFrame(animId)
      if (ro) ro.disconnect()
      else window.removeEventListener('resize', resize)
    }
  }, [contained, count])

  return (
    <canvas
      ref={canvasRef}
      class={
        contained
          ? 'absolute inset-0 pointer-events-none w-full h-full'
          : 'fixed inset-0 pointer-events-none z-0'
      }
      aria-hidden="true"
    />
  )
}
