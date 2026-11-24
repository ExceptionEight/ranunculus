// Автоматизация: the record of what was put off («выключи музыку через полчаса») — all there ever was, by when each
// was due, the latest first (what still waits is on top): what it was, on which station, how it ended, and why not if
// it did not run. The server deletes nothing of it. Read by pages; what changes meanwhile comes with the `later` event
// (the main page's list): the first page is then read again, its rows taking the place of the old ones.
// `LaterItem` is a row of it — the main page's too, where one that is over can be put out of sight.
import { useEffect, useRef, useState } from 'react'
import { Link } from 'react-router'
import { CalendarClock, X } from 'lucide-react'
import { api } from '../shared/api.js'
import { useAction, useLoad, useNow } from '../shared/hooks.js'
import { deviceName, useStore } from '../shared/store.js'
import { clock, day, ringDay, sameDay, untilAbout } from '../shared/format.js'
import { devicePath, Empty, Failed, Loading, PageHead } from './ui.jsx'

const ENDED = { done: 'выполнено', failed: 'не выполнено', missed: 'не выполнено', cancelled: 'отменено' }

/** «выключить музыку» → «Выключить музыку» */
function capital(s) {
  return s ? s[0].toUpperCase() + s.slice(1) : ''
}

/** A step for a device of the home, the control and what it is set to: «Нагрев: режим – Кипячение» (a number is not
 * torn from its unit, the dash does not begin a line). */
function stepWords(s) {
  return s.value ? `${s.what}\u00a0– ${s.value.replace(/(\d) /g, '$1\u00a0')}` : s.what
}

/** One thing put off: waiting — when it is due, cancelled by its button; over — how it ended, and why not if it did
 * not run. For a device of the home: its name, then a line for each step. `main`: on the main page, where one that is
 * over has a button too: out of sight there (the record keeps it), and its day is said when it is not today (the
 * record has the days as headings). */
export function LaterItem({ e, devices, main }) {
  const now = useNow(1000)
  const [remove, busy, error] = useAction(() => api.removeLater(e.id))
  const waits = e.state === 'wait'
  const today = sameDay(e.at, now)
  const station = <Link to={devicePath(e.device)}>{deviceName(devices[e.device]) || e.device}</Link>
  const label = waits ? 'Отменить' : 'Убрать с главной'
  return (
    <li className={`agenda-item put-off${waits ? '' : ' over'}`}>
      <span className="when">{clock(e.at)}</span>
      <span className="what">
        <b>{e.name || capital(e.what)}</b>
        {e.name &&
          e.steps.map((s, i) => (
            <span key={i} className="step">
              {stepWords(s)}
            </span>
          ))}
        {waits ? (
          <span>
            {station} · {today ? untilAbout(e.at, now) : main ? ringDay(e.at, now) : 'ждёт'}
          </span>
        ) : (
          <span>
            {station} · {ENDED[e.state] || e.state}
            {main && !today ? `, ${day(e.at, now).toLowerCase()}` : ''}
          </span>
        )}
        {(e.state === 'failed' || e.state === 'missed') && (
          <span className="err">{capital(e.error) || 'Причина неизвестна'}</span>
        )}
        {error && <span className="err">{error}</span>}
      </span>
      {(waits || main) && (
        <button
          type="button"
          className="btn ghost icon sm"
          disabled={busy}
          onClick={() => remove()}
          aria-label={label}
          title={label}
        >
          <X size={16} aria-hidden="true" />
        </button>
      )}
    </li>
  )
}

/** «Завтра», «Сегодня», «Вчера», «5 ноября» */
function dayOf(t, now) {
  return sameDay(t, now + 86400) ? 'Завтра' : day(t, now)
}

/** The first page read again: its rows over the old ones, the new ones in their places. */
function merged(p, fresh) {
  const now = new Map(fresh.items.map((x) => [x.id, x]))
  const had = new Set(p.items.map((x) => x.id))
  const items = [...fresh.items.filter((x) => !had.has(x.id)), ...p.items.map((x) => now.get(x.id) || x)]
  items.sort((a, b) => b.at - a.at || b.id - a.id)
  return { items, more: p.items.length > fresh.items.length ? p.more : fresh.more }
}

export default function Later() {
  const devices = useStore((s) => s.devices)
  const live = useStore((s) => s.later)
  const now = useNow(60000)
  const { data: page, error, set } = useLoad(() => api.laterHistory(), [])
  const [moreError, setMoreError] = useState(null)

  // something of it changed (a new one, one that ended): the first page again
  const seen = useRef(live)
  useEffect(() => {
    if (seen.current === live) return undefined
    seen.current = live
    let late = false
    api
      .laterHistory()
      .then((r) => late || set((p) => merged(p, r)))
      .catch(() => {})
    return () => {
      late = true
    }
  }, [live, set])

  // the next page: one at a time (a double click would bring the same page twice)
  const next = useRef(false)
  const [extending, setExtending] = useState(false)
  const showMore = async () => {
    const items = page?.items
    if (!items?.length || next.current) return
    next.current = true
    setExtending(true)
    setMoreError(null)
    try {
      const r = await api.laterHistory({ before: items[items.length - 1].id })
      set((p) => {
        const had = new Set(p.items.map((x) => x.id))
        return { items: [...p.items, ...r.items.filter((x) => !had.has(x.id))], more: r.more }
      })
    } catch (e) {
      setMoreError(e.message)
    } finally {
      next.current = false
      setExtending(false)
    }
  }

  const items = page?.items || []
  return (
    <main className="page">
      <PageHead
        back={{ to: '/', label: 'Главная' }}
        title="Автоматизация"
        sub="Отложенные команды: всё, что было и что ждёт"
      />
      {error && <Failed error={error} />}
      {!page && !error && <Loading />}
      {page && !items.length && (
        <div className="card">
          <Empty icon={CalendarClock} title="Истории пока нет">
            Скажите колонке «выключи музыку через полчаса» или «включи чайник в семь утра», и команда появится здесь.
          </Empty>
        </div>
      )}
      {items.length > 0 && (
        <section className="card flat" aria-label="Отложенные команды">
          <ul className="agenda">
            {items.map((e, i) => (
              <Day key={e.id} e={e} prev={items[i - 1]} now={now} devices={devices} />
            ))}
          </ul>
        </section>
      )}
      {moreError && <p className="err">{moreError}</p>}
      {page?.more && (
        <div>
          <button type="button" className="btn" disabled={extending} onClick={showMore}>
            Показать ещё
          </button>
        </div>
      )}
    </main>
  )
}

function Day({ e, prev, now, devices }) {
  const head = !prev || !sameDay(prev.at, e.at)
  return (
    <>
      {head && <li className="list-day">{dayOf(e.at, now)}</li>}
      <LaterItem e={e} devices={devices} />
    </>
  )
}
