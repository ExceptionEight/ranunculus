// Settings → how the assistant talks: what it is asked to keep in mind, and how the user is spoken of.
import { useEffect, useState } from 'react'
import { api } from '../../shared/api.js'
import { useAction } from '../../shared/hooks.js'
import { useStore } from '../../shared/store.js'
import { Seg } from '../ui.jsx'
import Pane from './Pane.jsx'

const ADDRESS = [
  ['neutral', 'Не указывать'],
  ['male', 'Мужской'],
  ['female', 'Женский'],
]

export default function Talk() {
  const saved = useStore((s) => s.settings?.preferences?.text ?? null)
  const address = useStore((s) => s.settings?.preferences?.address ?? null)
  const [setAddress, , addressError] = useAction((v) => api.changeSettings({ preferences: { address: v } }))
  const [text, setText] = useState('')
  const [done, setDone] = useState(false)
  useEffect(() => {
    if (saved != null) setText(saved)
  }, [saved])
  const [save, busy, error] = useAction(async () => {
    await api.changeSettings({ preferences: { text } })
    setDone(true)
  })
  const n = [...text].length
  return (
    <Pane title="Общение">
      <section className="card">
        <div className="rows">
          <div className="r stack-m">
            <div className="r-text">
              <b>Род в обращении</b>
              <span>«Ты спросил» или «ты спросила»</span>
            </div>
            <div className="r-ctl">
              {address && <Seg label="Род в обращении" items={ADDRESS} value={address} onChange={setAddress} />}
            </div>
            {addressError && <p className="err r-full">{addressError}</p>}
          </div>
        </div>
      </section>
      <section className="card">
        <p className="muted">
          Как ассистенту с вами общаться: обращение, длина ответов, темы, которых лучше не касаться. Учитывается во всех
          ответах на всех колонках.
        </p>
        <form
          className="form"
          onSubmit={(e) => {
            e.preventDefault()
            save()
          }}
        >
          <label className="sr-only" htmlFor="prefs">
            Пожелания
          </label>
          <textarea
            id="prefs"
            className="field"
            rows={8}
            maxLength={2000}
            placeholder="Например: отвечай коротко, обращайся на «ты», меня зовут Саша."
            value={text}
            onChange={(e) => {
              setText(e.target.value)
              setDone(false)
            }}
          />
          <div className="form-acts">
            <button className="btn primary" disabled={busy || saved == null || text.trim() === saved}>
              Сохранить
            </button>
            {done && !busy && <span className="ok">Сохранено</span>}
            <span className="counter num">{n} / 2000</span>
          </div>
          {error && <p className="err">{error}</p>}
        </form>
      </section>
    </Pane>
  )
}
