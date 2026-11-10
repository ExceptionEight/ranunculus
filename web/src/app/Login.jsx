// Signing in (the server's web/auth.rs, oidc.rs): a name and a password, and/or «Войти через …» an OIDC provider.
// No user yet: the form makes the first one (and signs in as it). A connection that may not take a password (plain
// HTTP from another device): what to open instead. Back from the provider refused: ?login=<word>, said once, then
// gone from the address.
import { useState } from 'react'
import { api } from '../shared/api.js'
import { signedIn } from '../shared/store.js'

/** What /?login=<word> means (oidc.rs `refused`). */
function refusal(word, name) {
  return {
    provider: `${name[0].toUpperCase() + name.slice(1)} не отвечает. Попробуйте позже.`,
    expired: 'Время входа истекло. Попробуйте ещё раз.',
    denied: 'Вход отменён.',
    group: 'У этой учётной записи нет доступа.',
    failed: `Не удалось войти через ${name}. Подробности в журнале сервера.`,
    insecure: 'Вход возможен только по защищённому соединению.',
    off: 'Вход через внешний сервис выключен.',
  }[word]
}

function takeLoginWord() {
  const q = new URLSearchParams(location.search)
  const word = q.get('login')
  if (word) {
    q.delete('login')
    history.replaceState(null, '', location.pathname + (q.size ? `?${q}` : ''))
  }
  return word
}

function Field({ label, hint, ...input }) {
  return (
    <label className="field-wrap">
      <span>{label}</span>
      <input className="field" required {...input} />
      {hint && <span className="field-hint">{hint}</span>}
    </label>
  )
}

export default function Login({ auth }) {
  const [name, setName] = useState('')
  const [password, setPassword] = useState('')
  const [again, setAgain] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState(null)
  const [word] = useState(takeLoginWord)
  const said = word && refusal(word, auth.oidc?.name || 'внешний сервис')
  const first = auth.setup
  const submit = async (e) => {
    e.preventDefault()
    if (first && password !== again) return setError('Пароли не совпадают')
    setBusy(true)
    setError(null)
    try {
      const r = first ? await api.register(name.trim(), password) : await api.login(name.trim(), password)
      signedIn(r.user)
    } catch (err) {
      setError(err.message)
    } finally {
      setBusy(false)
    }
  }
  const provider = auth.oidc && (
    <a
      className={`btn tall block${auth.password && auth.secure && !first ? '' : ' primary'}`}
      href={`${auth.oidc.start}?to=${encodeURIComponent(location.pathname)}`}
    >
      Войти через {auth.oidc.name}
    </a>
  )
  const nameField = (
    <Field
      label="Имя пользователя"
      name="username"
      autoComplete="username"
      autoCapitalize="none"
      spellCheck="false"
      autoFocus
      value={name}
      onChange={(e) => setName(e.target.value)}
    />
  )
  let lead = null
  let body
  if (!auth.password) {
    body = provider || <p className="notice">Вход не настроен на сервере.</p>
  } else if (!auth.secure) {
    body = (
      <>
        {provider}
        <p className="notice info">
          Вход по паролю доступен только по защищённому соединению. Откройте этот адрес через https://
        </p>
      </>
    )
  } else if (first) {
    lead = 'Создайте первого пользователя'
    body = (
      <>
        {nameField}
        <Field
          label="Пароль"
          hint="Не короче 8 символов"
          type="password"
          name="new-password"
          autoComplete="new-password"
          minLength={8}
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        <Field
          label="Повторите пароль"
          type="password"
          name="new-password-again"
          autoComplete="new-password"
          value={again}
          onChange={(e) => setAgain(e.target.value)}
        />
        {error && (
          <p className="err" role="alert">
            {error}
          </p>
        )}
        <button className="btn primary tall block" type="submit" disabled={busy || !name.trim() || !password || !again}>
          {busy ? 'Создание…' : 'Создать и войти'}
        </button>
        {provider && <p className="or">или</p>}
        {provider}
      </>
    )
  } else {
    body = (
      <>
        {nameField}
        <Field
          label="Пароль"
          type="password"
          name="password"
          autoComplete="current-password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        {error && (
          <p className="err" role="alert">
            {error}
          </p>
        )}
        <button className="btn primary tall block" type="submit" disabled={busy || !name.trim() || !password}>
          {busy ? 'Вход…' : 'Войти'}
        </button>
        {provider && <p className="or">или</p>}
        {provider}
      </>
    )
  }
  return (
    <main className="login">
      <form className="card login-card" onSubmit={submit}>
        <div className="login-brand">
          <img src="/favicon.svg" alt="" />
          <div>
            <h1>Ranunculus</h1>
            {lead && <p>{lead}</p>}
          </div>
        </div>
        {said && (
          <p className="notice" role="alert">
            {said}
          </p>
        )}
        {body}
      </form>
    </main>
  )
}
