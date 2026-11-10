// Settings → this account: the password, signing out.
import { useState } from 'react'
import { LogOut } from 'lucide-react'
import { api } from '../../shared/api.js'
import { useAction } from '../../shared/hooks.js'
import { signedOut, useStore } from '../../shared/store.js'
import Pane from './Pane.jsx'

function NewPassword({ onDone }) {
  const me = useStore((s) => s.auth?.user?.name ?? '')
  const [current, setCurrent] = useState('')
  const [password, setPassword] = useState('')
  const [again, setAgain] = useState('')
  const [save, busy, error] = useAction(async () => {
    if (password !== again) throw new Error('Пароли не совпадают')
    await api.changePassword(current, password)
    onDone(true)
  })
  return (
    <form
      className="form"
      style={{ width: '100%' }}
      onSubmit={(e) => {
        e.preventDefault()
        save()
      }}
    >
      {/* for password managers: whose password this is */}
      <input type="text" name="username" autoComplete="username" value={me} readOnly hidden />
      <label className="field-wrap">
        <span>Текущий пароль</span>
        <input
          className="field"
          type="password"
          autoComplete="current-password"
          required
          value={current}
          onChange={(e) => setCurrent(e.target.value)}
        />
      </label>
      <div className="form-pair">
        <label className="field-wrap">
          <span>Новый пароль</span>
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
        <label className="field-wrap">
          <span>Повторите пароль</span>
          <input
            className="field"
            type="password"
            autoComplete="new-password"
            required
            value={again}
            onChange={(e) => setAgain(e.target.value)}
          />
        </label>
      </div>
      {error && <p className="err">{error}</p>}
      <div className="form-acts">
        <button className="btn primary" type="submit" disabled={busy || !current || !password || !again}>
          {busy ? 'Сохранение…' : 'Сменить пароль'}
        </button>
        <button className="btn" type="button" onClick={() => onDone(false)}>
          Отмена
        </button>
      </div>
    </form>
  )
}

export default function Account() {
  const name = useStore((s) => s.auth?.user?.name ?? '')
  const via = useStore((s) => s.auth?.user?.via)
  const [changing, setChanging] = useState(false)
  const [changed, setChanged] = useState(false)
  const [out, busy, error] = useAction(async (all) => {
    await api.logout(all)
    signedOut()
  })
  return (
    <Pane title="Аккаунт">
      <section className="card">
        <div className="r" style={{ borderTop: 0, paddingTop: 0 }}>
          <span className="avatar" style={{ width: 52, height: 52, fontSize: 20 }} aria-hidden="true">
            {[...name][0]}
          </span>
          <div className="r-text">
            <b style={{ fontSize: 18 }}>{name}</b>
            <span>{via ? `Вход через ${via}` : 'Вход по паролю'}</span>
          </div>
        </div>
        <div className="rows">
          <div className={`r${changing ? ' wide' : ''}`}>
            <div className="r-text">
              <b>Пароль</b>
              {via ? (
                <span>Меняется в {via}</span>
              ) : changed ? (
                <span className="ok">Пароль изменён, на других устройствах выполнен выход</span>
              ) : (
                !changing && <span>После смены пароля на других устройствах нужно будет войти заново</span>
              )}
            </div>
            {!via &&
              (changing ? (
                <NewPassword
                  onDone={(done) => {
                    setChanging(false)
                    setChanged(done)
                  }}
                />
              ) : (
                <div className="r-ctl">
                  <button className="btn" onClick={() => (setChanged(false), setChanging(true))}>
                    Сменить
                  </button>
                </div>
              ))}
          </div>
          <div className="r">
            <div className="r-text">
              <b>Выйти</b>
              <span>На этом устройстве</span>
            </div>
            <div className="r-ctl">
              <button className="btn" onClick={() => out(false)} disabled={busy}>
                <LogOut size={17} aria-hidden="true" />
                Выйти
              </button>
            </div>
          </div>
          <div className="r">
            <div className="r-text">
              <b>Выйти на всех устройствах</b>
            </div>
            <div className="r-ctl">
              <button className="btn danger" onClick={() => out(true)} disabled={busy}>
                Выйти везде
              </button>
            </div>
          </div>
        </div>
        {error && <p className="err">{error}</p>}
      </section>
    </Pane>
  )
}
