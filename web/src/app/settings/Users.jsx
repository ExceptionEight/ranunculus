// Settings → who may sign in.
import { useState } from 'react'
import { Trash2 } from 'lucide-react'
import { api } from '../../shared/api.js'
import { useAction, useLoad } from '../../shared/hooks.js'
import { Loading } from '../ui.jsx'
import Pane from './Pane.jsx'

export default function Users() {
  const { data, error, reload } = useLoad(() => api.users(), [])
  const [adding, setAdding] = useState(false)
  const [name, setName] = useState('')
  const [password, setPassword] = useState('')
  const [asked, setAsked] = useState(null) // the user whose removal is being confirmed
  const [add, addBusy, addError] = useAction(async () => {
    await api.addUser(name.trim(), password)
    setName('')
    setPassword('')
    setAdding(false)
    reload()
  })
  const [remove, removeBusy, removeError] = useAction(async (id) => {
    await api.removeUser(id)
    setAsked(null)
    reload()
  })
  return (
    <Pane title="Пользователи">
      <section className="card">
        {error && <p className="err">{error}</p>}
        {!data && !error && <Loading />}
        {data && (
          <div className="rows">
            {data.items.map((u) => (
              <div className="r" key={u.id} style={{ flexWrap: 'wrap' }}>
                <span className="avatar" aria-hidden="true">
                  {[...u.name][0]}
                </span>
                <div className="r-text">
                  <b>
                    {u.name}
                    {u.me && (
                      <span className="tag rasp" style={{ marginLeft: 8 }}>
                        Вы
                      </span>
                    )}
                  </b>
                  <span>{u.via ? `Вход через ${u.via}` : 'Вход по паролю'}</span>
                </div>
                {!u.me && asked !== u.id && (
                  <div className="r-ctl">
                    <button
                      type="button"
                      className="btn ghost icon sm"
                      onClick={() => setAsked(u.id)}
                      aria-label={`Удалить «${u.name}»`}
                      title="Удалить"
                    >
                      <Trash2 size={17} aria-hidden="true" />
                    </button>
                  </div>
                )}
                {asked === u.id && (
                  <div className="confirm" role="alertdialog" aria-label={`Удалить «${u.name}»?`}>
                    <span>Удалить «{u.name}»? Войти под этим именем будет нельзя.</span>
                    <div className="confirm-acts">
                      <button
                        type="button"
                        className="btn sm danger solid"
                        onClick={() => remove(u.id)}
                        disabled={removeBusy}
                      >
                        Удалить
                      </button>
                      <button type="button" className="btn sm" onClick={() => setAsked(null)}>
                        Отмена
                      </button>
                    </div>
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
        {removeError && <p className="err">{removeError}</p>}
        {adding ? (
          <form
            className="form"
            style={{ borderTop: '1px solid var(--line)', paddingTop: 16 }}
            onSubmit={(e) => {
              e.preventDefault()
              add()
            }}
          >
            <div className="form-pair">
              <label className="field-wrap">
                <span>Имя пользователя</span>
                <input
                  className="field"
                  autoComplete="off"
                  autoCapitalize="none"
                  spellCheck="false"
                  required
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
              </label>
              <label className="field-wrap">
                <span>Пароль</span>
                <input
                  className="field"
                  type="password"
                  autoComplete="new-password"
                  minLength={8}
                  required
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                />
                <span className="field-hint">Не короче 8 символов</span>
              </label>
            </div>
            {addError && <p className="err">{addError}</p>}
            <div className="form-acts">
              <button className="btn primary" type="submit" disabled={addBusy || !name.trim() || !password}>
                {addBusy ? 'Добавление…' : 'Добавить'}
              </button>
              <button className="btn" type="button" onClick={() => setAdding(false)}>
                Отмена
              </button>
            </div>
          </form>
        ) : (
          <div>
            <button type="button" className="btn" onClick={() => setAdding(true)}>
              Добавить пользователя
            </button>
          </div>
        )}
      </section>
    </Pane>
  )
}
