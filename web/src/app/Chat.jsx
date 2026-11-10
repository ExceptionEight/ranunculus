// A station's conversations as a chat window of its own (opened by the chat button on the station's page): the
// newest at the bottom, older pages as the top comes into view; each request and its answer as bubbles, what the
// assistant did with its tools as chips between them, the day as a separator; what is going on now at the end as it
// happens. The time under a request opens its details. On a phone it takes the whole screen.
import { Fragment, useMemo, useState } from 'react'
import { Link } from 'react-router'
import {
  ArrowDown,
  CalendarDays,
  Calculator,
  ChevronLeft,
  CloudSun,
  Globe,
  History,
  ListMusic,
  MessagesSquare,
  Music2,
  Search,
  Timer,
  Volume2,
  Wrench,
} from 'lucide-react'
import { useChat } from '../shared/hooks.js'
import { deviceName } from '../shared/store.js'
import { clock, sameDay, secs, status } from '../shared/format.js'
import { describe } from '../shared/calls.js'
import { devicePath, Dot, Empty, Loading } from './ui.jsx'
import { Glyph } from './Flower.jsx'
import { tz } from '../shared/zone.js'

const TOOL_ICON = {
  set_timer: Timer,
  set_volume: Volume2,
  play_music: Music2,
  player_control: ListMusic,
  get_weather: CloudSun,
  web_search: Search,
  read_page: Globe,
  recall: History,
  calculate: Calculator,
  date_calc: CalendarDays,
}

/** «Сегодня», «Вчера», «Суббота, 7 ноября», with the year when it is not this one */
function dayTitle(t, now) {
  if (sameDay(t, now)) return 'Сегодня'
  if (sameDay(t, now - 86400)) return 'Вчера'
  const d = new Date(t * 1000)
  const year = (x) => new Date(x * 1000).toLocaleDateString('en-CA', { timeZone: tz(), year: 'numeric' })
  const s = d.toLocaleDateString('ru-RU', {
    timeZone: tz(),
    weekday: 'long',
    day: 'numeric',
    month: 'long',
    ...(year(t) !== year(now) ? { year: 'numeric' } : {}),
  })
  return s[0].toUpperCase() + s.slice(1)
}

/** Three dots while words are coming: `label` says whose («Слушает», «Отвечает»); without one, only to look at. */
export function Typing({ label }) {
  return (
    <span className="typing" {...(label ? { role: 'img', 'aria-label': label } : { 'aria-hidden': true })}>
      <i />
      <i />
      <i />
    </span>
  )
}

function Calls({ calls }) {
  if (!calls.length) return null
  return (
    <div className="calls">
      {calls.map((c, i) => {
        const Icon = TOOL_ICON[c.kind] || Wrench
        return (
          <span
            key={i}
            className={`call${c.failed ? ' failed' : ''}`}
            title={c.failed ? `${c.direction} Не получилось.` : c.direction}
          >
            <Icon size={14} aria-hidden="true" />
            {c.label[0]?.toUpperCase()}
            {c.label.slice(1)}
          </span>
        )
      })}
    </div>
  )
}

function Exchange({ x }) {
  const [open, setOpen] = useState(false)
  const calls = useMemo(() => (x.calls || []).map(describe), [x.calls])
  const stopped = x.ring === 'stopped'
  const timings = [
    x.asr_s != null && `распознавание ${secs(x.asr_s)}`,
    x.llm_s != null && `ответ ${secs(x.llm_s)}`,
    x.from_hit_s != null && `от слова до конца ответа ${secs(x.from_hit_s)}`,
  ].filter(Boolean)
  return (
    <article className="ex">
      {x.text ? (
        <p className="bubble you">{x.text}</p>
      ) : (
        !stopped && <p className="note">{x.heard ? 'Только слово активации' : 'Неразборчиво'}</p>
      )}
      <button
        type="button"
        className="ex-time"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        title="Подробности"
      >
        {clock(x.t)}
      </button>
      {stopped && <p className="note">Звонок выключен</p>}
      <Calls calls={calls} />
      {x.answer && <p className="bubble answer">{x.answer}</p>}
      {x.cut && <p className="note">Ответ прерван</p>}
      {open && (
        <div className="ex-more">
          {timings.length > 0
            ? `${timings[0][0].toUpperCase()}${timings.join(' · ').slice(1)}`
            : 'Время ответа не записано'}
          {x.heard && x.heard !== x.text && (
            <>
              <br />
              Распознано: «{x.heard}»
            </>
          )}
          {x.model && (
            <>
              <br />
              Модель: {x.model}
            </>
          )}
        </div>
      )}
    </article>
  )
}

/** What is going on at the station now: listening, the heard line, the answer's sentences as they come. */
function Live({ device, turn }) {
  if (turn) {
    const said = turn.said.join(' ')
    return (
      <article className="ex" aria-live="polite">
        {turn.text || turn.heard ? (
          <p className="bubble you">{turn.text || turn.heard}</p>
        ) : (
          <p className="note">Неразборчиво</p>
        )}
        <p className="bubble answer">{said ? <span className="caret">{said}</span> : <Typing label="Отвечает" />}</p>
      </article>
    )
  }
  if (device?.online && device.panel === 'listen') {
    return (
      <article className="ex" aria-live="polite">
        <p className="bubble you">
          <Typing label="Слушает" />
        </p>
      </article>
    )
  }
  return null
}

export default function Chat({ device: d }) {
  const { chat, live, scrollRef, topRef, atBottom, toBottom } = useChat(d.id)
  const items = chat?.items || []
  const now = Date.now() / 1000
  const empty = chat && !chat.loading && !chat.error && !items.length && !live && !(d.online && d.panel === 'listen')
  return (
    <main className="chat-page" aria-label={`Разговоры: ${deviceName(d)}`}>
      <header className="chat-head">
        <Link to={devicePath(d.id)} className="btn ghost icon" aria-label="Назад к колонке" title="Назад">
          <ChevronLeft size={22} aria-hidden="true" />
        </Link>
        <Glyph device={d} size={40} />
        <div className="chat-title">
          <h1>{deviceName(d)}</h1>
          <span>
            <Dot d={d} />
            {status(d)}
          </span>
        </div>
      </header>
      <div className="chat-scroll" ref={scrollRef}>
        <div className="chat-feed">
          <div ref={topRef} />
          {chat?.loading && items.length > 0 && <Loading />}
          {chat && !chat.loading && !chat.more && items.length > 0 && <p className="note">Начало истории</p>}
          {items.map((x, i) => {
            const head = i === 0 || !sameDay(items[i - 1].t, x.t)
            return (
              <Fragment key={x.id}>
                {head && <p className="day-sep">{dayTitle(x.t, now)}</p>}
                <Exchange x={x} />
              </Fragment>
            )
          })}
          <Live device={d} turn={live} />
          {(!chat || (chat.loading && !items.length)) && <Loading />}
          {chat?.error && <p className="notice">Не удалось загрузить разговоры: {chat.error}</p>}
          {empty && (
            <Empty icon={MessagesSquare} title="Разговоров пока нет">
              Здесь появятся запросы к колонке и её ответы.
            </Empty>
          )}
        </div>
      </div>
      {!atBottom && items.length > 0 && (
        <button type="button" className="to-latest" onClick={toBottom}>
          <ArrowDown size={16} aria-hidden="true" />К последним
        </button>
      )}
    </main>
  )
}
