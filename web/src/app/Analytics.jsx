// How the assistant is used: the conversations (requests per day, by the hour, what it did, by station) and the
// music (MusicStats.jsx); for one station or all, over a week, a month, three or a year.
import { useMemo, useState } from 'react'
import { NavLink, useLocation } from 'react-router'
import { ChartColumn } from 'lucide-react'
import { api } from '../shared/api.js'
import { useLoad } from '../shared/hooks.js'
import { deviceList, deviceName, useStore } from '../shared/store.js'
import { plural, secs } from '../shared/format.js'
import { Empty, Failed, Hint, Loading, PageHead, Seg } from './ui.jsx'
import { Bars, dayLabel, dayTick, everyDay, periodWords } from './charts.jsx'
import MusicStats from './MusicStats.jsx'

const PERIODS = [
  [7, '7 дней'],
  [30, '30 дней'],
  [90, '90 дней'],
  [365, 'Год'],
]
const TOOLS = {
  set_timer: 'Таймеры',
  set_volume: 'Громкость',
  play_music: 'Включение музыки',
  player_control: 'Управление плеером',
  get_weather: 'Погода',
  web_search: 'Поиск в интернете',
  read_page: 'Чтение страниц',
  recall: 'Память',
  calculate: 'Вычисления',
  date_calc: 'Даты',
}

/** What the music's numbers mean: beside the «?» in the corner while the music is shown. */
function MusicHelp() {
  return (
    <Hint label="Как считаются треки">
      <dl className="help-list">
        <div>
          <dt>Прослушан</dt>
          <dd>Играл не меньше 30 секунд</dd>
        </div>
        <div>
          <dt>До конца</dt>
          <dd>Доиграл или выключен в последние 10 секунд</dd>
        </div>
        <div>
          <dt>Пропущен</dt>
          <dd>Переключён раньше 30 секунд</dd>
        </div>
      </dl>
    </Hint>
  )
}

const requests = (n) => `${n} ${plural(n, ['запрос', 'запроса', 'запросов'])}`
const avg = (x) => x.toLocaleString('ru-RU', { maximumFractionDigits: 1 })

export default function Analytics() {
  const devices = useStore((s) => s.devices)
  const list = useMemo(() => deviceList(devices), [devices])
  const [device, setDevice] = useState('')
  const [days, setDays] = useState(30)
  const music = useLocation().pathname.startsWith('/analytics/music')
  return (
    <main className="page">
      <PageHead title="Аналитика">{music && <MusicHelp />}</PageHead>
      <div className="toolbar">
        <nav className="seg big" aria-label="Что показывать">
          <NavLink to="/analytics" end>
            Разговоры
          </NavLink>
          <NavLink to="/analytics/music">Музыка</NavLink>
        </nav>
        <span style={{ flex: 1 }} />
        {list.length > 1 && (
          <label>
            <span className="sr-only">Колонка</span>
            <select className="field sm" value={device} onChange={(e) => setDevice(e.target.value)}>
              <option value="">Все колонки</option>
              {list.map((d) => (
                <option key={d.id} value={d.id}>
                  {deviceName(d)}
                </option>
              ))}
            </select>
          </label>
        )}
        <Seg label="Период" items={PERIODS} value={days} onChange={setDays} />
      </div>
      {music ? (
        <MusicStats device={device} days={days} />
      ) : (
        <Talk device={device} days={days} devices={devices} setDevice={setDevice} />
      )}
    </main>
  )
}

function Talk({ device, days: chosen, devices, setDevice }) {
  const {
    data: fresh,
    error,
    last,
  } = useLoad(() => api.stats({ device: device || undefined, days: chosen }), [device, chosen])
  // drawn by the period it was counted for: while another loads, the last one stays (dimmed), never with the new days
  const data = error ? null : (last?.data ?? null)
  const days = last?.deps[1] ?? chosen
  const shownDevice = last?.deps[0] ?? device

  // every day of the period, the empty ones too
  const series = useMemo(
    () => (data ? everyDay(data.since, days, data.days).map((p) => ({ ...p, v: p.d?.requests || 0 })) : []),
    [data, days],
  )

  const sum = (k) => (data?.days || []).reduce((a, d) => a + (d[k] || 0), 0)
  const total = sum('requests')
  const weighted = (k) => {
    const rows = (data?.days || []).filter((d) => d[k] != null)
    const n = rows.reduce((a, d) => a + d.requests, 0)
    return n ? rows.reduce((a, d) => a + d[k] * d.requests, 0) / n : null
  }
  const llm = weighted('llm_s')
  const tools = useMemo(() => {
    const t = {}
    ;(data?.days || []).forEach((d) => Object.entries(d.tools || {}).forEach(([k, n]) => (t[k] = (t[k] || 0) + n)))
    return Object.entries(t).sort((a, b) => b[1] - a[1])
  }, [data])
  const toolMax = Math.max(1, ...tools.map(([, n]) => n))

  if (error) return <Failed error={error} />
  if (!data) return <Loading />
  if (!total)
    return (
      <div className={`card${fresh ? '' : ' stale'}`}>
        <Empty icon={ChartColumn} title="Нет данных">
          За {periodWords(days)} запросов не было.
        </Empty>
      </div>
    )
  const byDevice = !shownDevice && data.devices.length > 1
  const devMax = Math.max(1, ...data.devices.map((x) => x.requests))
  return (
    <div
      className={fresh ? undefined : 'stale'}
      aria-busy={!fresh}
      style={{ display: 'flex', flexDirection: 'column', gap: 'var(--gap)' }}
    >
      <div className="stats">
        <div className="hero rasp stat">
          <span>Запросы за {periodWords(days)}</span>
          <b className="num">{total}</b>
        </div>
        <div className="card stat">
          <span>В среднем за день</span>
          <b className="num">{avg(total / days)}</b>
        </div>
        <div className="card stat">
          <span>Среднее время ответа</span>
          <b className="num">{llm != null ? secs(llm) : 'нет'}</b>
        </div>
        <div className="card stat">
          <span>Прерванные ответы</span>
          <b className="num">{sum('cut')}</b>
        </div>
        <div className="card stat">
          <span>Не распознано</span>
          <b className="num">{sum('unheard')}</b>
        </div>
      </div>

      {/* two columns: the charts on the left, the lists on the right (one column on a phone) */}
      <div className="row top">
        <div className="col grow-2">
          <section className="card" aria-labelledby="ch-days">
            <h2 id="ch-days">Запросы по дням</h2>
            <Bars
              points={series}
              label={`Запросы по дням за ${periodWords(days)}`}
              tick={dayTick(series, days)}
              describe={(p) => [
                dayLabel(p.key, true),
                [
                  requests(p.v),
                  p.d?.llm_s != null && `ответ в среднем ${secs(p.d.llm_s)}`,
                  p.d?.cut && `прервано ${p.d.cut}`,
                ]
                  .filter(Boolean)
                  .join(' · '),
              ]}
            />
          </section>
          <section className="card" aria-labelledby="ch-hours">
            <h2 id="ch-hours">Время суток</h2>
            <Bars
              points={data.hours.map((v, h) => ({ key: h, name: `${h}:00`, v }))}
              label="Запросы по часам суток"
              tick={(p, i, narrow) => (p.key % (narrow ? 6 : 3) === 0 ? `${p.key}:00` : '')}
              describe={(p) => [`С ${p.key}:00 до ${p.key + 1}:00`, requests(p.v)]}
            />
          </section>
        </div>
        <div className="col grow-1">
          <section className="card" aria-labelledby="ch-tools">
            <h2 id="ch-tools">Что делал ассистент</h2>
            {tools.length ? (
              <ul className="bars-list">
                {tools.map(([k, n]) => (
                  <li key={k}>
                    <span className="bl-top">
                      <span>{TOOLS[k] || k}</span>
                      <span>{n}</span>
                    </span>
                    <span className="meter" aria-hidden="true">
                      <i style={{ width: `${(n / toolMax) * 100}%` }} />
                    </span>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="faint">Только разговоры, без действий</p>
            )}
          </section>
          {byDevice && (
            <section className="card" aria-labelledby="ch-dev">
              <h2 id="ch-dev">По колонкам</h2>
              <ul className="bars-list">
                {data.devices.map(({ device: id, requests: n }) => (
                  <li key={id}>
                    <span className="bl-top">
                      <button type="button" className="link-btn" onClick={() => setDevice(id)}>
                        {deviceName(devices[id]) || id}
                      </button>
                      <span>{n}</span>
                    </span>
                    <span className="meter" aria-hidden="true">
                      <i style={{ width: `${(n / devMax) * 100}%` }} />
                    </span>
                  </li>
                ))}
              </ul>
            </section>
          )}
        </div>
      </div>
    </div>
  )
}
