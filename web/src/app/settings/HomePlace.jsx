// Settings → the home: its city (the weather when none is named) and its time zone.
import { useEffect, useState } from 'react'
import { api } from '../../shared/api.js'
import { useAction } from '../../shared/hooks.js'
import { useStore } from '../../shared/store.js'
import { Loading } from '../ui.jsx'
import Pane from './Pane.jsx'

const ZONES = typeof Intl.supportedValuesOf === 'function' ? Intl.supportedValuesOf('timeZone') : []

export default function HomePlace() {
  const home = useStore((s) => s.settings?.home ?? null)
  const [city, setCity] = useState('')
  const [done, setDone] = useState(false)
  const kept = home?.city
  useEffect(() => {
    if (kept != null) setCity(kept)
  }, [kept])
  const [save, busy, error] = useAction(async () => {
    await api.changeSettings({ home: { city } })
    setDone(true)
  })
  const [setZone, zoneBusy, zoneError] = useAction((timezone) => api.changeSettings({ home: { timezone } }))
  if (!home)
    return (
      <Pane title="Дом">
        <Loading />
      </Pane>
    )
  return (
    <Pane title="Дом">
      <section className="card" aria-labelledby="home-city">
        <h3 id="home-city">Город</h3>
        <p className="about">Для прогноза погоды, когда город не назван</p>
        <form
          className="form"
          onSubmit={(e) => {
            e.preventDefault()
            save()
          }}
        >
          <label className="sr-only" htmlFor="city">
            Город
          </label>
          <input
            id="city"
            className="field"
            maxLength={100}
            placeholder="Например, Казань"
            value={city}
            onChange={(e) => {
              setCity(e.target.value)
              setDone(false)
            }}
          />
          <div className="form-acts">
            <button className="btn primary" disabled={busy || city.trim() === home.city}>
              Сохранить
            </button>
            {done && !busy && <span className="ok">Сохранено</span>}
          </div>
          {error && <p className="err">{error}</p>}
        </form>
      </section>

      <section className="card" aria-labelledby="home-zone">
        <h3 id="home-zone">Часовой пояс</h3>
        <p className="about">Время в ответах, будильники и статистика. Меняется вместе с городом</p>
        <label className="sr-only" htmlFor="zone">
          Часовой пояс
        </label>
        <select
          id="zone"
          className="field"
          value={home.zone}
          disabled={zoneBusy}
          onChange={(e) => setZone(e.target.value)}
        >
          {(ZONES.includes(home.zone) ? ZONES : [home.zone, ...ZONES]).map((z) => (
            <option key={z} value={z}>
              {z}
            </option>
          ))}
        </select>
        {zoneError && <p className="err">{zoneError}</p>}
      </section>
    </Pane>
  )
}
