// The music listened to (listens.rs): minutes and tracks per day, the songs most played, the artists. Updates itself
// when a listen ends.
import { useEffect, useMemo, useState } from 'react'
import { Music2 } from 'lucide-react'
import { api } from '../shared/api.js'
import { useLoad } from '../shared/hooks.js'
import { onListen } from '../shared/store.js'
import { minutes, plural } from '../shared/format.js'
import { Empty, Failed, Loading } from './ui.jsx'
import { Bars, dayLabel, dayTick, everyDay, periodWords } from './charts.jsx'

const times = (n) => `${n} ${plural(n, ['раз', 'раза', 'раз'])}`
const tracks = (n) => `${n} ${plural(n, ['трек', 'трека', 'треков'])}`

function Song({ s, rank }) {
  return (
    <li className="song">
      <span className="rank">{rank}</span>
      <span className="what">
        <b>{s.title}</b>
        <span>{s.artist}</span>
        <small>
          Включали {times(s.listens)} · до конца {s.finished}
          {s.skipped ? ` · пропущен ${times(s.skipped)}` : ''}
        </small>
      </span>
      <span className="score">
        <b>{s.played}</b>
        <small>{minutes(s.minutes)}</small>
      </span>
    </li>
  )
}

export default function MusicStats({ device, days: chosen }) {
  const {
    data: fresh,
    error,
    last,
    reload,
  } = useLoad(() => api.musicStats({ device: device || undefined, days: chosen }), [device, chosen])
  const [all, setAll] = useState(false)
  // a listen over: the numbers again, a little later (several end together when tracks are skipped through)
  useEffect(() => {
    let t
    const off = onListen(() => {
      clearTimeout(t)
      t = setTimeout(reload, 2500)
    })
    return () => {
      off()
      clearTimeout(t)
    }
  }, [reload])
  // drawn by the period it was counted for: while another loads, the last one stays (dimmed), never with the new days
  const data = error ? null : (last?.data ?? null)
  const days = last?.deps[1] ?? chosen

  const series = useMemo(() => (data ? everyDay(data.since, days, data.days) : []), [data, days])
  const byMinutes = series.map((p) => ({ ...p, v: Math.round(p.d?.minutes || 0) }))
  const byFinished = series.map((p) => ({ ...p, v: p.d?.finished || 0 }))
  const describe = (p) => [
    dayLabel(p.key, true),
    p.d
      ? `${minutes(p.d.minutes)} · ${tracks(p.d.played)} · до конца ${p.d.finished} · пропущено ${p.d.skipped}`
      : 'музыка не играла',
  ]
  const t = data?.totals
  const artistMax = Math.max(1, ...(data?.artists || []).map((a) => a.minutes))

  if (error) return <Failed error={error} />
  if (!data) return <Loading />
  if (!t.listens) {
    return (
      <div className={`card${fresh ? '' : ' stale'}`} aria-busy={!fresh}>
        <Empty icon={Music2} title="Нет данных">
          За {periodWords(days)} музыка не играла.
        </Empty>
      </div>
    )
  }
  const songs = all ? data.songs : data.songs.slice(0, 15)
  return (
    <div
      className={fresh ? undefined : 'stale'}
      aria-busy={!fresh}
      style={{ display: 'flex', flexDirection: 'column', gap: 'var(--gap)' }}
    >
      <div className="stats">
        <div className="hero rasp stat">
          <span>Музыка за {periodWords(days)}</span>
          <b className="num">{minutes(t.minutes)}</b>
        </div>
        <div className="card stat">
          <span>В среднем за день</span>
          <b className="num">{minutes(t.minutes / days)}</b>
        </div>
        <div className="card stat">
          <span>Прослушано треков</span>
          <b className="num">{t.played}</b>
        </div>
        <div className="card stat">
          <span>До конца</span>
          <b className="num">{t.finished}</b>
        </div>
        <div className="card stat">
          <span>Пропущено</span>
          <b className="num">{t.skipped}</b>
        </div>
      </div>

      <div className="row top">
        <section className="card grow-1" aria-labelledby="m-min">
          <h2 id="m-min">Минуты по дням</h2>
          <Bars
            points={byMinutes}
            label={`Минуты музыки по дням за ${periodWords(days)}`}
            tick={dayTick(series, days)}
            describe={describe}
          />
        </section>
        <section className="card grow-1" aria-labelledby="m-fin">
          <h2 id="m-fin">Треки до конца по дням</h2>
          <Bars
            points={byFinished}
            label={`Треки, прослушанные до конца, по дням за ${periodWords(days)}`}
            tick={dayTick(series, days)}
            describe={describe}
          />
        </section>
      </div>

      <div className="row top">
        <section className="card grow-2" aria-labelledby="m-songs">
          <div className="card-head">
            <h2 id="m-songs">Треки</h2>
            <span className="faint" style={{ fontSize: 13 }}>
              Прослушано
            </span>
          </div>
          {data.songs.length ? (
            <>
              <ol className="songs">
                {songs.map((s, i) => (
                  <Song key={`${s.title}|${s.artist}`} s={s} rank={i + 1} />
                ))}
              </ol>
              {data.songs.length > songs.length && (
                <div>
                  <button type="button" className="btn" onClick={() => setAll(true)}>
                    Показать все {data.songs.length}
                  </button>
                </div>
              )}
            </>
          ) : (
            <p className="faint">Ни один трек не играл дольше 30 секунд</p>
          )}
        </section>
        {data.artists.length > 0 && (
          <section className="card grow-1" aria-labelledby="m-art">
            <h2 id="m-art">Исполнители</h2>
            <ul className="bars-list">
              {data.artists.slice(0, 12).map((a) => (
                <li key={a.artist}>
                  <span className="bl-top">
                    <span>{a.artist}</span>
                    <span>{minutes(a.minutes)}</span>
                  </span>
                  <span className="meter" aria-hidden="true">
                    <i style={{ width: `${(a.minutes / artistMax) * 100}%` }} />
                  </span>
                </li>
              ))}
            </ul>
          </section>
        )}
      </div>
    </div>
  )
}
