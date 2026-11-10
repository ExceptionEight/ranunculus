// A station's alarms as cards (a time, once or on chosen weekdays; each may have its own sound, length, rise and
// volume, or take the defaults from Settings → Звуки) and a card to add one. Its timers are in the strip above (Device.jsx).
import { useState } from 'react'
import { Link } from 'react-router'
import { Plus, Trash2 } from 'lucide-react'
import { api } from '../shared/api.js'
import { useAction, useNow, useSounds } from '../shared/hooks.js'
import { useStore } from '../shared/store.js'
import { daysWords, ringDay, RING_STOPS, soundTitle, span, WEEKDAYS } from '../shared/format.js'
import { Sheet, SoundSelect, Switch } from './ui.jsx'

const RAMPS = [0, 15, 30, 60, 120, 300]
const rampWords = (s) => (s ? `за ${span(s)}` : 'сразу громко')
const cap = (s) => s[0].toUpperCase() + s.slice(1)
const PRESETS = [
  ['По будням', [1, 2, 3, 4, 5]],
  ['По выходным', [6, 7]],
  ['Каждый день', [1, 2, 3, 4, 5, 6, 7]],
  ['Один раз', []],
]
const same = (a, b) => a.length === b.length && a.every((x, i) => x === b[i])

function Editor({ device, alarm, onClose }) {
  const sounds = useSounds()
  const settings = useStore((s) => s.settings)
  const def = settings?.alarm
  const [f, setF] = useState(() =>
    alarm
      ? {
          time: alarm.time,
          days: alarm.days,
          label: alarm.label,
          sound: alarm.sound,
          ring: alarm.ring,
          ramp: alarm.ramp,
          volume: alarm.volume,
        }
      : { time: '07:00', days: [1, 2, 3, 4, 5], label: '', sound: null, ring: null, ramp: null, volume: null },
  )
  const set = (k) => (v) => setF((x) => ({ ...x, [k]: v }))
  const [save, busy, error] = useAction(async () => {
    if (alarm) await api.changeAlarm(alarm.id, f)
    else await api.addAlarm({ ...f, device })
    onClose()
  })
  const [remove, removing, removeError] = useAction(async () => {
    await api.deleteAlarm(alarm.id)
    onClose()
  })
  const days = [...f.days].sort()
  const toggleDay = (n) => set('days')(f.days.includes(n) ? f.days.filter((x) => x !== n) : [...f.days, n].sort())
  const inherit = (what) => `По умолчанию${what ? `: ${what}` : ''}`
  return (
    <Sheet
      title={alarm ? 'Будильник' : 'Новый будильник'}
      onClose={onClose}
      onSubmit={save}
      foot={
        <>
          {alarm && (
            <button type="button" className="btn ghost danger" disabled={removing} onClick={() => remove()}>
              <Trash2 size={17} aria-hidden="true" />
              Удалить
            </button>
          )}
          <span className="spacer" />
          <button type="button" className="btn" onClick={onClose}>
            Отмена
          </button>
          <button className="btn primary" disabled={busy || !f.time}>
            {alarm ? 'Сохранить' : 'Поставить'}
          </button>
        </>
      }
    >
      <label className="sr-only" htmlFor="al-time">
        Время
      </label>
      <input
        id="al-time"
        className="field time-input"
        type="time"
        required
        value={f.time}
        onChange={(e) => set('time')(e.target.value)}
      />
      <div className="field-wrap">
        <span id="al-days">{f.days.length ? `Повтор: ${daysWords(f.days)}` : 'Без повтора'}</span>
        <div className="days" role="group" aria-labelledby="al-days">
          {WEEKDAYS.map((w, i) => (
            <button
              key={w}
              type="button"
              className="day-chip"
              aria-pressed={f.days.includes(i + 1)}
              onClick={() => toggleDay(i + 1)}
            >
              {w}
            </button>
          ))}
        </div>
        <div className="chips">
          {PRESETS.map(([name, ds]) => (
            <button
              key={name}
              type="button"
              className="chip"
              aria-pressed={same(days, ds)}
              onClick={() => set('days')(ds)}
            >
              {name}
            </button>
          ))}
        </div>
      </div>
      <label className="field-wrap">
        <span>Название</span>
        <input
          className="field"
          maxLength={60}
          placeholder="Например, «Работа»"
          value={f.label}
          onChange={(e) => set('label')(e.target.value)}
        />
      </label>
      <label className="field-wrap">
        <span>Звук</span>
        <SoundSelect
          sounds={sounds}
          value={f.sound}
          onChange={set('sound')}
          inherit={inherit(def && soundTitle(def.sound))}
        />
      </label>
      <div className="form-pair">
        <label className="field-wrap">
          <span>Длительность</span>
          <select
            className="field"
            value={f.ring ?? ''}
            onChange={(e) => set('ring')(e.target.value ? Number(e.target.value) : null)}
          >
            <option value="">{inherit(def && span(def.ring))}</option>
            {RING_STOPS.filter((s) => s >= 15).map((s) => (
              <option key={s} value={s}>
                {span(s)}
              </option>
            ))}
          </select>
        </label>
        <label className="field-wrap">
          <span>Нарастание громкости</span>
          <select
            className="field"
            value={f.ramp ?? ''}
            onChange={(e) => set('ramp')(e.target.value === '' ? null : Number(e.target.value))}
          >
            <option value="">{inherit(def && rampWords(def.ramp))}</option>
            {RAMPS.map((s) => (
              <option key={s} value={s}>
                {cap(rampWords(s))}
              </option>
            ))}
          </select>
        </label>
      </div>
      <label className="field-wrap">
        <span>Громкость звонка</span>
        <select
          className="field"
          value={f.volume ?? ''}
          onChange={(e) => set('volume')(e.target.value === '' ? null : Number(e.target.value))}
        >
          <option value="">
            {inherit(def && (def.volume == null ? 'текущая громкость колонки' : String(def.volume)))}
          </option>
          {[1, 2, 3, 4, 5, 6, 7, 8, 9, 10].map((v) => (
            <option key={v} value={v}>
              {v}
            </option>
          ))}
        </select>
      </label>
      {(error || removeError) && <p className="err">{error || removeError}</p>}
    </Sheet>
  )
}

// Each card keeps its own clock (one on top of the page would render all of it again every second): an alarm only
// needs «сегодня» to turn into «завтра».
function AlarmCard({ a, onOpen }) {
  const now = useNow(30000)
  const [toggle, busy, error] = useAction((on) => api.changeAlarm(a.id, { enabled: on }))
  return (
    <article className={`card alarm${a.enabled ? '' : ' off'}`}>
      <button type="button" className="alarm-open" onClick={onOpen} aria-label={`Будильник на ${a.time}, изменить`} />
      <div className="alarm-top">
        <span className="alarm-time">{a.time}</span>
        <Switch on={a.enabled} disabled={busy} onChange={(on) => toggle(on)} label={`Будильник на ${a.time}`} />
      </div>
      <div className="alarm-what">
        <b>{a.label || 'Будильник'}</b>
        <span>{daysWords(a.days)[0].toUpperCase() + daysWords(a.days).slice(1)}</span>
        <span>{a.enabled ? `Зазвонит ${ringDay(a.next, now)}` : 'Выключен'}</span>
      </div>
      {error && <p className="err">{error}</p>}
    </article>
  )
}

export default function Alarms({ d }) {
  const alarms = useStore((s) => s.alarms)
  const [open, setOpen] = useState(null) // an alarm, or 'new'
  const mine = alarms.filter((a) => a.device === d.id)
  return (
    <>
      <div className="grid-cards">
        {mine.map((a) => (
          <AlarmCard key={a.id} a={a} onOpen={() => setOpen(a)} />
        ))}
        <button type="button" className="alarm-new" onClick={() => setOpen('new')}>
          <span className="ico">
            <Plus size={22} aria-hidden="true" />
          </span>
          Новый будильник
        </button>
      </div>
      <p className="faint" style={{ padding: '0 4px', fontSize: 14 }}>
        Звонок выключается голосом или касанием колонки. Звук и длительность по умолчанию:{' '}
        <Link to="/settings/sounds">Настройки → Звуки</Link>.
      </p>

      {open && <Editor device={d.id} alarm={open === 'new' ? null : open} onClose={() => setOpen(null)} />}
    </>
  )
}
