// The home's devices, from Home Assistant (the server's house/): on Главная, by room — a card a device, with what it
// does now and its main switch; and a device's own page, with every control it has: what it is for, how it is set up,
// what it reports about itself. A control is a switch, a number, a choice, a reading or an action, whatever the
// device is: a device's kind decides only its icon and the wording of its one line on the card.
import { useEffect, useRef, useState } from 'react'
import { Link, useParams } from 'react-router'
import {
  AirVent,
  Blinds,
  Bot,
  Box,
  ChevronRight,
  Coffee,
  Droplet,
  Droplets,
  Fan,
  Gamepad2,
  Gauge,
  Heater,
  Lightbulb,
  Lock,
  MonitorSpeaker,
  Plug,
  Power,
  ShieldAlert,
  Siren,
  Speaker,
  Sprout,
  Tv,
} from 'lucide-react'
import { api } from '../shared/api.js'
import { useStore } from '../shared/store.js'
import { tz } from '../shared/zone.js'
import { Empty, Loading, PageHead, Switch } from './ui.jsx'

const ICONS = {
  light: Lightbulb,
  switch: Power,
  outlet: Plug,
  water_heater: Heater,
  kettle: Coffee,
  climate: AirVent,
  humidifier: Droplets,
  fan: Fan,
  cover: Blinds,
  media_player: MonitorSpeaker,
  tv: Tv,
  speaker: Speaker,
  vacuum: Bot,
  mower: Sprout,
  lock: Lock,
  alarm: ShieldAlert,
  siren: Siren,
  valve: Droplet,
  remote: Gamepad2,
  sensor: Gauge,
}

const thingPath = (id) => `/house/${encodeURIComponent(id)}`

/** How many digits after the comma a step of a number asks for (0.5 → 1, 0.01 → 2; three at most). */
const decimals = (step) => (step >= 1 ? 0 : Math.min(3, String(step).split('.')[1]?.length ?? 0))

/** A number as it is read: by a control's step, else one digit (two under one: 0,02 is not «0»). */
function num(v, digits = Math.abs(v) < 1 ? 2 : 1) {
  return Number(v)
    .toLocaleString('ru-RU', { maximumFractionDigits: digits })
    .replace(/^[-−]0$/, '0')
}

/** A control's value as it is read: «29 °C», «41%», «Зелёный чай»; null: not known. */
export function shown(c) {
  const v = c.value
  if (v == null || v === '') return null
  if (c.options) return c.options.find((o) => o.value === v)?.label ?? String(v)
  // a moment (when a filter is due, when the sun rises): as a clock says it, in the home's time
  if (c.class === 'timestamp' && !Number.isNaN(Date.parse(v))) {
    return new Date(v).toLocaleString('ru-RU', {
      timeZone: tz(),
      day: 'numeric',
      month: 'long',
      hour: '2-digit',
      minute: '2-digit',
    })
  }
  if (typeof v !== 'number') return String(v)
  const n = num(v, c.kind === 'number' && c.step ? decimals(c.step) : undefined)
  if (!c.unit) return n
  return c.unit === '%' ? `${n}%` : `${n}\u00a0${c.unit}`
}

/** A device's main switch, if it has one. */
function mainSwitch(d) {
  return d.controls.find((c) => c.group === 'main' && c.kind === 'bool')
}

/** How a switch's two states are said, by what it switches. */
const STATES = { open: ['открыто', 'закрыто'], locked: ['заперто', 'не заперто'], playing: ['играет', 'пауза'] }
const stateWord = (c) => (STATES[c.id.split('#')[1]] || ['включено', 'выключено'])[c.value ? 0 : 1]

/** In the middle of a line: «Охлаждение» → «охлаждение», «HDMI 1» as it is. */
const inLine = (t) => (/^[А-ЯЁA-Z][а-яёa-z]/.test(t) ? t[0].toLowerCase() + t.slice(1) : t)

/** What a device does now, in a line: «Выключено», «29° · нагрев до 70°», «Охлаждение · до 22°», «Открыто на 40%». */
function summary(d) {
  if (!d.online) return 'Не в сети'
  const main = d.controls.filter((c) => c.group === 'main' && c.available && c.value != null)
  const by = (aspect) => main.find((c) => c.id.endsWith(`#${aspect}`))
  const reading = (cls) => main.find((c) => c.kind === 'reading' && c.class === cls && typeof c.value === 'number')
  const parts = []
  const [temp, wet, sw, target] = [reading('temperature'), reading('humidity'), mainSwitch(d), by('target')]
  if (temp) parts.push(`${num(temp.value)}°`)
  if (wet) parts.push(`${num(wet.value)}%`)
  if (d.kind === 'climate' && by('mode')) {
    parts.push(inLine(shown(by('mode'))))
    if (by('mode').value !== 'off' && target) parts.push(`до ${num(target.value)}°`)
  } else if (sw && sw.value != null) {
    const level = by('brightness') || by('speed')
    if (!sw.value) parts.push(stateWord(sw))
    else if (['kettle', 'water_heater'].includes(d.kind) && target) parts.push(`нагрев до ${num(target.value)}°`)
    else if (by('playing')?.value && by('title')) parts.push(`играет: ${by('title').value}`)
    else if (level?.value) parts.push(`${stateWord(sw)} · ${num(level.value)}%`)
    else parts.push(stateWord(sw))
  } else if (by('position')) {
    const p = by('position').value
    parts.push(p <= 0 ? 'закрыто' : p >= 100 ? 'открыто' : `открыто на ${num(p)}%`)
  }
  if (!parts.length) {
    const first = main.find((c) => c.kind !== 'action' && shown(c) != null)
    if (first) parts.push(inLine(shown(first)))
  }
  const line = parts.join(' · ')
  return line ? line[0].toUpperCase() + line.slice(1) : ''
}

/** The device does something now (its icon is lit): its main switch is on, a climate device is in a mode. */
function active(d) {
  if (!d.online) return false
  const sw = mainSwitch(d)
  if (sw) return !!sw.value
  const mode = d.kind === 'climate' && d.controls.find((c) => c.id.endsWith('#mode'))
  return !!mode && mode.value != null && mode.value !== 'off'
}

/** A control being set. What was asked for is shown at once and until the home says its own word (the server
 * answers when the device has confirmed; the event with the new state comes a moment before or after) — or until the
 * server refuses: then why, until the control changes. One request at a time: what is asked meanwhile waits, the
 * last of it only (a slider moved again, a switch turned back). → {value, set, busy, error}; `c` may be absent. */
function useSetting(c) {
  const [asked, setAsked] = useState(undefined)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState(null)
  const going = useRef(false)
  const next = useRef(undefined)
  const id = c?.id
  const value = c?.value
  useEffect(() => {
    setAsked(undefined)
    setError(null)
  }, [id, value])
  // no word from the home after the server's «done» (the value was that already): back to what the home says
  useEffect(() => {
    if (asked === undefined || busy) return
    const t = setTimeout(() => setAsked(undefined), 2000)
    return () => clearTimeout(t)
  }, [asked, busy])
  const set = async (v) => {
    setAsked(v)
    if (going.current) {
      next.current = v
      return
    }
    going.current = true
    setBusy(true)
    setError(null)
    try {
      for (let now = v; now !== undefined; now = next.current) {
        next.current = undefined
        await api.setControl(id, now)
      }
    } catch (e) {
      next.current = undefined
      setAsked(undefined)
      setError(e.message)
    } finally {
      going.current = false
      setBusy(false)
    }
  }
  return { value: asked === undefined ? value : asked, set, busy, error }
}

/** A card on Главная: what the device is, what it does now, and its main switch — unless that is a lock or a door
 * (what the voice may not set either): those are turned on the device's page, not by a stray tap. */
function ThingCard({ d, stale }) {
  const Icon = ICONS[d.kind] || Box
  const sw = mainSwitch(d)
  const { value, set, busy, error } = useSetting(sw)
  const line = summary(d)
  return (
    <article className={`card thing${d.online ? '' : ' off'}${active(d) ? ' on' : ''}`}>
      <Link to={thingPath(d.id)} className="station-link thing-link">
        <span className="thing-icon" aria-hidden="true">
          <Icon size={20} />
        </span>
        <span className="station-name">
          <b title={d.name}>{d.name}</b>
          {error ? (
            <span className="err" role="alert">
              {error}
            </span>
          ) : (
            <span title={line}>{line}</span>
          )}
        </span>
      </Link>
      {sw?.voice ? (
        <Switch
          on={d.online && !!value}
          disabled={stale || !d.online || !sw.available}
          busy={busy}
          onChange={(v) => set(v)}
          label={`${d.name}: ${sw.name}`}
        />
      ) : (
        <ChevronRight size={18} aria-hidden="true" className="thing-more" />
      )}
    </article>
  )
}

/** The home on Главная: every room with its devices; nothing while no Home Assistant is connected. */
export function HouseRooms() {
  const house = useStore((s) => s.house)
  if (!house || house.status === 'off' || !house.devices.length) return null
  const stale = house.status !== 'on'
  const known = new Set(house.rooms.map((r) => r.id))
  const rooms = [
    ...house.rooms.map((r) => ({ ...r, devices: house.devices.filter((d) => d.room === r.id) })),
    // in no room, or in one the home does not list
    {
      id: null,
      name: house.rooms.length ? 'Без комнаты' : 'Устройства',
      devices: house.devices.filter((d) => !known.has(d.room)),
    },
  ].filter((r) => r.devices.length)
  return (
    <>
      {stale && (
        <p className="house-note" role="status">
          {house.status === 'connecting'
            ? 'Подключение к Home Assistant…'
            : 'Home Assistant не отвечает. Устройства показаны такими, какими были'}
        </p>
      )}
      {rooms.map((r) => (
        <section key={r.id ?? ''} className="stations-col" aria-label={r.name}>
          <div className="section-head">
            <h2 className="section-title">{r.name}</h2>
          </div>
          <div className="things">
            {r.devices.map((d) => (
              <ThingCard key={d.id} d={d} stale={stale} />
            ))}
          </div>
        </section>
      ))}
    </>
  )
}

// ---------------------------------------------------------------- a device's page

/** A number in a range: a slider, set when it is let go. */
function Slider({ c, value, off, set }) {
  const [draft, setDraft] = useState(null)
  const v = draft ?? value ?? c.min
  const commit = () => {
    if (draft == null) return
    setDraft(null)
    if (draft !== value) set(draft)
  }
  const text = draft == null && value == null ? '—' : shown({ ...c, value: v })
  return (
    <div className="slide long">
      <input
        className="range"
        type="range"
        min={c.min}
        max={c.max}
        step={c.step || 1}
        value={v}
        disabled={off}
        aria-label={c.name}
        aria-valuetext={text}
        style={{ '--pos': `${((v - c.min) / (c.max - c.min || 1)) * 100}%` }}
        onChange={(e) => setDraft(Number(e.target.value))}
        onPointerUp={commit}
        onKeyUp={commit}
        onBlur={commit}
      />
      <output>{text}</output>
    </div>
  )
}

/** A number with no range said, or one too wide for a slider: typed, set on Enter or when left; Escape: as it was. */
function NumberField({ c, value, off, set }) {
  const [text, setText] = useState(null)
  const commit = () => {
    if (text == null) return
    setText(null)
    const t = text.trim().replace(',', '.').replace('−', '-')
    if (/^-?\d+(\.\d+)?$/.test(t) && Number(t) !== value) set(Number(t))
  }
  return (
    <span className="thing-typed">
      <input
        className="field sm"
        inputMode={c.min >= 0 ? 'decimal' : 'text'}
        value={text ?? (value == null ? '' : String(value).replace('.', ','))}
        disabled={off}
        aria-label={c.name}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === 'Enter') commit()
          if (e.key === 'Escape') setText(null)
        }}
      />
      {c.unit && <span className="faint">{c.unit}</span>}
    </span>
  )
}

/** A slider serves a range it can be set in by hand: up to a thousand steps. */
const slides = (c) => c.min != null && c.max != null && c.max > c.min && (c.max - c.min) / (c.step || 1) <= 1000

function ControlRow({ c, away }) {
  const { value, set, busy, error } = useSetting(c)
  const off = away || !c.available
  const options = c.options || []
  let ctl
  if (c.kind === 'bool') {
    ctl = <Switch on={!!value} disabled={off} busy={busy} onChange={(v) => set(v)} label={c.name} />
  } else if (c.kind === 'number') {
    const Field = slides(c) ? Slider : NumberField
    ctl = <Field c={c} value={value} off={off} set={set} />
  } else if (c.kind === 'choice') {
    const known = options.some((o) => o.value === value)
    ctl = (
      <select
        className="field sm"
        value={known ? value : ''}
        disabled={off}
        aria-label={c.name}
        aria-busy={busy}
        onChange={(e) => set(e.target.value)}
      >
        {!known && <option value="">—</option>}
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    )
  } else if (c.kind === 'action') {
    ctl = (
      <button
        type="button"
        className="btn sm"
        disabled={off}
        aria-busy={busy}
        aria-label={c.name}
        onClick={() => !busy && set(true)}
      >
        Выполнить
      </button>
    )
  } else ctl = <span className="thing-value num">{shown(c) ?? '—'}</span>
  return (
    <div
      className={`r ${c.kind === 'number' || c.kind === 'choice' ? 'stack-m' : 'wrap'}${c.available ? '' : ' away'}`}
    >
      <div className="r-text">
        <b>{c.name}</b>
        {error && (
          <span className="err" role="alert">
            {error}
          </span>
        )}
      </div>
      <div className="r-ctl">{ctl}</div>
    </div>
  )
}

const GROUPS = [
  ['main', null],
  ['settings', 'Настройки'],
  ['info', 'Сведения'],
]

export default function Thing() {
  const { id } = useParams()
  const house = useStore((s) => s.house)
  const link = useStore((s) => s.link)
  const d = house?.devices.find((x) => x.id === id)
  const back = { to: '/', label: 'Главная' }
  if (!d) {
    // not there, or not known yet: Home Assistant is being connected to, is away, or is not set up at all
    const waiting = link !== 'live' || !house || house.status === 'connecting'
    return (
      <main className="page narrow">
        <PageHead back={back} title="Устройство" />
        {waiting ? (
          <Loading />
        ) : (
          <div className="card">
            {house.status === 'on' ? (
              <Empty icon={Box} title="Устройство не найдено">
                Его нет в Home Assistant, или оно там скрыто.
              </Empty>
            ) : (
              <Empty
                icon={Box}
                title={house.status === 'off' ? 'Home Assistant не подключён' : 'Home Assistant не отвечает'}
              >
                Подключение — в разделе <Link to="/settings/house">Настройки → Умный дом</Link>.
              </Empty>
            )}
          </div>
        )}
      </main>
    )
  }
  const Icon = ICONS[d.kind] || Box
  const room = house.rooms.find((r) => r.id === d.room)?.name
  const stale = house.status !== 'on'
  const away = stale || !d.online
  const state = house.status === 'connecting' ? 'Подключение…' : stale ? 'Home Assistant не отвечает' : summary(d)
  const made = [d.maker, d.model].filter(Boolean).join(' · ')
  return (
    <main className="page narrow">
      <PageHead
        back={back}
        icon={
          <span className={`thing-icon big${active(d) ? ' on' : ''}`} aria-hidden="true">
            <Icon size={24} />
          </span>
        }
        title={d.name}
        sub={[room, state].filter(Boolean).join(' · ')}
      />
      {GROUPS.map(([group, title]) => {
        const list = d.controls.filter((c) => c.group === group)
        if (!list.length && !(group === 'info' && made)) return null
        return (
          <section key={group} className="card" aria-label={title || 'Управление'}>
            {title && <h2>{title}</h2>}
            <div className="rows thing-rows">
              {list.map((c) => (
                <ControlRow key={c.id} c={c} away={away} />
              ))}
              {group === 'info' && made && (
                <div className="r wrap">
                  <div className="r-text">
                    <b>Модель</b>
                  </div>
                  <div className="r-ctl">
                    <span className="thing-value">{made}</span>
                  </div>
                </div>
              )}
            </div>
          </section>
        )
      })}
    </main>
  )
}
