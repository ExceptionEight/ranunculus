// The pieces the pages share: a station's state as a dot and a word (its glyph is Flower.jsx), the switch, the segmented
// control, the volume (a slider, the blue tile), the music (the raspberry tile), a ring going on, a duration over the
// ring's stops, a sound picker, a dialog as a sheet, «loading», «did not load», an empty place.
import { useEffect, useId, useRef, useState } from 'react'
import { Link } from 'react-router'
import {
  BellRing,
  ChevronLeft,
  CircleHelp,
  Pause,
  Play,
  Repeat1,
  SkipBack,
  SkipForward,
  Square,
  Volume1,
  Volume2,
  VolumeX,
  X,
} from 'lucide-react'
import { api } from '../shared/api.js'
import { useAction, useNow } from '../shared/hooks.js'
import { deviceName } from '../shared/store.js'
import { active, level, nearestStop, RING_STOPS, soundTitle, span, trackTime, until } from '../shared/format.js'

export const devicePath = (id) => `/devices/${encodeURIComponent(id)}`

/** The dot before a station's state: blue — on, raspberry — ringing or playing, grey — off; it pulses while busy. */
export function Dot({ d }) {
  const tone = !d?.online ? 'gray' : d.ring || d.music?.state === 'play' ? 'rasp' : ''
  return <span className={`dot ${tone}${active(d) ? ' busy' : ''}`} aria-hidden="true" />
}

export function Switch({ on, onChange, label, disabled }) {
  return (
    <button
      type="button"
      role="switch"
      className="switch"
      aria-checked={!!on}
      aria-label={label}
      disabled={disabled}
      onClick={(e) => {
        e.stopPropagation()
        onChange(!on)
      }}
    />
  )
}

/** One of a few: `items` [[value, label, extra?]]. */
export function Seg({ items, value, onChange, label, className = '' }) {
  return (
    <div className={`seg ${className}`} role="group" aria-label={label}>
      {items.map(([v, name, extra]) => (
        <button key={v} type="button" aria-pressed={value === v} onClick={() => onChange(v)}>
          {name}
          {extra != null && <span className="count">{extra}</span>}
        </button>
      ))}
    </div>
  )
}

/** A page's head: a way back, a line over the title, the title, actions on the right. */
export function PageHead({ back, over, title, children }) {
  return (
    <header className="head">
      <div className="head-text">
        {back && (
          <Link to={back.to} className="back">
            <ChevronLeft size={18} aria-hidden="true" />
            {back.label}
          </Link>
        )}
        {over && <p className="over">{over}</p>}
        <h1>{title}</h1>
      </div>
      {children && <div className="head-acts">{children}</div>}
    </header>
  )
}

/** A «?» with a note beside it: a mouse shows it while over the «?» (or the note; leaving, it stays a moment, so that
 * a slanting way from the one to the other keeps it), a finger or a key opens and closes it, a tap elsewhere or Esc
 * closes it. The note hangs under the «?», its right edge at the «?»'s. */
export function Hint({ label, children }) {
  const [open, setOpen] = useState(false)
  const ref = useRef(null)
  const by = useRef('') // what pressed the «?»: a mouse has opened it already by being over it
  const id = useId()
  const leaving = useRef(null)
  useEffect(() => () => clearTimeout(leaving.current), [])
  useEffect(() => {
    if (!open) return
    const away = (e) => {
      if (!ref.current?.contains(e.target)) setOpen(false)
    }
    const esc = (e) => {
      if (e.key === 'Escape') setOpen(false)
    }
    document.addEventListener('pointerdown', away)
    document.addEventListener('keydown', esc)
    return () => {
      document.removeEventListener('pointerdown', away)
      document.removeEventListener('keydown', esc)
    }
  }, [open])
  return (
    <span
      className="hint"
      ref={ref}
      onPointerEnter={(e) => {
        if (e.pointerType !== 'mouse') return
        clearTimeout(leaving.current)
        setOpen(true)
      }}
      onPointerLeave={(e) => {
        if (e.pointerType !== 'mouse') return
        clearTimeout(leaving.current)
        leaving.current = setTimeout(() => setOpen(false), 300)
      }}
    >
      <button
        type="button"
        className="btn icon"
        aria-label={label}
        aria-expanded={open}
        aria-controls={id}
        onPointerDown={(e) => (by.current = e.pointerType)}
        onClick={() => {
          if (by.current === 'mouse') setOpen(true)
          else setOpen((o) => !o)
          by.current = ''
        }}
      >
        <CircleHelp size={20} aria-hidden="true" />
      </button>
      {open && (
        <div className="hint-pop" id={id} role="note" aria-label={label}>
          <div className="hint-card">
            <b className="hint-title">{label}</b>
            {children}
          </div>
        </div>
      )}
    </span>
  )
}

export function Empty({ icon: Icon, title, children, small }) {
  return (
    <div className={`empty${small ? ' sm' : ''}`}>
      {Icon && (
        <span className={`ico${small ? '' : ' lg'}`} aria-hidden="true">
          <Icon size={small ? 20 : 26} />
        </span>
      )}
      {title && <b>{title}</b>}
      {children && <p>{children}</p>}
    </div>
  )
}

/** Something is on its way from the server. */
export function Loading() {
  return (
    <div className="loading" role="status" aria-label="Загрузка">
      <span className="spinner" />
    </div>
  )
}

/** It did not come: why, in the server's words. */
export function Failed({ error }) {
  return (
    <p className="notice" role="alert">
      Не удалось загрузить: {error}
    </p>
  )
}

// ---------------------------------------------------------------- dialogs

/** A dialog: a sheet from the bottom on a phone, a card in the middle on a wide screen. A form when `onSubmit`. */
export function Sheet({ title, onClose, onSubmit, children, foot }) {
  const ref = useRef(null)
  const id = useId()
  // where the press began: text selected in a field and let go over the backdrop sends the click to the dialog too
  const down = useRef(null)
  useEffect(() => {
    // no close() on cleanup: under StrictMode its `close` event would end the dialog it just opened; a dialog taken
    // out of the page closes anyway
    if (ref.current && !ref.current.open) ref.current.showModal()
  }, [])
  const Body = onSubmit ? 'form' : 'div'
  return (
    <dialog
      ref={ref}
      className="sheet"
      aria-labelledby={id}
      onClose={onClose}
      onPointerDown={(e) => (down.current = e.target)}
      onClick={(e) => {
        if (e.target === ref.current && down.current === ref.current) onClose()
        down.current = null
      }}
    >
      <Body
        style={{ display: 'contents' }}
        onSubmit={
          onSubmit &&
          ((e) => {
            e.preventDefault()
            onSubmit()
          })
        }
      >
        <div className="sheet-head">
          <h2 id={id}>{title}</h2>
          <button type="button" className="btn ghost icon sm" onClick={onClose} aria-label="Закрыть">
            <X size={20} aria-hidden="true" />
          </button>
        </div>
        <div className="sheet-body">{children}</div>
        {foot && <div className="sheet-foot">{foot}</div>}
      </Body>
    </dialog>
  )
}

// ---------------------------------------------------------------- the volume

/** The volume, 0 … 10 in whole steps (a half set by voice is shown as it is), sent when let go; the server's word
 * moves it unless it is being changed here. */
function useVolume(d) {
  const remote = d.volume?.level
  const [v, setV] = useState(remote ?? 0)
  const latest = useRef(v) // what is sent when let go: set at once, not on the next render
  const moving = useRef(false)
  useEffect(() => {
    if (!moving.current && remote != null) {
      setV(remote)
      latest.current = remote
    }
  }, [remote])
  const back = () => {
    moving.current = false
    setV(remote ?? 0)
    latest.current = remote ?? 0
  }
  const [send, , error] = useAction(async (lvl) => {
    try {
      await api.setVolume(d.id, lvl)
    } catch (e) {
      back()
      throw e
    }
  })
  const move = (x) => {
    moving.current = true
    latest.current = x
    setV(x)
  }
  const commit = () => {
    if (!moving.current) return
    moving.current = false
    if (latest.current !== remote) send(latest.current)
  }
  return { v, move, commit, back, moving, error, known: remote != null, off: !d.online || remote == null }
}

/** A small volume control: for a station's card. */
export function VolumeSlider({ d }) {
  const { v, move, commit, error, known, off } = useVolume(d)
  const Icon = v === 0 ? VolumeX : v < 5 ? Volume1 : Volume2
  return (
    <div>
      <div className="slide">
        <Icon size={20} aria-hidden="true" />
        <input
          className="range"
          type="range"
          min={0}
          max={10}
          step={1}
          value={v}
          disabled={off}
          aria-label={`Громкость: ${deviceName(d)}`}
          aria-valuetext={`${level({ level: v })} из 10`}
          style={{ '--pos': `${v * 10}%` }}
          onChange={(e) => move(Number(e.target.value))}
          onPointerUp={commit}
          onKeyUp={commit}
          onBlur={commit}
        />
        <output aria-live="polite">{known ? level({ level: v }) : ''}</output>
      </div>
      {error && <p className="err">{error}</p>}
    </div>
  )
}

const KEYS = { ArrowRight: 1, ArrowUp: 1, PageUp: 1, ArrowLeft: -1, ArrowDown: -1, PageDown: -1 }

/** The blue tile: the volume big and ten bars, one a level, which are the control: a tap or a slide along them sets
 * it (the leftmost edge is silence), arrows from the keyboard. A finger moving up or down scrolls the page instead. */
export function VolumeCard({ d, className = '' }) {
  const { v, move, commit, back, moving, error, known, off } = useVolume(d)
  const ref = useRef(null)
  const at = (e) => {
    const r = ref.current.getBoundingClientRect()
    const x = ((e.clientX - r.left) / r.width) * 10
    return x < 0.35 ? 0 : Math.min(10, Math.max(1, Math.ceil(x)))
  }
  return (
    <section className={`hero blue ${className}`} aria-label="Громкость">
      <div className="vol-head">
        <span className="vol-label">Громкость</span>
        <span className="vol-num num" aria-hidden="true">
          {known ? level({ level: v }) : ''}
        </span>
      </div>
      <div
        ref={ref}
        className="vol-bars"
        role="slider"
        tabIndex={off ? -1 : 0}
        aria-label="Громкость"
        aria-valuemin={0}
        aria-valuemax={10}
        aria-valuenow={v}
        aria-valuetext={`${level({ level: v })} из 10`}
        aria-disabled={off}
        onPointerDown={(e) => {
          if (off || e.button > 0) return
          e.currentTarget.setPointerCapture(e.pointerId)
          if (e.pointerType === 'mouse') move(at(e)) // a finger waits: it may be scrolling the page
        }}
        onPointerMove={(e) => !off && e.currentTarget.hasPointerCapture(e.pointerId) && move(at(e))}
        onPointerUp={(e) => {
          if (off) return
          if (!moving.current) move(at(e)) // a tap
          commit()
        }}
        onPointerCancel={() => moving.current && back()}
        onKeyDown={(e) => {
          if (off) return
          const n =
            e.key === 'Home'
              ? 0
              : e.key === 'End'
                ? 10
                : KEYS[e.key] != null
                  ? Math.min(10, Math.max(0, Math.round(v) + KEYS[e.key]))
                  : null
          if (n == null) return
          e.preventDefault()
          move(n)
        }}
        onKeyUp={commit}
        onBlur={commit}
      >
        {Array.from({ length: 10 }, (_, i) => (
          <i
            key={i}
            className={i + 1 <= v ? 'lit' : i < v ? 'half' : undefined}
            style={{ height: `${20 + (i * 80) / 9}%` }}
          />
        ))}
      </div>
      {error && (
        <p className="sub" role="alert">
          {error}
        </p>
      )}
    </section>
  )
}

// ---------------------------------------------------------------- the music

/** The raspberry tile: the track, where it is (drag to seek; it moves on by itself while it sounds), its keys.
 * `named`: the station's name on it (on the home page, where several may play). */
export function PlayerCard({ d, named, className = '' }) {
  const m = d.music
  const [act, busy, error] = useAction((a, extra) => api.music(d.id, a, extra))
  const [title, artist] = m.title != null ? [m.title, m.artist] : (m.track || '').split(' — ')
  const playing = m.state === 'play'
  // the place as told, and when it came: moved on by the clock while a stream sounds
  const [base, setBase] = useState(() => ({ m, at: performance.now() }))
  const [now, setNow] = useState(() => performance.now())
  // the slider while dragged (and after, till the player says where it went): the server's place does not move it
  const [drag, setDrag] = useState(null)
  const dragging = useRef(false)
  const giveUp = useRef(null)
  useEffect(() => () => clearTimeout(giveUp.current), [])
  useEffect(() => {
    setBase({ m, at: performance.now() })
    if (!dragging.current) setDrag(null)
  }, [m])
  useEffect(() => {
    if (!m.live) return
    const t = setInterval(() => setNow(performance.now()), 500)
    return () => clearInterval(t)
  }, [m.live])
  const dur = m.duration || null
  const moved = base.m.live ? Math.max(0, now - base.at) / 1000 : 0
  const pos = Math.min(dur ?? Infinity, (base.m.pos || 0) + moved)
  const shown = drag ?? pos
  const commit = () => {
    if (!dragging.current || drag == null) return // once: pointerup, keyup and blur all end a drag
    dragging.current = false
    act('seek', { to: Math.round(drag) })
    clearTimeout(giveUp.current)
    giveUp.current = setTimeout(() => setDrag(null), 3000) // the player's word never came: back to its place
  }
  const at = dur ? Math.min(shown, dur) : 0
  return (
    <section className={`hero rasp ${className}`} aria-label={`Музыка: ${deviceName(d)}`}>
      <div className="hero-top">
        {named ? (
          <Link className="pill" to={devicePath(d.id)}>
            {deviceName(d)}
            {!playing && ' · пауза'}
          </Link>
        ) : (
          <span className="pill">{playing ? 'Сейчас играет' : 'Пауза'}</span>
        )}
        <div className="hero-keys" style={{ gap: 8 }}>
          <button
            type="button"
            className="round sm"
            disabled={busy}
            aria-pressed={!!m.repeat}
            onClick={() => act(m.repeat ? 'repeat_off' : 'repeat')}
            aria-label="Повторять трек"
            title={m.repeat ? 'Повтор включён' : 'Повторять трек'}
          >
            <Repeat1 size={18} aria-hidden="true" />
          </button>
        </div>
      </div>
      <div className="hero-text">
        <h2>{title || 'Без названия'}</h2>
        {artist && <p className="sub">{artist}</p>}
      </div>
      <div className="seek">
        <input
          className="range thin"
          type="range"
          min={0}
          max={dur || 1}
          step={1}
          value={at}
          disabled={!dur}
          aria-label="Место в треке"
          aria-valuetext={dur ? `${trackTime(shown)} из ${trackTime(dur)}` : trackTime(shown)}
          style={{ '--pos': `${dur ? (at / dur) * 100 : 0}%` }}
          onChange={(e) => {
            dragging.current = true
            clearTimeout(giveUp.current)
            setDrag(Number(e.target.value))
          }}
          onPointerUp={commit}
          onKeyUp={commit}
          onBlur={commit}
        />
        <div className="seek-times num">
          <span>{trackTime(shown)}</span>
          <span>{dur ? trackTime(dur) : ''}</span>
        </div>
      </div>
      <div className="hero-keys">
        <button
          type="button"
          className="round"
          disabled={busy}
          onClick={() => act('previous')}
          aria-label="Предыдущий трек"
          title="Предыдущий трек"
        >
          <SkipBack size={22} fill="currentColor" aria-hidden="true" />
        </button>
        <button
          type="button"
          className="round big"
          disabled={busy}
          onClick={() => act(playing ? 'pause' : 'resume')}
          aria-label={playing ? 'Пауза' : 'Продолжить'}
          title={playing ? 'Пауза' : 'Продолжить'}
        >
          {playing ? (
            <Pause size={28} fill="currentColor" aria-hidden="true" />
          ) : (
            <Play size={28} fill="currentColor" aria-hidden="true" />
          )}
        </button>
        <button
          type="button"
          className="round"
          disabled={busy}
          onClick={() => act('next')}
          aria-label="Следующий трек"
          title="Следующий трек"
        >
          <SkipForward size={22} fill="currentColor" aria-hidden="true" />
        </button>
      </div>
      {error && (
        <p className="sub" role="alert">
          {error}
        </p>
      )}
    </section>
  )
}

// ---------------------------------------------------------------- a ring going on

/** A ring's title: «Будильник 07:30 (работа)», «Таймер на 10 мин», «Проверка звука». */
export function ringTitle(ring) {
  if (ring?.kind === 'test') return 'Проверка звука'
  const s = ring?.what || (ring?.kind === 'alarm' ? 'будильник' : 'таймер')
  return s[0].toUpperCase() + s.slice(1)
}

/** A ring going on: what, till when, and the one button that matters. Its own clock: only this re-renders. */
export function RingingCard({ d, named, className = '' }) {
  const now = useNow(1000)
  const [stop, busy, error] = useAction(() => api.stopRing(d.id))
  return (
    <section className={`hero rasp ringing ${className}`} role="alert">
      <div className="hero-top">
        <span className="pill">
          <BellRing size={16} className="ring-icon" aria-hidden="true" />
          {named ? deviceName(d) : 'Звонок'}
        </span>
      </div>
      <div className="hero-text">
        <h2>{ringTitle(d.ring)}</h2>
        {d.ring.until > now && <p className="sub">Затихнет {until(d.ring.until, now)}</p>}
      </div>
      <div>
        <button type="button" className="btn white tall" disabled={busy} onClick={() => stop()}>
          <Square size={16} fill="currentColor" aria-hidden="true" />
          Выключить
        </button>
      </div>
      {error && (
        <p className="sub" role="alert">
          {error}
        </p>
      )}
    </section>
  )
}

// ---------------------------------------------------------------- sounds

/** How long something rings: the stops 3 s … 10 min; `onCommit` when let go. */
export function Duration({ value, onCommit, label, disabled }) {
  const [i, setI] = useState(nearestStop(value))
  const moving = useRef(false)
  useEffect(() => {
    if (!moving.current) setI(nearestStop(value))
  }, [value])
  const commit = () => {
    if (!moving.current) return
    moving.current = false
    if (RING_STOPS[i] !== value) onCommit(RING_STOPS[i])
  }
  return (
    <div className="slide" style={{ width: 300, maxWidth: '100%' }}>
      <input
        className="range"
        type="range"
        min={0}
        max={RING_STOPS.length - 1}
        step={1}
        value={i}
        disabled={disabled}
        aria-label={label}
        aria-valuetext={span(RING_STOPS[i])}
        style={{ '--pos': `${(i / (RING_STOPS.length - 1)) * 100}%` }}
        onChange={(e) => {
          moving.current = true
          setI(Number(e.target.value))
        }}
        onPointerUp={commit}
        onKeyUp={commit}
        onBlur={commit}
      />
      <output style={{ minWidth: '5.6em' }}>{span(RING_STOPS[i])}</output>
    </div>
  )
}

/** A sound from the list: the built-in and the user's; broken ones are not offered. `inherit`: an option for «as in
 * the settings» (value null). */
export function SoundSelect({ sounds, value, onChange, id, inherit, disabled }) {
  const playable = (sounds || []).filter((s) => !s.error)
  const known = playable.some((s) => s.name === value)
  return (
    <select
      id={id}
      className="field"
      value={value ?? ''}
      disabled={disabled}
      onChange={(e) => onChange(e.target.value || null)}
    >
      {inherit && <option value="">{inherit}</option>}
      {playable.map((s) => (
        <option key={s.name} value={s.name}>
          {soundTitle(s.name)}
        </option>
      ))}
      {value && !known && <option value={value}>{soundTitle(value)}</option>}
    </select>
  )
}

/** A listen button: the sound once, or as it rings for `seconds`. */
export function Listen({ preview, name, seconds, children }) {
  const on = preview.isPlaying(name, seconds ?? null)
  return (
    <button type="button" className="btn sm" onClick={() => preview.play(name, seconds ?? null)} aria-pressed={on}>
      {on ? (
        <Square size={14} fill="currentColor" aria-hidden="true" />
      ) : (
        <Play size={15} fill="currentColor" aria-hidden="true" />
      )}
      {on ? 'Остановить' : children || 'Послушать'}
    </button>
  )
}
