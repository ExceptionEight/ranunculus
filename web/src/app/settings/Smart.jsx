// Settings → the home's devices: the connection to Home Assistant — its address and a token, tried by the server
// before they are kept. The token goes to the server and stays there: it is never shown again.
import { useEffect, useRef, useState } from 'react'
import { api } from '../../shared/api.js'
import { plural } from '../../shared/format.js'
import { useAction } from '../../shared/hooks.js'
import { useStore } from '../../shared/store.js'
import { Loading } from '../ui.jsx'
import Pane from './Pane.jsx'

function Status({ house }) {
  if (house.status === 'on') {
    const parts = [
      house.name,
      house.version && `Home Assistant ${house.version}`,
      `${house.devices.length} ${plural(house.devices.length, ['устройство', 'устройства', 'устройств'])}`,
    ]
    return (
      <p className="house-status ok" role="status">
        Подключено · {parts.filter(Boolean).join(' · ')}
      </p>
    )
  }
  if (house.status === 'failed') {
    return (
      <p className="house-status err" role="status">
        {house.error}
      </p>
    )
  }
  return (
    <p className="house-status faint" role="status">
      Подключение…
    </p>
  )
}

export default function Smart() {
  const house = useStore((s) => s.house)
  const [url, setUrl] = useState('')
  const [token, setToken] = useState('')
  const [asked, setAsked] = useState(false)
  const kept = house?.url
  useEffect(() => {
    if (kept != null) setUrl(kept)
  }, [kept])
  // «Отмена» gives the focus back to the button the question came from
  const offButton = useRef(null)
  const cancelled = useRef(false)
  useEffect(() => {
    if (!asked && cancelled.current) offButton.current?.focus()
    cancelled.current = false
  }, [asked])
  const [save, busy, error] = useAction(async () => {
    await api.connectHouse(url.trim(), token.trim())
    setToken('')
  })
  const [forget, forgetBusy, forgetError] = useAction(async () => {
    await api.forgetHouse()
    setAsked(false)
    setToken('')
  })
  if (!house)
    return (
      <Pane title="Умный дом">
        <Loading />
      </Pane>
    )
  const set = house.status !== 'off'
  const same = set && url.trim() === house.url && !token.trim()
  return (
    <Pane title="Умный дом">
      <section className="card" aria-labelledby="ha-h">
        <h3 id="ha-h">Home Assistant</h3>
        <p className="about">
          Устройства дома берутся из Home Assistant: комнаты, названия и то, что скрыто, настраиваются там.
        </p>
        {set && <Status house={house} />}
        <form
          className="form"
          onSubmit={(e) => {
            e.preventDefault()
            save()
          }}
        >
          <label className="field-wrap">
            <span>Адрес</span>
            <input
              className="field"
              value={url}
              maxLength={300}
              placeholder="http://192.168.1.10:8123"
              autoCapitalize="none"
              autoCorrect="off"
              spellCheck={false}
              onChange={(e) => setUrl(e.target.value)}
            />
          </label>
          <label className="field-wrap">
            <span>Токен</span>
            <input
              className="field"
              type="password"
              value={token}
              maxLength={2000}
              placeholder={set ? `Задан${house.token_tail ? `: …${house.token_tail}` : ''}` : ''}
              autoComplete="off"
              onChange={(e) => setToken(e.target.value)}
            />
          </label>
          <div className="form-acts">
            <button className="btn primary" disabled={busy || !url.trim() || same || (!set && !token.trim())}>
              {busy ? 'Проверка…' : set ? 'Сохранить' : 'Подключить'}
            </button>
            {set && !asked && (
              <button type="button" className="btn ghost danger" ref={offButton} onClick={() => setAsked(true)}>
                Отключить
              </button>
            )}
          </div>
          {error && <p className="err">{error}</p>}
          {asked && (
            <div className="confirm" role="alertdialog" aria-label="Отключение Home Assistant">
              <span>Устройства пропадут с главной, адрес и токен будут удалены.</span>
              <div className="confirm-acts">
                <button
                  type="button"
                  className="btn sm danger solid"
                  onClick={() => forget()}
                  disabled={forgetBusy}
                  autoFocus
                >
                  Отключить
                </button>
                <button
                  type="button"
                  className="btn sm"
                  onClick={() => {
                    cancelled.current = true
                    setAsked(false)
                  }}
                >
                  Отмена
                </button>
              </div>
            </div>
          )}
          {forgetError && <p className="err">{forgetError}</p>}
        </form>
      </section>
    </Pane>
  )
}
