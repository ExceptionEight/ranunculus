// Главная, top to bottom: what needs a hand now (a ring; no models chosen yet), the stations with their volume, what
// plays, the home's devices by room (House.jsx), what is put off («выключи музыку через полчаса»: waiting, and how
// what was due lately ended; all there ever was — Later.jsx), and what is set to ring (the timers going, the alarms
// on). Editing an alarm is on its station's page.
import { useMemo } from 'react'
import { Link } from 'react-router'
import { AlarmClock, CalendarClock, ChevronRight, KeyRound, Music2, Speaker, X } from 'lucide-react'
import { api } from '../shared/api.js'
import { useAction, useNow } from '../shared/hooks.js'
import { deviceList, deviceName, useStore } from '../shared/store.js'
import { clock, lastSeen, ringDay, status, trackTime } from '../shared/format.js'
import { missing, nothingChosen } from '../shared/providers.js'
import { devicePath, Empty, PageHead, PlayerCard, RingingCard, Switch, VolumeSlider } from './ui.jsx'
import { Glyph } from './Flower.jsx'
import { HouseRooms } from './House.jsx'
import { LaterItem } from './Later.jsx'
import { tz } from '../shared/zone.js'

function today() {
  const s = new Date().toLocaleDateString('ru-RU', { timeZone: tz(), weekday: 'long', day: 'numeric', month: 'long' })
  return s[0].toUpperCase() + s.slice(1)
}

function StationCard({ d }) {
  const m = d.online && d.music
  return (
    <article className={`card station${d.online ? '' : ' off'}`}>
      <Link to={devicePath(d.id)} className="station-link">
        <Glyph device={d} size={36} />
        <span className="station-name">
          <b>{deviceName(d)}</b>
          <span>{d.online ? status(d) : lastSeen(d.last_seen) || status(d)}</span>
        </span>
        <ChevronRight size={18} aria-hidden="true" />
      </Link>
      {m && (
        <p className="station-now">
          <Music2 size={15} aria-hidden="true" />
          <span>
            {m.title ?? m.track}
            {m.artist ? ` — ${m.artist}` : ''}
          </span>
        </p>
      )}
      {d.online && <VolumeSlider d={d} />}
    </article>
  )
}

function TimerItem({ t, devices }) {
  const now = useNow(1000)
  const [cancel, busy, error] = useAction(() => api.cancelTimer(t.id))
  return (
    <li className="agenda-item">
      <span className="when">{trackTime(t.at - now)}</span>
      <span className="what">
        <b>{t.label ? `Таймер «${t.label}»` : 'Таймер'}</b>
        <span>
          <Link to={devicePath(t.device)}>{deviceName(devices[t.device]) || t.device}</Link> · в {clock(t.at)}
        </span>
        {error && <span className="err">{error}</span>}
      </span>
      <button
        type="button"
        className="btn ghost icon sm"
        disabled={busy}
        onClick={() => cancel()}
        aria-label="Отменить таймер"
        title="Отменить"
      >
        <X size={16} aria-hidden="true" />
      </button>
    </li>
  )
}

function AlarmItem({ a, devices }) {
  const now = useNow(30000)
  const [toggle, busy, error] = useAction((on) => api.changeAlarm(a.id, { enabled: on }))
  return (
    <li className="agenda-item">
      <span className="when">{a.time}</span>
      <span className="what">
        <b>{a.label || 'Будильник'}</b>
        <span>
          <Link to={devicePath(a.device)}>{deviceName(devices[a.device]) || a.device}</Link> · {ringDay(a.next, now)}
        </span>
        {error && <span className="err">{error}</span>}
      </span>
      <Switch on={a.enabled} disabled={busy} onChange={(on) => toggle(on)} label={`Будильник на ${a.time}`} />
    </li>
  )
}

function PutOff({ devices }) {
  const later = useStore((s) => s.later)
  return (
    <section className="stations-col" aria-labelledby="later-h">
      <div className="section-head">
        <h2 id="later-h" className="section-title">
          Автоматизация
        </h2>
        <Link to="/automation">Вся история</Link>
      </div>
      <div className="card">
        {later.length > 0 ? (
          <ul className="agenda">
            {later.map((e) => (
              <LaterItem key={e.id} e={e} devices={devices} main />
            ))}
          </ul>
        ) : (
          <Empty small icon={CalendarClock}>
            Отложенных команд нет. Скажите колонке «выключи музыку через полчаса», и команда появится здесь
          </Empty>
        )}
      </div>
    </section>
  )
}

function Agenda({ devices, list }) {
  const timers = useStore((s) => s.timers)
  const alarms = useStore((s) => s.alarms)
  const on = alarms.filter((a) => a.enabled)
  return (
    <section className="stations-col" aria-labelledby="agenda-h">
      <div className="section-head">
        <h2 id="agenda-h" className="section-title">
          Будильники и таймеры
        </h2>
        {list.length === 1 && <Link to={devicePath(list[0].id)}>Все</Link>}
      </div>
      <div className="card">
        {timers.length + on.length > 0 ? (
          <ul className="agenda">
            {timers.map((t) => (
              <TimerItem key={`t${t.id}`} t={t} devices={devices} />
            ))}
            {on.map((a) => (
              <AlarmItem key={`a${a.id}`} a={a} devices={devices} />
            ))}
          </ul>
        ) : (
          <Empty small icon={AlarmClock}>
            Нет включённых будильников и идущих таймеров
          </Empty>
        )}
      </div>
    </section>
  )
}

export default function Home() {
  const devices = useStore((s) => s.devices)
  const link = useStore((s) => s.link)
  const providers = useStore((s) => s.providers)
  const unset = missing(providers)
  const list = useMemo(() => deviceList(devices), [devices])
  const ringing = list.filter((d) => d.online && d.ring)
  const playing = list.filter((d) => d.online && d.music && !d.ring)
  return (
    <main className="page">
      <PageHead title="Главная" sub={today()} />
      {unset.length > 0 && (
        <div className="card">
          <Empty icon={KeyRound} title="Осталось выбрать модели" small>
            Пока их нет, колонка не услышит и не ответит.{' '}
            {nothingChosen(providers) ? 'Хватит одного ключа OpenRouter:' : 'Выбрать их можно в'}{' '}
            <Link to="/settings/models">Настройки → Модели</Link>.
          </Empty>
        </div>
      )}
      {ringing.length > 0 && (
        <div className="row">
          {ringing.map((d) => (
            <RingingCard key={d.id} d={d} named className="grow-2" />
          ))}
        </div>
      )}
      <section className="stations-col" aria-labelledby="stations-h">
        <div className="section-head">
          <h2 id="stations-h" className="section-title">
            Колонки
          </h2>
        </div>
        {list.length > 0 ? (
          <div className="stations">
            {list.map((d) => (
              <StationCard key={d.id} d={d} />
            ))}
          </div>
        ) : (
          link === 'live' && (
            <div className="card">
              <Empty icon={Speaker} title="Колонок пока нет">
                Чтобы подключить колонку, запустите на компьютере с сервером{' '}
                <code>station/install.sh АДРЕС_КОЛОНКИ</code>. Она появится здесь сама.
              </Empty>
            </div>
          )
        )}
      </section>

      {playing.length > 0 && (
        <div className="col">
          {playing.map((d) => (
            <PlayerCard key={d.id} d={d} named />
          ))}
        </div>
      )}
      <HouseRooms />
      <PutOff devices={devices} />
      <Agenda devices={devices} list={list} />
    </main>
  )
}
