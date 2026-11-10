// The analytics' charts: one series of bars over a baseline, drawn at the width it is given (text stays legible on a
// phone), a tooltip over the bar under the pointer or finger, a table for screen readers; the days of a period with
// the empty ones filled in.
import { useLayoutEffect, useRef, useState } from 'react'
import { tz } from '../shared/zone.js'

export function periodWords(days) {
  return days === 365 ? 'год' : `${days} дней`
}

/** Every day of the period, oldest first: {key: «2026-11-07», name, d: that day's row or undefined}. */
export function everyDay(since, days, rows) {
  const by = Object.fromEntries(rows.map((d) => [d.day, d]))
  return Array.from({ length: days }, (_, k) => {
    // the noon of each day: an hour either way (the clocks changed) is still that day
    const iso = new Date((since + k * 86400 + 43200) * 1000).toLocaleDateString('en-CA', { timeZone: tz() })
    return { key: iso, name: dayLabel(iso), d: by[iso] }
  })
}

/** A day's tick under the bars: every day for a week, then sparser (fewer on a narrow chart). */
export function dayTick(series, days) {
  return (p, i, narrow) => {
    const every = days <= 7 ? 1 : days <= 30 ? (narrow ? 10 : 7) : days <= 90 ? (narrow ? 30 : 14) : narrow ? 91 : 61
    return i % every === (series.length - 1) % every
      ? new Date(`${p.key}T12:00:00Z`).toLocaleDateString('ru-RU', { timeZone: 'UTC', day: 'numeric', month: 'short' })
      : ''
  }
}

export function dayLabel(iso, withWeekday) {
  const d = new Date(`${iso}T12:00:00Z`)
  return d.toLocaleDateString('ru-RU', {
    timeZone: 'UTC',
    day: 'numeric',
    month: 'long',
    ...(withWeekday ? { weekday: 'short' } : {}),
  })
}

function useWidth() {
  const ref = useRef(null)
  const [w, setW] = useState(0)
  useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    setW(el.clientWidth)
    const ro = new ResizeObserver(([e]) => setW(Math.round(e.contentRect.width)))
    ro.observe(el)
    return () => ro.disconnect()
  }, [])
  return [ref, w]
}

/** One series of bars over a baseline. `describe(p)` → [title, details] for the tooltip and the table. */
export function Bars({ points, label, describe, tick, height = 200 }) {
  const [ref, W] = useWidth()
  const [hover, setHover] = useState(null)
  const max = Math.max(1, ...points.map((p) => p.v))
  const H = height
  const top = 10
  const bottom = 24
  const left = 32
  const narrow = W < 480
  const plot = Math.max(1, W - left)
  const step = plot / points.length
  const gap = step > 6 ? Math.min(4, step * 0.25) : 1
  const nice = Math.max(1, Math.ceil(max / 4))
  const gridVals = [0, 1, 2, 3, 4].map((k) => k * nice) // the top line is at or over the tallest bar
  const y = (v) => top + (H - top - bottom) * (1 - v / gridVals[4])
  const shown = hover != null ? points[hover] : null
  const [title, details] = shown ? describe(shown) : []
  return (
    <figure className="chart-box" ref={ref} style={{ margin: 0 }} onMouseLeave={() => setHover(null)}>
      {W > 0 && (
        <svg className="chart" width={W} height={H} viewBox={`0 0 ${W} ${H}`} role="img" aria-label={label}>
          {gridVals.map((v) => (
            <g key={v}>
              <line
                className={v ? 'grid' : 'base'}
                x1={left}
                x2={W}
                y1={y(v)}
                y2={y(v)}
                strokeDasharray={v ? '2 4' : undefined}
              />
              <text x={left - 8} y={y(v) + 4} textAnchor="end">
                {v}
              </text>
            </g>
          ))}
          {points.map((p, i) => {
            const x = left + i * step + gap / 2
            const w = Math.max(1, step - gap)
            const h = Math.max(0, y(0) - y(p.v))
            const r = Math.min(4, w / 2, h)
            const t = tick(p, i, narrow)
            return (
              <g key={p.key} onMouseEnter={() => setHover(i)} onClick={() => setHover(i)}>
                <rect className="hit" x={left + i * step} y={top} width={step} height={H - top - bottom + 4} />
                {h > 0 && (
                  <path
                    className={`bar${hover != null && hover !== i ? ' dim' : ''}`}
                    d={`M${x},${y(0)} v${-(h - r)} q0,${-r} ${r},${-r} h${w - 2 * r} q${r},0 ${r},${r} v${h - r} z`}
                  />
                )}
                {t && (
                  <text x={x + w / 2} y={H - 6} textAnchor="middle">
                    {t}
                  </text>
                )}
              </g>
            )
          })}
        </svg>
      )}
      {shown && (
        <div
          className="tip"
          style={{ left: Math.min(Math.max(left + (hover + 0.5) * step, 110), W - 110), top: y(shown.v) }}
          aria-hidden="true"
        >
          <b>{title}</b>
          {details && <span>{details}</span>}
        </div>
      )}
      <p className="sr-only" aria-live="polite">
        {shown ? `${title}: ${details}` : ''}
      </p>
      {/* in a box: a table's height is only its least, so one hidden by itself still stretched the page by its rows */}
      <div className="sr-only">
        <table>
          <caption>{label}</caption>
          <tbody>
            {points.map((p) => (
              <tr key={p.key}>
                <th scope="row">{p.name}</th>
                <td>{p.v}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </figure>
  )
}
