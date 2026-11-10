// A station's page. On top what it does now, in the reference's two tiles: the raspberry one is a ring going on, or
// the music, or else what it does (what is said while it talks; its next alarm and last request when quiet); the blue
// one is the volume. Its timers under them; then its alarms and wakes. Its conversations (the chat button) and its
// own settings (the gear) are pages of their own.
// Room is kept for what comes next: «Слушать» and «Записать» beside the buttons, «Записи» among the tabs.
import { useEffect, useRef, useState } from 'react'
import { Link, Navigate, NavLink, Route, Routes, useParams } from 'react-router'
import {
  AlarmClock,
  ChevronRight,
  MessageCircle,
  MessagesSquare,
  SlidersHorizontal,
  Speaker,
  Timer as TimerIcon,
  X,
} from 'lucide-react'
import { api } from '../shared/api.js'
import { useAction, useNow } from '../shared/hooks.js'
import { chatOf, deviceName, getState, loadChat, useStore } from '../shared/store.js'
import { clock, day, lastSeen, ringDay, soundTitle, span, status, trackTime } from '../shared/format.js'
import { devicePath, Dot, Empty, Loading, PageHead, PlayerCard, RingingCard, VolumeCard } from './ui.jsx'
import { Glyph } from './Flower.jsx'
import Chat, { Typing } from './Chat.jsx'
import Alarms from './Alarms.jsx'
import Wakes from './Wakes.jsx'

/** One page per station: another station's address mounts it anew, so nothing typed or kept for one (a name being
 * edited, a filter) carries over to the next. */
export default function Device() {
  const { id } = useParams()
  return <DevicePage key={id} id={id} />
}

function DevicePage({ id }) {
  const d = useStore((s) => s.devices[id])
  const link = useStore((s) => s.link)
  if (!d) {
    return (
      <main className="page">
        <PageHead back={{ to: '/', label: 'Главная' }} title="Колонка" />
        {link === 'live' ? (
          <div className="card">
            <Empty icon={Speaker} title="Колонка не найдена">
              Колонки «{id}» нет на этом сервере.
            </Empty>
          </div>
        ) : (
          <Loading />
        )}
      </main>
    )
  }
  return (
    <Routes>
      <Route path="settings" element={<StationSettings d={d} />} />
      <Route path="chat" element={<Chat device={d} />} />
      <Route path="*" element={<Station d={d} />} />
    </Routes>
  )
}

// ---------------------------------------------------------------- the station

function Station({ d }) {
  const base = devicePath(d.id)
  return (
    <main className="page">
      <PageHead
        back={{ to: '/', label: 'Главная' }}
        over={
          <>
            <Dot d={d} />
            {d.online ? status(d) : lastSeen(d.last_seen) || status(d)}
          </>
        }
        title={deviceName(d)}
      >
        <Link to={`${base}/chat`} className="btn icon" aria-label="Разговоры" title="Разговоры">
          <MessagesSquare size={20} aria-hidden="true" />
        </Link>
        <Link to={`${base}/settings`} className="btn icon" aria-label="Настройки колонки" title="Настройки колонки">
          <SlidersHorizontal size={20} aria-hidden="true" />
        </Link>
      </PageHead>

      {d.online && (
        <div className="row">
          {d.ring ? (
            <RingingCard d={d} className="grow-2" />
          ) : d.music ? (
            <PlayerCard d={d} className="grow-2" />
          ) : (
            <LiveCard d={d} />
          )}
          <VolumeCard d={d} className="grow-1" />
        </div>
      )}
      <Timers d={d} />

      <div className="tabs-line">
        <nav className="seg big" aria-label="Разделы колонки">
          <NavLink to={base} end>
            Будильники
          </NavLink>
          <NavLink to={`${base}/wakes`}>Срабатывания</NavLink>
        </nav>
      </div>
      <Routes>
        <Route index element={<Alarms d={d} />} />
        <Route path="wakes" element={<Wakes d={d} />} />
        <Route path="talk" element={<Navigate to={`${base}/chat`} replace />} />
        {/* absolute: a relative one in a splat route resolves against the whole address */}
        <Route path="*" element={<Navigate to={base} replace />} />
      </Routes>
    </main>
  )
}

/** When nothing plays or rings: what the station does; while it talks, what is said, as it comes; when quiet, its
 * next alarm and when it was last asked something. */
function LiveCard({ d }) {
  const live = useStore((s) => s.live[d.id])
  const chat = useStore(chatOf(d.id))
  const alarms = useStore((s) => s.alarms)
  const now = useNow(30000)
  useEffect(() => {
    if (!getState().chats[d.id]) loadChat(d.id)
  }, [d.id])
  const last = chat?.items?.[chat.items.length - 1]
  const next = alarms.find((a) => a.device === d.id && a.enabled && a.next)
  const heard = live && (live.text || live.heard)
  const said = live?.said.join(' ')
  const talking = live || d.panel === 'listen'
  return (
    <section className="card live-card grow-2" aria-label="Что происходит">
      <div className="live-state">
        <Glyph device={d} size={56} />
        <b>{status(d)}</b>
      </div>
      {talking ? (
        <div className="live-lines" aria-live="polite">
          {live ? (
            <>
              {heard && <p className="bubble you">{heard}</p>}
              <p className="bubble answer">
                {said ? <span className="caret">{said}</span> : <Typing label="Отвечает" />}
              </p>
            </>
          ) : (
            <p className="bubble you">
              <Typing label="Слушает" />
            </p>
          )}
        </div>
      ) : (
        <div className="rows">
          {next && (
            <Link to={devicePath(d.id)} className="r live-row">
              <span className="ico sm" aria-hidden="true">
                <AlarmClock size={18} />
              </span>
              <span className="r-text">
                <b>
                  {next.label || 'Будильник'} {ringDay(next.next, now)} в {next.time}
                </b>
              </span>
              <ChevronRight size={18} aria-hidden="true" />
            </Link>
          )}
          {last && (
            <Link to={`${devicePath(d.id)}/chat`} className="r live-row">
              <span className="ico sm" aria-hidden="true">
                <MessageCircle size={18} />
              </span>
              <span className="r-text">
                <b>{last.text ? `«${last.text}»` : 'Последний запрос'}</b>
                <span>
                  {day(last.t)
                    .replace(/^Сегодня$/, 'сегодня')
                    .replace(/^Вчера$/, 'вчера')}{' '}
                  в {clock(last.t)}
                </span>
              </span>
            </Link>
          )}
        </div>
      )}
    </section>
  )
}

function TimerChip({ t }) {
  const now = useNow(1000)
  const [cancel, busy, error] = useAction(() => api.cancelTimer(t.id))
  return (
    <div className="timer-chip">
      <span className="ico sm">
        <TimerIcon size={18} aria-hidden="true" />
      </span>
      <div style={{ display: 'flex', flexDirection: 'column' }}>
        <span className="left">{trackTime(t.at - now)}</span>
        <span>
          {t.label ? `«${t.label}»` : 'Таймер'} на {span(t.seconds)}
        </span>
        {error && <span className="err">{error}</span>}
      </div>
      <button
        type="button"
        className="btn ghost icon sm"
        disabled={busy}
        onClick={() => cancel()}
        aria-label="Отменить таймер"
        title="Отменить"
      >
        <X size={18} aria-hidden="true" />
      </button>
    </div>
  )
}

function Timers({ d }) {
  const timers = useStore((s) => s.timers)
  const mine = timers.filter((t) => t.device === d.id)
  if (!mine.length) return null
  return (
    <div className="timer-strip" aria-label="Таймеры">
      {mine.map((t) => (
        <TimerChip key={t.id} t={t} />
      ))}
    </div>
  )
}

// ---------------------------------------------------------------- its settings

function Name({ d }) {
  const remote = d.name || ''
  const [name, setName] = useState(remote)
  const [saved, setSaved] = useState(false)
  // renamed elsewhere (another tab, the event stream): the field follows unless something else is typed in it
  const was = useRef(remote)
  useEffect(() => {
    const before = was.current // read now: the updater below runs later
    was.current = remote
    setName((n) => (n.trim() === before || n.trim() === remote ? remote : n))
  }, [remote])
  const [save, busy, error] = useAction(async () => {
    await api.renameDevice(d.id, name.trim())
    setSaved(true)
  })
  return (
    <form
      className="form-acts"
      style={{ flexWrap: 'nowrap', width: '100%' }}
      onSubmit={(e) => {
        e.preventDefault()
        save()
      }}
    >
      <label className="sr-only" htmlFor="dev-name">
        Название
      </label>
      <input
        id="dev-name"
        className="field"
        value={name}
        maxLength={60}
        placeholder={d.id}
        onChange={(e) => {
          setName(e.target.value)
          setSaved(false)
        }}
      />
      <button className="btn" disabled={busy || name.trim() === (d.name || '')}>
        {saved ? 'Сохранено' : 'Сохранить'}
      </button>
      {error && <p className="err">{error}</p>}
    </form>
  )
}

function TryRing({ d, sound, label }) {
  const [go, busy, error] = useAction(() => api.tryRing(d.id, sound, 5))
  return (
    <>
      <button type="button" className="btn" disabled={!d.online || !!d.ring || busy} onClick={() => go()}>
        {label}
      </button>
      {error && <p className="err">{error}</p>}
    </>
  )
}

function StationSettings({ d }) {
  const settings = useStore((s) => s.settings)
  const base = devicePath(d.id)
  return (
    <main className="page narrow">
      <PageHead back={{ to: base, label: deviceName(d) }} title="Настройки колонки" />
      <section className="card" aria-labelledby="st-main">
        <h2 id="st-main">Основное</h2>
        <div className="rows">
          <div className="r stack-m">
            <div className="r-text">
              <b>Название</b>
            </div>
            <div className="r-ctl" style={{ width: 360 }}>
              <Name d={d} />
            </div>
          </div>
          <div className="r stack-m">
            <div className="r-text">
              <b>Идентификатор</b>
            </div>
            <div className="r-ctl">
              <code>{d.id}</code>
            </div>
          </div>
        </div>
      </section>

      <section className="card" aria-labelledby="st-sound">
        <h2 id="st-sound">Проверка звука</h2>
        <p className="about">
          Пять секунд звонка на колонке. Звуки: таймер «{soundTitle(settings?.timer?.sound || 'chime')}», будильник «
          {soundTitle(settings?.alarm?.sound || 'chime')}». <Link to="/settings/sounds">Изменить</Link>
        </p>
        <div className="form-acts">
          <TryRing d={d} sound={settings?.timer?.sound} label="Звонок таймера" />
          <TryRing d={d} sound={settings?.alarm?.sound} label="Звонок будильника" />
        </div>
        {!d.online && <p className="faint">Колонка не в сети</p>}
      </section>
    </main>
  )
}
