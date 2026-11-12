// Settings → Модели. The connections: a service's address and its key, given once (OpenRouter, a server at home).
// The roles: who hears (recognition), answers (a language model), speaks (a voice) and finds past talks by meaning
// (embeddings) — each one model of one of the connections, used by every station. Keys go in and never come back.
import { useEffect, useMemo, useRef, useState } from 'react'
import { AudioWaveform, Brain, Ear, KeyRound, MessageSquareText, Play, Plug, Plus } from 'lucide-react'
import { api } from '../../shared/api.js'
import { useAction } from '../../shared/hooks.js'
import { useStore } from '../../shared/store.js'
import { away, cloudNote, nothingChosen, ROLE_ABOUT, ROLE_NONE, ROLE_TITLE, ROLES } from '../../shared/providers.js'
import { Loading, Sheet } from '../ui.jsx'

const OPENROUTER = { label: 'OpenRouter', name: 'OpenRouter', url: 'https://openrouter.ai/api/v1', needsKey: true }
// host.docker.internal: this computer as the server's container reaches it (compose.yaml names it)
const SERVERS = [
  { label: 'Ollama на этом компьютере', name: 'Ollama', url: 'http://host.docker.internal:11434/v1' },
  { label: 'OpenAI', name: 'OpenAI', url: 'https://api.openai.com/v1', needsKey: true },
  { label: 'Другой сервер с API, совместимым с OpenAI', name: '', url: '' },
]

const seconds = (s) => s.toLocaleString('ru-RU', { maximumFractionDigits: 1 })
const price = (x) =>
  x == null ? '?' : x < 0.01 ? x.toFixed(3).replace('.', ',') : x.toLocaleString('ru-RU', { maximumFractionDigits: 2 })

/** What a model of a list costs and cannot do, in a line. */
function about(m, role) {
  const parts = []
  if (role === 'llm' && m.in != null) parts.push(`$${price(m.in)} / $${price(m.out)} за 1 млн токенов`)
  if (role === 'embeddings' && m.in != null) parts.push(`$${price(m.in)} за 1 млн токенов`)
  if (m.tools === false) parts.push('не умеет ставить таймеры и включать музыку')
  return parts.join(' · ')
}

// ---------------------------------------------------------------- the key, first of all

/** OpenRouter without a key: the one thing to do before anything works. */
function FirstKey({ connection, fresh }) {
  const [key, setKey] = useState('')
  const [save, busy, error] = useAction(() => api.changeConnection(connection.id, { key: key.trim() }))
  return (
    <section className="card key-card" aria-labelledby="first-key">
      <div className="card-head">
        <div className="card-title">
          <span className="ico" aria-hidden="true">
            <KeyRound size={20} />
          </span>
          <div className="r-text">
            <b id="first-key">Ключ OpenRouter</b>
            <span>
              Один ключ на всё: распознавание, ответы, голос и память.
              {fresh && ' Модели подберутся сами, потом их можно сменить.'}
            </span>
          </div>
        </div>
      </div>
      <form
        className="key-row"
        onSubmit={(e) => {
          e.preventDefault()
          save()
        }}
      >
        <input
          className="field"
          type="password"
          autoComplete="off"
          placeholder="sk-or-v1-…"
          aria-label="Ключ OpenRouter"
          value={key}
          onChange={(e) => setKey(e.target.value)}
        />
        <button className="btn primary" disabled={busy || !key.trim()}>
          Сохранить
        </button>
      </form>
      <p className="field-hint">
        Ключ создаётся на{' '}
        <a href="https://openrouter.ai/keys" target="_blank" rel="noreferrer">
          openrouter.ai/keys
        </a>
        . Он хранится на сервере и обратно в браузер не возвращается.
      </p>
      {error && <p className="err">{error}</p>}
    </section>
  )
}

// ---------------------------------------------------------------- connections

function ConnectionEditor({ connection, haveOpenRouter, onClose }) {
  const presets = useMemo(() => (haveOpenRouter ? SERVERS : [OPENROUTER, ...SERVERS]), [haveOpenRouter])
  const [preset, setPreset] = useState(0)
  const [f, setF] = useState(() =>
    connection
      ? { name: connection.name, url: connection.url, key: '' }
      : { name: presets[0].name, url: presets[0].url, key: '' },
  )
  const [clearKey, setClearKey] = useState(false)
  const set = (k) => (v) => setF((x) => ({ ...x, [k]: v }))
  const choosePreset = (i) => {
    setPreset(i)
    setF((x) => ({ ...x, name: presets[i].name, url: presets[i].url }))
  }
  const [save, busy, error] = useAction(async () => {
    const body = { name: f.name, url: f.url }
    const key = f.key.trim()
    if (key) body.key = key
    else if (clearKey) body.key = ''
    await (connection ? api.changeConnection(connection.id, body) : api.addConnection(body))
    onClose()
  })
  const needsKey = !connection && presets[preset].needsKey && !f.key.trim()
  return (
    <Sheet
      title={connection ? connection.name : 'Новое подключение'}
      onClose={onClose}
      onSubmit={save}
      foot={
        <>
          <span className="spacer" />
          <button type="button" className="btn" onClick={onClose}>
            Отмена
          </button>
          <button className="btn primary" disabled={busy || !f.name.trim() || !f.url.trim() || needsKey}>
            {connection ? 'Сохранить' : 'Добавить'}
          </button>
        </>
      }
    >
      {!connection && (
        <label className="field-wrap">
          <span>Сервис</span>
          <select className="field" value={preset} onChange={(e) => choosePreset(Number(e.target.value))}>
            {presets.map((p, i) => (
              <option key={p.label} value={i}>
                {p.label}
              </option>
            ))}
          </select>
        </label>
      )}
      <label className="field-wrap">
        <span>Название</span>
        <input className="field" required maxLength={60} value={f.name} onChange={(e) => set('name')(e.target.value)} />
      </label>
      <label className="field-wrap">
        <span>Адрес API</span>
        <input
          className="field"
          required
          placeholder="https://…/v1"
          value={f.url}
          onChange={(e) => set('url')(e.target.value)}
        />
      </label>
      <div className="field-wrap">
        <label htmlFor="cn-key">
          <span className="label">Ключ API</span>
        </label>
        <input
          id="cn-key"
          className="field"
          type="password"
          autoComplete="off"
          placeholder={
            connection?.key_set && !clearKey
              ? `Задан${connection.key_tail ? `: …${connection.key_tail}` : ''}. Оставьте пустым, чтобы не менять`
              : 'Для локальных моделей оставьте пустым'
          }
          value={f.key}
          onChange={(e) => set('key')(e.target.value)}
        />
        {connection?.key_set && !f.key && (
          <label className="check">
            <input type="checkbox" checked={clearKey} onChange={(e) => setClearKey(e.target.checked)} /> Удалить ключ
          </label>
        )}
      </div>
      {away(f.url) && <p className="notice info">Облачный провайдер может хранить ваши данные.</p>}
      {error && <p className="err">{error}</p>}
    </Sheet>
  )
}

/** `used`: the roles that use it — it stays while they do. */
function ConnectionItem({ c, used, onEdit }) {
  const [confirm, setConfirm] = useState(false)
  const [del, deleting, error] = useAction(() => api.deleteConnection(c.id))
  const key = c.key_set ? (c.key_tail ? `ключ …${c.key_tail}` : 'ключ задан') : c.cloud ? 'ключ не задан' : 'без ключа'
  return (
    <li className="prov">
      <div className="r-text" style={{ flex: '1 1 260px' }}>
        <b>
          {c.name}
          {c.cloud && <span className="tag rasp">Облако</span>}
        </b>
        <span>{[c.url, key].join(' · ')}</span>
      </div>
      <div className="prov-acts">
        <button type="button" className="btn sm" onClick={onEdit}>
          Изменить
        </button>
        {!used.length && !confirm && (
          <button type="button" className="btn sm ghost danger" onClick={() => setConfirm(true)}>
            Удалить
          </button>
        )}
      </div>
      {confirm && (
        <div className="confirm" role="alertdialog" aria-label={`Удалить «${c.name}»?`}>
          <span>Удалить «{c.name}»?</span>
          <div className="confirm-acts">
            <button type="button" className="btn sm danger solid" disabled={deleting} onClick={() => del()} autoFocus>
              Удалить
            </button>
            <button type="button" className="btn sm" onClick={() => setConfirm(false)}>
              Отмена
            </button>
          </div>
        </div>
      )}
      {error && <p className="err r-full">{error}</p>}
    </li>
  )
}

// ---------------------------------------------------------------- roles

/** The connection's models for the role, the cheapest first (or by name); those that cannot call tools are marked —
 * the assistant needs them for timers, music, the volume. Loaded as it opens; a server that lists nothing leaves the
 * name to be typed. */
function ModelPicker({ connection, role, value, onPick, onList }) {
  const [items, setItems] = useState(null)
  const [filter, setFilter] = useState('')
  const [load, busy, error] = useAction(async () => {
    const list = (await api.models(connection, role)).items
    setItems(list)
    onList(list)
  })
  useEffect(() => {
    load()
    // eslint-disable-next-line react-hooks/exhaustive-deps -- once: the picker is made anew for another connection
  }, [])
  const shown = useMemo(() => {
    const f = filter.trim().toLowerCase()
    return (items || [])
      .filter((m) => !f || m.id.toLowerCase().includes(f) || m.name.toLowerCase().includes(f))
      .slice(0, 80)
  }, [items, filter])
  if (busy && !items) return <p className="faint">Загрузка списка моделей…</p>
  if (error)
    return (
      <p className="field-hint">
        Список моделей не получен ({error}). Название можно ввести вручную.{' '}
        <button type="button" className="link-btn" onClick={() => load()}>
          Повторить
        </button>
      </p>
    )
  if (!items?.length) return null
  return (
    <div className="models-list">
      {items.length > 8 && (
        <input
          className="field sm"
          placeholder="Поиск по названию"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          aria-label="Поиск модели"
        />
      )}
      <ul role="listbox" aria-label="Модели">
        {shown.map((m) => (
          <li key={m.id}>
            <button type="button" role="option" aria-selected={m.id === value} onClick={() => onPick(m)}>
              <span className="mid">{m.id}</span>
              <span className="mmeta">{about(m, role)}</span>
            </button>
          </li>
        ))}
        {!shown.length && (
          <li className="faint" style={{ padding: 10 }}>
            Ничего не найдено
          </li>
        )}
      </ul>
    </div>
  )
}

/** A voice played here: stopped, and its file let go, when another starts or the page is left. */
function useVoice() {
  const voice = useRef(null) // {audio, url}
  const alive = useRef(true)
  const hush = () => {
    const v = voice.current
    voice.current = null
    if (!v) return
    v.audio.pause()
    v.audio.removeAttribute('src')
    URL.revokeObjectURL(v.url)
  }
  useEffect(() => {
    alive.current = true
    return () => {
      alive.current = false
      hush()
    }
  }, [])
  const play = (blob) => {
    hush()
    if (!alive.current) return // left while it was being made: nobody to hear it
    const url = URL.createObjectURL(blob)
    const audio = new Audio(url)
    const v = { audio, url }
    voice.current = v
    audio.onended = () => voice.current === v && hush()
    audio.play().catch(() => voice.current === v && hush())
  }
  return { play, hush }
}

/** «Проверить»: recognition hears, a model answers, a voice speaks (played here), the memory gets its vectors. `as`:
 * the role as it would be, not as it is in use. */
function useCheck(role) {
  const [result, setResult] = useState(null)
  const voice = useVoice()
  const [run, busy, error] = useAction(async (as) => {
    setResult(null)
    voice.hush()
    const r = await api.checkRole(role, as)
    if (role === 'tts') {
      voice.play(r.audio)
      setResult(`Работает: звучит пример голоса, синтез занял ${seconds(r.seconds)} с`)
    } else if (role === 'llm') {
      setResult(`Работает: ответ «${r.text}» за ${seconds(r.seconds)} с`)
    } else if (role === 'asr') {
      setResult(`Работает: распознано «${r.text}» за ${seconds(r.seconds)} с, сказано «${r.expected}»`)
    } else {
      setResult(`Работает: вектор из ${r.size} чисел за ${seconds(r.seconds)} с`)
    }
  })
  return { run, busy, result, error }
}

function RoleEditor({ role, chosen, connections, onClose }) {
  const [f, setF] = useState(() => ({
    connection: chosen?.connection ?? (connections.find((c) => c.key_set) || connections[0])?.id ?? '',
    model: chosen?.model ?? '',
    voice: chosen?.voice ?? '',
    speed: chosen?.speed ?? '',
  }))
  const [list, setList] = useState([]) // the connection's models, once they are known
  const set = (k) => (v) => setF((x) => ({ ...x, [k]: v }))
  const connection = connections.find((c) => c.id === Number(f.connection))
  const voices = list.find((m) => m.id === f.model)?.voices || []
  const body = () => {
    const b = { connection: Number(f.connection), model: f.model.trim() }
    if (role === 'tts') Object.assign(b, { voice: f.voice.trim(), speed: f.speed === '' ? null : Number(f.speed) })
    return b
  }
  const pick = (m) =>
    setF((x) => ({
      ...x,
      model: m.id,
      // another model, other voices: its first one, to be changed below
      voice: m.voices?.length && !m.voices.includes(x.voice) ? m.voices[0] : x.voice,
    }))
  const [save, busy, error] = useAction(async () => {
    await api.setRole(role, body())
    onClose(true)
  })
  const listen = useCheck('tts')
  return (
    <Sheet
      title={ROLE_TITLE[role]}
      onClose={() => onClose()}
      onSubmit={save}
      foot={
        <>
          {role === 'tts' && (
            <button
              type="button"
              className="btn"
              disabled={listen.busy || !connection}
              onClick={() => listen.run(body())}
            >
              <Play size={15} aria-hidden="true" /> {listen.busy ? 'Синтез…' : 'Послушать'}
            </button>
          )}
          <span className="spacer" />
          <button type="button" className="btn" onClick={() => onClose()}>
            Отмена
          </button>
          <button className="btn primary" disabled={busy || !connection}>
            Сохранить
          </button>
        </>
      }
    >
      <label className="field-wrap">
        <span>Подключение</span>
        <select
          className="field"
          value={f.connection}
          onChange={(e) => {
            setList([])
            setF((x) => ({ ...x, connection: e.target.value, model: '', voice: '' }))
          }}
        >
          {connections.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
              {c.cloud && !c.key_set ? ' (ключ не задан)' : ''}
            </option>
          ))}
        </select>
      </label>
      <div className="field-wrap">
        <label htmlFor="role-model">
          <span className="label">Модель</span>
        </label>
        <input
          id="role-model"
          className="field"
          maxLength={200}
          placeholder="Выберите из списка или введите название"
          value={f.model}
          onChange={(e) => set('model')(e.target.value)}
        />
        {connection && (
          <ModelPicker
            key={connection.id}
            connection={connection.id}
            role={role}
            value={f.model}
            onPick={pick}
            onList={setList}
          />
        )}
      </div>
      {role === 'tts' && (
        <div className="form-pair">
          <label className="field-wrap">
            <span>Голос</span>
            {voices.length ? (
              <select className="field" value={f.voice} onChange={(e) => set('voice')(e.target.value)}>
                {!voices.includes(f.voice) && <option value={f.voice}>{f.voice || 'Не выбран'}</option>}
                {voices.map((v) => (
                  <option key={v} value={v}>
                    {v}
                  </option>
                ))}
              </select>
            ) : (
              <input
                className="field"
                maxLength={100}
                placeholder="Если у модели их несколько"
                value={f.voice}
                onChange={(e) => set('voice')(e.target.value)}
              />
            )}
          </label>
          <label className="field-wrap">
            <span>Скорость</span>
            <input
              className="field"
              type="number"
              min={0.5}
              max={3}
              step={0.05}
              placeholder="Как у модели"
              value={f.speed}
              onChange={(e) => set('speed')(e.target.value)}
            />
          </label>
        </div>
      )}
      {role === 'tts' && (listen.result || listen.error) && (
        <p className={`prov-note${listen.error ? ' err' : ''}`} role="status">
          {listen.error ? `Не работает: ${listen.error}` : listen.result}
        </p>
      )}
      {connection?.cloud && <p className="notice info">{cloudNote(role)}</p>}
      {error && <p className="err">{error}</p>}
    </Sheet>
  )
}

const ROLE_ICON = { asr: Ear, llm: MessageSquareText, tts: AudioWaveform, embeddings: Brain }

/** `saved`: counts its saves here — each is checked at once (a wrong model name or key is seen here, not heard from
 * the station), but for a voice: its check is a sound. */
function Role({ role, chosen, connections, saved, onEdit }) {
  const Icon = ROLE_ICON[role]
  const connection = chosen && connections.find((c) => c.id === chosen.connection)
  const check = useCheck(role)
  const [clear, clearing, clearError] = useAction(() => api.clearRole(role))
  useEffect(() => {
    if (saved && role !== 'tts') check.run()
    // eslint-disable-next-line react-hooks/exhaustive-deps -- once per save: `check` is new at every render
  }, [saved])
  const details = chosen && [
    connection?.name,
    chosen.voice && `голос ${chosen.voice}`,
    chosen.speed && `скорость ${String(chosen.speed).replace('.', ',')}`,
  ]
  return (
    <section className="card" aria-labelledby={`role-${role}`}>
      <div className="card-head">
        <div className="card-title">
          <span className="ico" aria-hidden="true">
            <Icon size={20} />
          </span>
          <div className="r-text">
            <b id={`role-${role}`}>{ROLE_TITLE[role]}</b>
            <span>{ROLE_ABOUT[role]}</span>
          </div>
        </div>
      </div>
      <div className="prov" style={{ borderTop: 0, paddingBottom: 0 }}>
        <div className="r-text" style={{ flex: '1 1 260px' }}>
          {chosen ? (
            <>
              <b>
                {chosen.model || 'Модель сервера'}
                {chosen.cloud && <span className="tag rasp">Облако</span>}
              </b>
              <span>{details.filter(Boolean).join(' · ')}</span>
            </>
          ) : (
            <span>{ROLE_NONE[role]}</span>
          )}
        </div>
        <div className="prov-acts">
          {chosen && (
            <button type="button" className="btn sm" disabled={check.busy} onClick={() => check.run()}>
              {check.busy ? 'Проверка…' : 'Проверить'}
            </button>
          )}
          <button type="button" className={`btn sm${chosen ? '' : ' primary'}`} onClick={onEdit}>
            {chosen ? 'Изменить' : 'Выбрать'}
          </button>
          {chosen && role === 'embeddings' && (
            <button type="button" className="btn sm ghost" disabled={clearing} onClick={() => clear()}>
              Не использовать
            </button>
          )}
        </div>
        {(check.result || check.error) && (
          <p className={`prov-note${check.error ? ' err' : ''}`} role="status">
            {check.error ? `Не работает: ${check.error}` : check.result}
          </p>
        )}
        {clearError && <p className="err r-full">{clearError}</p>}
      </div>
    </section>
  )
}

export default function Connections({ Pane }) {
  const providers = useStore((s) => s.providers)
  const [edit, setEdit] = useState(null) // {role} | {connection: one of them | null (a new one)}
  const [saved, setSaved] = useState({}) // role → how many times saved here
  const closeRole = (role) => (ok) => {
    setEdit(null)
    if (ok) setSaved((s) => ({ ...s, [role]: (s[role] || 0) + 1 }))
  }
  if (!providers)
    return (
      <Pane title="Модели">
        <Loading />
      </Pane>
    )
  const { connections, roles } = providers
  const keyless = connections.find((c) => c.openrouter && !c.key_set)
  const usedBy = (c) => ROLES.filter((r) => roles[r]?.connection === c.id)
  return (
    <Pane title="Модели">
      <p className="muted" style={{ padding: '0 4px', marginTop: -8 }}>
        Кто распознаёт речь, отвечает, озвучивает ответы и ищет в памяти.
      </p>
      {keyless && <FirstKey connection={keyless} fresh={nothingChosen(providers)} />}
      {ROLES.map((role) => (
        <Role
          key={role}
          role={role}
          chosen={roles[role]}
          connections={connections}
          saved={saved[role] || 0}
          onEdit={() => setEdit({ role })}
        />
      ))}
      <section className="card" aria-labelledby="connections-h">
        <div className="card-head">
          <div className="card-title">
            <span className="ico" aria-hidden="true">
              <Plug size={20} />
            </span>
            <div className="r-text">
              <b id="connections-h">Подключения</b>
              <span>Адрес сервиса и ключ к нему. Свой сервер с API, как у OpenAI, добавляется сюда же.</span>
            </div>
          </div>
          <button type="button" className="btn sm" onClick={() => setEdit({ connection: null })}>
            <Plus size={16} aria-hidden="true" /> Добавить
          </button>
        </div>
        {connections.length > 0 ? (
          <ul className="provs" style={{ listStyle: 'none', margin: 0, padding: 0 }}>
            {connections.map((c) => (
              <ConnectionItem key={c.id} c={c} used={usedBy(c)} onEdit={() => setEdit({ connection: c })} />
            ))}
          </ul>
        ) : (
          <p className="faint">Подключений нет: добавьте OpenRouter или свой сервер.</p>
        )}
      </section>
      {edit?.role && (
        <RoleEditor
          role={edit.role}
          chosen={roles[edit.role]}
          connections={connections}
          onClose={closeRole(edit.role)}
        />
      )}
      {edit && 'connection' in edit && (
        <ConnectionEditor
          connection={edit.connection}
          haveOpenRouter={connections.some((c) => c.openrouter)}
          onClose={() => setEdit(null)}
        />
      )}
    </Pane>
  )
}
