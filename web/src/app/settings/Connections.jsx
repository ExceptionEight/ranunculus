// Settings → Модели: who answers (language models), who hears (recognition), whose voice (synthesis). Each kind: its
// services, one of them in use by every station. Keys go in and never come back.
import { useEffect, useMemo, useRef, useState } from 'react'
import { AudioWaveform, Check, Ear, MessageSquareText, Plus } from 'lucide-react'
import { api } from '../../shared/api.js'
import { useAction } from '../../shared/hooks.js'
import { useStore } from '../../shared/store.js'
import { cloudNote, KIND_ABOUT, KIND_TITLE, KINDS } from '../../shared/providers.js'
import { Loading, Sheet } from '../ui.jsx'

// host.docker.internal: this computer as the server's container reaches it (compose.yaml names it)
const PRESETS = {
  llm: [
    { label: 'OpenRouter', name: 'OpenRouter', url: 'https://openrouter.ai/api/v1', needsKey: true },
    { label: 'Ollama на этом компьютере', name: 'Ollama', url: 'http://host.docker.internal:11434/v1' },
    { label: 'Другой сервер с API, совместимым с OpenAI', name: '', url: '' },
  ],
  asr: [
    {
      label: 'OpenAI',
      name: 'OpenAI',
      url: 'https://api.openai.com/v1',
      model: 'gpt-4o-mini-transcribe',
      needsKey: true,
    },
    { label: 'Другой сервер с API, совместимым с OpenAI', name: '', url: '' },
  ],
  tts: [
    {
      label: 'OpenAI',
      name: 'OpenAI',
      url: 'https://api.openai.com/v1',
      model: 'gpt-4o-mini-tts',
      voice: 'nova',
      speed: 1.2,
      needsKey: true,
    },
    { label: 'Другой сервер с API, совместимым с OpenAI', name: '', url: '' },
  ],
}

const price = (x) =>
  x == null ? '?' : x < 0.01 ? x.toFixed(3).replace('.', ',') : x.toLocaleString('ru-RU', { maximumFractionDigits: 2 })

/** The service's models, cheapest first (OpenRouter's prices: $ per million tokens in / out); those that cannot call
 * tools are marked — the assistant needs them for timers, music, the volume. Keyed by the address where it is used:
 * another address, another list. */
function ModelPicker({ query, value, onPick }) {
  const [items, setItems] = useState(null)
  const [filter, setFilter] = useState('')
  const [load, busy, error] = useAction(async () => setItems((await api.models(query)).items))
  const shown = useMemo(() => {
    const f = filter.trim().toLowerCase()
    return (items || [])
      .filter((m) => !f || m.id.toLowerCase().includes(f) || m.name.toLowerCase().includes(f))
      .slice(0, 60)
  }, [items, filter])
  if (!items) {
    return (
      <div>
        <button type="button" className="link-btn" disabled={busy || !query.url} onClick={() => load()}>
          {busy ? 'Загрузка списка…' : 'Выбрать из списка моделей'}
        </button>
        {error && <p className="err">{error}</p>}
      </div>
    )
  }
  return (
    <div className="models-list">
      <input
        className="field sm"
        placeholder="Поиск, например deepseek"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
        aria-label="Поиск модели"
      />
      <ul role="listbox" aria-label="Модели">
        {shown.map((m) => (
          <li key={m.id}>
            <button type="button" role="option" aria-selected={m.id === value} onClick={() => onPick(m.id)}>
              <span className="mid">{m.id}</span>
              <span className="mmeta">
                {m.in != null ? `$${price(m.in)} / $${price(m.out)} за 1 млн токенов` : ''}
                {m.tools === false ? `${m.in != null ? ' · ' : ''}не умеет ставить таймеры и включать музыку` : ''}
              </span>
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

/** `builtIn`: the recognition and the voice that came with the installation and are not connected now. */
function Editor({ kind, provider, builtIn, onClose }) {
  const presets = useMemo(
    () => [
      ...builtIn.filter((b) => b.kind === kind).map((b) => ({ ...b, label: `${b.name} (входит в установку)` })),
      ...PRESETS[kind],
    ],
    [builtIn, kind],
  )
  const [preset, setPreset] = useState(0)
  const [f, setF] = useState(() =>
    provider
      ? {
          name: provider.name,
          url: provider.url,
          model: provider.model,
          voice: provider.voice,
          speed: provider.speed,
          key: '',
        }
      : {
          name: presets[0].name,
          url: presets[0].url,
          model: presets[0].model || '',
          voice: presets[0].voice || '',
          speed: presets[0].speed ?? null,
          key: '',
        },
  )
  const [clearKey, setClearKey] = useState(false)
  const set = (k) => (v) => setF((x) => ({ ...x, [k]: v }))
  const choosePreset = (i) => {
    setPreset(i)
    const p = presets[i]
    setF((x) => ({
      ...x,
      name: p.name,
      url: p.url,
      model: p.model || '',
      voice: p.voice || '',
      speed: p.speed ?? null,
    }))
  }
  const [save, busy, error] = useAction(async () => {
    const body = { name: f.name, url: f.url, model: f.model || '' }
    if (kind === 'tts')
      Object.assign(body, { voice: f.voice || '', speed: f.speed === '' || f.speed == null ? null : Number(f.speed) })
    if (f.key) body.key = f.key
    else if (clearKey) body.key = ''
    const saved = provider ? await api.changeProvider(provider.id, body) : await api.addProvider({ ...body, kind })
    onClose(saved.id)
  })
  const cloud = /^https:\/\//.test(f.url)
  const needsKey = !provider && presets[preset].needsKey && !f.key
  return (
    <Sheet
      title={provider ? provider.name : `Новая модель: ${KIND_TITLE[kind].toLowerCase()}`}
      onClose={() => onClose()}
      onSubmit={save}
      foot={
        <>
          <span className="spacer" />
          <button type="button" className="btn" onClick={() => onClose()}>
            Отмена
          </button>
          <button className="btn primary" disabled={busy || !f.name.trim() || !f.url.trim() || needsKey}>
            {provider ? 'Сохранить' : 'Добавить'}
          </button>
        </>
      }
    >
      {!provider && (
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
        <label htmlFor="pv-key">
          <span className="label">Ключ API</span>
        </label>
        <input
          id="pv-key"
          className="field"
          type="password"
          autoComplete="off"
          placeholder={
            provider?.key_set && !clearKey
              ? `Задан${provider.key_tail ? `: …${provider.key_tail}` : ''}. Оставьте пустым, чтобы не менять`
              : 'Не нужен для локального сервера'
          }
          value={f.key}
          onChange={(e) => set('key')(e.target.value)}
        />
        {provider?.key_set && !f.key && (
          <label className="check">
            <input type="checkbox" checked={clearKey} onChange={(e) => setClearKey(e.target.checked)} /> Удалить ключ
          </label>
        )}
      </div>
      <div className="field-wrap">
        <label htmlFor="pv-model">
          <span className="label">Модель</span>
        </label>
        <input
          id="pv-model"
          className="field"
          maxLength={200}
          placeholder={kind === 'llm' ? 'Например, deepseek/deepseek-v4.1-flash' : ''}
          value={f.model}
          onChange={(e) => set('model')(e.target.value)}
        />
        {kind === 'llm' && (
          <ModelPicker
            key={f.url}
            query={{ url: f.url, key: f.key || undefined, id: provider?.id }}
            value={f.model}
            onPick={set('model')}
          />
        )}
      </div>
      {kind === 'tts' && (
        <div className="form-pair">
          <label className="field-wrap">
            <span>Голос</span>
            <input
              className="field"
              maxLength={100}
              placeholder="Например, nova"
              value={f.voice}
              onChange={(e) => set('voice')(e.target.value)}
            />
          </label>
          <label className="field-wrap">
            <span>Скорость</span>
            <input
              className="field"
              type="number"
              min={0.5}
              max={3}
              step={0.1}
              value={f.speed ?? ''}
              onChange={(e) => set('speed')(e.target.value)}
            />
          </label>
        </div>
      )}
      {cloud && <p className="notice info">{cloudNote(kind)}</p>}
      {error && <p className="err">{error}</p>}
    </Sheet>
  )
}

/** «Проверить»: a model answers, recognition hears, a voice speaks (played here). The voice stops and its file is let
 * go when another check starts or the page is left. */
function useCheck(p) {
  const [result, setResult] = useState(null)
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
  const [run, busy, error] = useAction(async () => {
    setResult(null)
    hush()
    const r = await api.checkProvider(p.id, p.kind)
    if (p.kind === 'tts') {
      if (!alive.current) return // left while it was being made: nobody to hear it
      const url = URL.createObjectURL(r.audio)
      const audio = new Audio(url)
      const v = { audio, url }
      voice.current = v
      audio.onended = () => voice.current === v && hush()
      audio.play().catch(() => voice.current === v && hush())
      setResult(
        `Работает: звучит пример голоса, синтез занял ${r.seconds.toLocaleString('ru-RU', { maximumFractionDigits: 1 })} с`,
      )
    } else if (p.kind === 'llm') {
      setResult(`Работает: ответ «${r.text}» за ${r.seconds.toLocaleString('ru-RU', { maximumFractionDigits: 1 })} с`)
    } else {
      setResult(`Работает: распознано «${r.text}», сказано «${r.expected}»`)
    }
  })
  return { run, busy, result, error }
}

/** `inUse`: the one of its kind every station uses; `only`: the only one of its kind; `saved`: counts its saves here —
 * each is checked at once (a wrong model name or key is seen here, not heard from the station), but for a voice:
 * its check is a sound. */
function Item({ p, inUse, only, saved, onEdit }) {
  const [choose, busy, error] = useAction(() => api.chooseProviders({ [p.kind]: p.id }))
  // the server keeps what is in use while another could take its place, and the only recognition and voice
  const deletable = !inUse || (only && p.kind === 'llm')
  const [confirm, setConfirm] = useState(false)
  const [del, deleting, delError] = useAction(() => api.deleteProvider(p.id))
  const check = useCheck(p)
  useEffect(() => {
    if (saved && p.kind !== 'tts') check.run()
    // eslint-disable-next-line react-hooks/exhaustive-deps -- once per save: `check` is new at every render
  }, [saved])
  return (
    <li className="prov">
      <div className="r-text" style={{ flex: '1 1 260px' }}>
        <b>
          {p.name}
          {inUse && !only && <span className="tag rasp">Используется</span>}
          {p.cloud && <span className="tag rasp">Облако</span>}
        </b>
        <span>{[p.model, p.url].filter(Boolean).join(' · ')}</span>
      </div>
      <div className="prov-acts">
        <button type="button" className="btn sm" disabled={check.busy} onClick={() => check.run()}>
          {check.busy ? 'Проверка…' : 'Проверить'}
        </button>
        <button type="button" className="btn sm" onClick={onEdit}>
          Изменить
        </button>
        {!inUse && (
          <button type="button" className="btn sm" disabled={busy} onClick={() => choose()}>
            <Check size={15} aria-hidden="true" /> Выбрать
          </button>
        )}
        {deletable && !confirm && (
          <button type="button" className="btn sm ghost danger" onClick={() => setConfirm(true)}>
            Удалить
          </button>
        )}
      </div>
      {confirm && (
        <div className="confirm" role="alertdialog" aria-label={`Удалить «${p.name}»?`}>
          <span>Удалить «{p.name}»?</span>
          <button type="button" className="btn sm danger solid" disabled={deleting} onClick={() => del()} autoFocus>
            Удалить
          </button>
          <button type="button" className="btn sm" onClick={() => setConfirm(false)}>
            Отмена
          </button>
        </div>
      )}
      {(check.result || check.error) && (
        <p className={`prov-note${check.error ? ' err' : ''}`} role="status">
          {check.error ? `Не работает: ${check.error}` : check.result}
        </p>
      )}
      {(error || delError) && <p className="err r-full">{error || delError}</p>}
    </li>
  )
}

const KIND_ICON = { llm: MessageSquareText, asr: Ear, tts: AudioWaveform }

export default function Connections({ Pane }) {
  const providers = useStore((s) => s.providers)
  const [edit, setEdit] = useState(null) // {kind, provider?}
  const [saved, setSaved] = useState({}) // id → how many times saved here
  const closeEditor = (id) => {
    setEdit(null)
    if (id) setSaved((s) => ({ ...s, [id]: (s[id] || 0) + 1 }))
  }
  return (
    <Pane title="Модели">
      <p className="muted" style={{ padding: '0 4px', marginTop: -8 }}>
        Сервисы, которые распознают речь, отвечают и озвучивают ответы.
      </p>
      {!providers && <Loading />}
      {providers &&
        KINDS.map((kind) => {
          const items = providers.items.filter((p) => p.kind === kind)
          const Icon = KIND_ICON[kind]
          return (
            <section key={kind} className="card" aria-labelledby={`k-${kind}`}>
              <div className="card-head">
                <div style={{ display: 'flex', alignItems: 'center', gap: 12, minWidth: 0 }}>
                  <span className="ico" aria-hidden="true">
                    <Icon size={20} />
                  </span>
                  <div className="r-text">
                    <b id={`k-${kind}`} style={{ fontSize: 18, fontWeight: 700 }}>
                      {KIND_TITLE[kind]}
                    </b>
                    <span>{KIND_ABOUT[kind]}</span>
                  </div>
                </div>
                <button type="button" className="btn sm" onClick={() => setEdit({ kind })}>
                  <Plus size={16} aria-hidden="true" /> Добавить
                </button>
              </div>
              {items.length > 0 ? (
                <ul className="provs" style={{ listStyle: 'none', margin: 0, padding: 0 }}>
                  {items.map((p) => (
                    <Item
                      key={p.id}
                      p={p}
                      inUse={providers.in_use[kind] === p.id}
                      only={items.length === 1}
                      saved={saved[p.id] || 0}
                      onEdit={() => setEdit({ kind, provider: p })}
                    />
                  ))}
                </ul>
              ) : (
                <p className="faint">
                  {kind === 'llm' ? 'Добавьте модель, чтобы ассистент мог отвечать' : 'Ничего не подключено'}
                </p>
              )}
            </section>
          )
        })}
      {edit && <Editor kind={edit.kind} provider={edit.provider} builtIn={providers.built_in} onClose={closeEditor} />}
    </Pane>
  )
}
