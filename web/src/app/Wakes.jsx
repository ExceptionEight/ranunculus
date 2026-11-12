// A station's wakes, each recorded: the 2 s before it and the command after. Each may be marked right (a tick) or
// false (a cross): the false ones become negatives to retrain the wake word with (wakes.rs).
import { useCallback, useEffect, useRef, useState } from 'react'
import { AudioLines, Check, Pause, Play, Trash2, X } from 'lucide-react'
import { api } from '../shared/api.js'
import { useLoad } from '../shared/hooks.js'
import { onWake } from '../shared/store.js'
import { clock, day, sameDay } from '../shared/format.js'
import { Empty, Failed, Loading, Seg } from './ui.jsx'

const FILTERS = [
  ['', 'Все'],
  ['none', 'Без отметки'],
  ['not', 'Ложные'],
  ['me', 'Верные'],
]

let playing = null // one recording at a time

function Item({ w, onChange, onDelete }) {
  const [on, setOn] = useState(false)
  const [error, setError] = useState(null)
  const audio = useRef(null)
  useEffect(() => () => audio.current?.pause(), [])
  const play = () => {
    if (on) {
      audio.current?.pause()
      setOn(false)
      return
    }
    playing?.()
    const a = new Audio(`/api/wakes/${w.id}/audio`)
    audio.current = a
    a.onended = () => setOn(false)
    a.play().then(
      () => setOn(true),
      () => setError('Запись не воспроизводится'),
    )
    playing = () => {
      a.pause()
      setOn(false)
    }
  }
  const mark = async (label) => {
    setError(null)
    try {
      onChange(await api.markWake(w.id, w.label === label ? null : label))
    } catch (e) {
      setError(e.message)
    }
  }
  const command = w.heard == null ? '…' : w.heard === '' ? 'тишина' : `«${w.heard}»`
  // the window is recognised only by recognition at home, right after the command: none a minute on, none coming
  const late = w.heard != null && Date.now() / 1000 - w.t > 60
  const word = w.heard_window == null ? (late ? null : '…') : w.heard_window ? `«${w.heard_window}»` : 'не разобрано'
  return (
    <li className="wake">
      <button
        type="button"
        className="play-btn"
        onClick={play}
        aria-pressed={on}
        aria-label={on ? 'Остановить' : `Послушать срабатывание в ${clock(w.t)}`}
      >
        {on ? (
          <Pause size={18} fill="currentColor" aria-hidden="true" />
        ) : (
          <Play size={18} fill="currentColor" aria-hidden="true" />
        )}
      </button>
      <div className="wake-text">
        <b>
          {clock(w.t)}
          <span className="faint" style={{ fontWeight: 400 }}>
            {w.button ? ' · кнопка' : ` · уверенность ${w.p.toFixed(2).replace('.', ',')}`}
            {w.barge ? ' · во время ответа' : ''}
          </span>
        </b>
        {word && <span>Слово: {word}</span>}
        <span>Команда: {command}</span>
        {error && <span className="err">{error}</span>}
      </div>
      <div className="wake-acts" role="group" aria-label="Отметка">
        <button
          type="button"
          className="mark me"
          aria-pressed={w.label === 'me'}
          onClick={() => mark('me')}
          aria-label="Верное срабатывание"
          title="Верно: слово сказали колонке"
        >
          <Check size={18} strokeWidth={2.4} aria-hidden="true" />
        </button>
        <button
          type="button"
          className="mark not"
          aria-pressed={w.label === 'not'}
          onClick={() => mark('not')}
          aria-label="Ложное срабатывание"
          title="Ложное: колонка сработала зря"
        >
          <X size={18} strokeWidth={2.4} aria-hidden="true" />
        </button>
        <button
          type="button"
          className="btn ghost icon sm"
          onClick={() => onDelete(w.id)}
          aria-label="Удалить запись"
          title="Удалить запись"
        >
          <Trash2 size={17} aria-hidden="true" />
        </button>
      </div>
    </li>
  )
}

/** The counts moved: a wake out of its old label (undefined: it was not counted) and into its new one (undefined: gone). */
function shift(counts, from, to) {
  const c = { ...counts }
  const move = (label, by) => {
    const k = label ?? 'none'
    c[k] = (c[k] || 0) + by
    c.all = (c.all || 0) + by
  }
  if (from !== undefined) move(from, -1)
  if (to !== undefined) move(to, 1)
  return c
}

export default function Wakes({ d }) {
  const [filter, setFilter] = useState('')
  // the list for this filter, {items, more, counts}; the counts are the device's, whatever the filter
  const {
    data: page,
    error,
    set,
    last,
  } = useLoad(() => api.wakes({ device: d.id, label: filter || undefined }), [d.id, filter])
  const [actError, setActError] = useState(null)
  const pageRef = useRef(page) // for the events: what the list has now
  pageRef.current = page
  const gone = useRef(new Set()) // deleted here: their event changes nothing more

  // the next page: one at a time, into the list it was asked for (a double click would bring the same page twice —
  // the same wakes, the same keys — and a filter switched meanwhile would get the old one's page)
  const next = useRef(null)
  const [extending, setExtending] = useState(false)
  useEffect(
    () => () => {
      next.current = null
      setExtending(false)
    },
    [d.id, filter],
  )
  const showMore = async () => {
    const items = page?.items
    if (!items?.length || next.current) return
    const token = {}
    next.current = token
    setExtending(true)
    setActError(null)
    try {
      const r = await api.wakes({ device: d.id, label: filter || undefined, before: items[items.length - 1].id })
      if (next.current !== token) return
      set((p) => {
        const have = new Set(p.items.map((x) => x.id))
        return {
          ...p,
          items: [...p.items, ...r.items.filter((x) => !have.has(x.id) && !gone.current.has(x.id))],
          more: r.more,
        }
      })
    } catch (e) {
      if (next.current === token) setActError(e.message)
    } finally {
      if (next.current === token) {
        next.current = null
        setExtending(false)
      }
    }
  }

  // A wake as it is now — an event, the answer to a mark, a deletion — into the list and the counts. One the list
  // has: from its old label to the new. One newer than
  // everything (the unfiltered list knows): counted in. Any other the list does not have (another filter's, not loaded
  // yet, deleted elsewhere): its old label is not known here — the counts are asked for again. A wake that fits the
  // filter goes where it belongs, if that part of the list is loaded.
  const isNewest = useCallback(
    (p, w) => !w.deleted && !filter && (p.items.length ? w.id > p.items[0].id : !p.more),
    [filter],
  )
  const absorb = useCallback(
    (p, w) => {
      const i = p.items.findIndex((x) => x.id === w.id)
      if (w.deleted)
        return i < 0
          ? p
          : { ...p, items: p.items.filter((x) => x.id !== w.id), counts: shift(p.counts, p.items[i].label) }
      if (i >= 0)
        return {
          ...p,
          items: p.items.map((x) => (x.id === w.id ? w : x)),
          counts: shift(p.counts, p.items[i].label, w.label),
        }
      const fits = !filter || (filter === 'none' ? w.label == null : w.label === filter)
      let at = p.items.findIndex((x) => x.id < w.id)
      if (at < 0) at = p.more ? -1 : p.items.length
      const items = fits && at >= 0 ? [...p.items.slice(0, at), w, ...p.items.slice(at)] : p.items
      return { ...p, items, counts: isNewest(p, w) ? shift(p.counts, undefined, w.label) : p.counts }
    },
    [filter, isNewest],
  )
  const recount = useRef({ timer: null, seq: 0 })
  useEffect(() => {
    const r = recount.current
    return () => {
      clearTimeout(r.timer)
      r.seq++
    }
  }, [d.id, filter])
  const take = useCallback(
    (w) => {
      const p = pageRef.current
      if (!p) return // the list is on its way: its answer brings the counts
      if (!w.deleted && w.device !== d.id) return
      const known = p.items.some((x) => x.id === w.id)
      if (w.deleted && !known && gone.current.has(w.id)) return
      set((q) => absorb(q, w))
      if (known || isNewest(p, w)) return
      const r = recount.current
      clearTimeout(r.timer)
      const my = ++r.seq
      r.timer = setTimeout(async () => {
        try {
          const { counts } = await api.wakes({ device: d.id, limit: 1 })
          if (my === r.seq) set((q) => ({ ...q, counts }))
        } catch {
          /* the counts stay as they were */
        }
      }, 300)
    },
    [d.id, set, absorb, isNewest],
  )
  // live: the event stream carries every wake recorded or changed
  useEffect(() => onWake(take), [take])

  const remove = async (id) => {
    setActError(null)
    gone.current.add(id)
    try {
      await api.deleteWake(id)
      take({ id, deleted: true })
    } catch (e) {
      gone.current.delete(id)
      setActError(e.message)
    }
  }
  const items = page?.items || []
  // while another filter's list loads, the counts stay on the buttons: they are the device's, not the list's
  const c = (page ?? last?.data)?.counts
  return (
    <>
      <div className="toolbar">
        <Seg
          label="Какие показывать"
          items={FILTERS.map(([v, name]) => [v, name, c ? ((v === '' ? c.all : c[v]) ?? 0) : null])}
          value={filter}
          onChange={setFilter}
        />
      </div>
      <p className="faint" style={{ padding: '0 4px', fontSize: 14 }}>
        Здесь можно послушать разговоры и разметить верные и неверные срабатывания для дообучения модели.
      </p>
      {error && <Failed error={error} />}
      {actError && <p className="err">{actError}</p>}
      {!page && !error && <Loading />}
      {page && !items.length && (
        <div className="card">
          <Empty icon={AudioLines} title={filter ? 'Ничего не найдено' : 'Срабатываний пока нет'}>
            {filter ? null : 'Запись появится здесь, когда колонка услышит слово активации.'}
          </Empty>
        </div>
      )}
      {items.length > 0 && (
        <section className="card flat" aria-label="Срабатывания">
          <ul style={{ listStyle: 'none', margin: 0, padding: 0 }}>
            {items.map((w, i) => (
              <WakeWithDay key={w.id} w={w} prev={items[i - 1]} onChange={take} onDelete={remove} />
            ))}
          </ul>
        </section>
      )}
      {page?.more && (
        <div>
          <button type="button" className="btn" disabled={extending} onClick={showMore}>
            Показать ещё
          </button>
        </div>
      )}
    </>
  )
}

function WakeWithDay({ w, prev, onChange, onDelete }) {
  const head = !prev || !sameDay(prev.t, w.t)
  return (
    <>
      {head && <li className="list-day">{day(w.t)}</li>}
      <Item w={w} onChange={onChange} onDelete={onDelete} />
    </>
  )
}
