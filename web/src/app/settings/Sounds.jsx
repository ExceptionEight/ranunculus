// Settings → sounds: what timers and alarms ring with, for how long, and the user's own files.
import { useEffect, useState } from 'react'
import { Pause, Pencil, Play, Trash2, Upload } from 'lucide-react'
import { api } from '../../shared/api.js'
import { useAction, useDrop, usePreview, useSounds, useUploads } from '../../shared/hooks.js'
import { useStore } from '../../shared/store.js'
import { bytes, secs, soundTitle, span } from '../../shared/format.js'
import { Duration, Listen, Loading, SoundSelect } from '../ui.jsx'
import Pane from './Pane.jsx'

function whyNot(error) {
  if (/Invalid data|could not find|Error opening/i.test(error)) return 'файл не распознан как звук'
  if (/тишина/.test(error)) return 'в файле только тишина'
  if (/меньше 0,1/.test(error)) return 'звук короче 0,1 секунды'
  return error
}

function SoundItem({ s, used, preview }) {
  const [renaming, setRenaming] = useState(false)
  const [name, setName] = useState(s.name)
  const [confirm, setConfirm] = useState(false)
  const [rename, renBusy, renError] = useAction(async () => {
    await api.renameSound(s.name, name.trim())
    setRenaming(false)
  })
  const [del, delBusy, delError] = useAction(() => api.deleteSound(s.name))
  const on = preview.isPlaying(s.name)
  const meta = s.error
    ? `Не воспроизводится: ${whyNot(s.error)}`
    : s.builtin
      ? `Встроенный · ${secs(s.seconds)}`
      : `${secs(s.seconds)} · ${bytes(s.bytes)}`
  return (
    <li className="sound">
      <button
        type="button"
        className="play-btn"
        disabled={!!s.error}
        onClick={() => preview.play(s.name)}
        aria-pressed={on}
        aria-label={on ? `Остановить «${soundTitle(s.name)}»` : `Послушать «${soundTitle(s.name)}»`}
        title={on ? 'Остановить' : 'Послушать'}
      >
        {on ? (
          <Pause size={18} fill="currentColor" aria-hidden="true" />
        ) : (
          <Play size={18} fill="currentColor" aria-hidden="true" />
        )}
      </button>
      {renaming ? (
        <form
          onSubmit={(e) => {
            e.preventDefault()
            rename()
          }}
        >
          <label className="sr-only" htmlFor={`rn-${s.name}`}>
            Новое имя
          </label>
          <input
            id={`rn-${s.name}`}
            className="field sm"
            value={name}
            maxLength={100}
            autoFocus
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === 'Escape' && setRenaming(false)}
          />
          <button className="btn sm primary" disabled={renBusy || !name.trim() || name.trim() === s.name}>
            Сохранить
          </button>
          <button type="button" className="btn sm" onClick={() => setRenaming(false)}>
            Отмена
          </button>
        </form>
      ) : (
        <span className="r-text">
          <b>{soundTitle(s.name)}</b>
          <span className={s.error ? 'bad' : undefined} title={s.error || s.name}>
            {meta}
            {used.length > 0 && ` · ${used.join(', ')}`}
          </span>
        </span>
      )}
      {!s.builtin && !renaming && (
        <span className="prov-acts">
          <button
            type="button"
            className="btn ghost icon sm"
            onClick={() => (setName(s.name), setRenaming(true))}
            aria-label={`Переименовать «${soundTitle(s.name)}»`}
            title="Переименовать"
          >
            <Pencil size={17} aria-hidden="true" />
          </button>
          <button
            type="button"
            className="btn ghost icon sm"
            onClick={() => setConfirm(true)}
            aria-label={`Удалить «${soundTitle(s.name)}»`}
            title="Удалить"
          >
            <Trash2 size={17} aria-hidden="true" />
          </button>
        </span>
      )}
      {confirm && (
        <div className="confirm" role="alertdialog" aria-label={`Удалить «${soundTitle(s.name)}»?`}>
          <span>
            Удалить «{soundTitle(s.name)}»?{used.length > 0 ? ' Вместо него будет звучать колокольчик.' : ''}
          </span>
          <button type="button" className="btn sm danger solid" disabled={delBusy} onClick={() => del()} autoFocus>
            Удалить
          </button>
          <button type="button" className="btn sm" onClick={() => setConfirm(false)}>
            Отмена
          </button>
        </div>
      )}
      {(renError || delError) && <p className="err r-full">{renError || delError}</p>}
    </li>
  )
}

function Uploads() {
  const up = useUploads()
  const [drop, over] = useDrop(up.add)
  return (
    <div className={`drop${over ? ' over' : ''}`} {...drop}>
      <span className="ico" aria-hidden="true">
        <Upload size={20} />
      </span>
      <p>
        Перетащите аудиофайл сюда или выберите его: MP3, OGG, WAV, M4A, FLAC. Тишина в начале обрежется, громкость
        выровняется.
      </p>
      <label className="btn">
        Выбрать файл
        <input
          type="file"
          accept="audio/*,.mp3,.ogg,.oga,.opus,.wav,.m4a,.flac,.aac"
          multiple
          hidden
          onChange={(e) => {
            if (e.target.files?.length) up.add(e.target.files)
            e.target.value = ''
          }}
        />
      </label>
      {up.items.length > 0 && (
        <ul className="ups">
          {up.items.map((it) => (
            <li key={it.key}>
              <span className="nm">{it.name}</span>
              {it.state === 'up' &&
                (it.progress < 1 ? (
                  <progress value={it.progress} max={1} aria-label={`Загрузка «${it.name}»`} />
                ) : (
                  <span>Проверка…</span>
                ))}
              {it.state === 'done' && <span className="ok">{it.replaced ? 'Заменён' : 'Загружен'}</span>}
              {it.state === 'error' && <span className="bad">{it.error}</span>}
              {it.state === 'exists' && (
                <span>
                  Уже есть.{' '}
                  <button type="button" className="link-btn" onClick={() => up.replace(it)}>
                    Заменить
                  </button>
                </span>
              )}
              {it.state !== 'up' && (
                <button type="button" className="btn ghost sm" onClick={() => up.dismiss(it.key)}>
                  Скрыть
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

const RAMPS = [0, 15, 30, 60, 120, 300]

export default function Sounds() {
  const settings = useStore((s) => s.settings)
  const sounds = useSounds()
  const preview = usePreview()
  const { stop } = preview
  useEffect(() => () => stop(), [stop])
  const [change, busy, error] = useAction((patch) => api.changeSettings(patch))
  if (!settings)
    return (
      <Pane title="Звуки">
        <Loading />
      </Pane>
    )
  const { timer, alarm } = settings
  const usedBy = (name) => [timer.sound === name && 'таймер', alarm.sound === name && 'будильник'].filter(Boolean)
  return (
    <Pane title="Звуки">
      <section className="card" aria-labelledby="snd-timer">
        <h3 id="snd-timer">Таймер</h3>
        <div className="rows">
          <div className="r stack-m">
            <label className="r-text" htmlFor="t-sound">
              <b>Звук</b>
            </label>
            <div className="r-ctl">
              <SoundSelect
                id="t-sound"
                sounds={sounds}
                value={timer.sound}
                disabled={busy}
                onChange={(v) => change({ timer: { sound: v } })}
              />
            </div>
          </div>
          <div className="r stack-m">
            <div className="r-text">
              <b>Длительность</b>
            </div>
            <div className="r-ctl">
              <Duration
                value={timer.ring}
                label="Длительность звонка таймера"
                onCommit={(v) => change({ timer: { ring: v } })}
              />
            </div>
          </div>
          <div className="r">
            <div className="r-text">
              <b>Послушать</b>
              <span>Как на колонке{timer.ring > 30 ? ', первые 30 секунд' : ''}</span>
            </div>
            <div className="r-ctl">
              <Listen preview={preview} name={timer.sound} seconds={Math.min(timer.ring, 30)} />
            </div>
          </div>
        </div>
      </section>

      <section className="card" aria-labelledby="snd-alarm">
        <h3 id="snd-alarm">Будильник</h3>
        <p className="about">Для будильников без собственных настроек</p>
        <div className="rows">
          <div className="r stack-m">
            <label className="r-text" htmlFor="a-sound">
              <b>Звук</b>
            </label>
            <div className="r-ctl">
              <SoundSelect
                id="a-sound"
                sounds={sounds}
                value={alarm.sound}
                disabled={busy}
                onChange={(v) => change({ alarm: { sound: v } })}
              />
            </div>
          </div>
          <div className="r stack-m">
            <div className="r-text">
              <b>Длительность</b>
            </div>
            <div className="r-ctl">
              <Duration
                value={alarm.ring}
                label="Длительность звонка будильника"
                onCommit={(v) => change({ alarm: { ring: v } })}
              />
            </div>
          </div>
          <div className="r stack-m">
            <label className="r-text" htmlFor="a-ramp">
              <b>Нарастание громкости</b>
            </label>
            <div className="r-ctl">
              <select
                id="a-ramp"
                className="field"
                value={alarm.ramp}
                disabled={busy}
                onChange={(e) => change({ alarm: { ramp: Number(e.target.value) } })}
              >
                {RAMPS.map((r) => (
                  <option key={r} value={r}>
                    {r ? `За ${span(r)}` : 'Сразу громко'}
                  </option>
                ))}
              </select>
            </div>
          </div>
          <div className="r stack-m">
            <label className="r-text" htmlFor="a-vol">
              <b>Громкость звонка</b>
            </label>
            <div className="r-ctl">
              <select
                id="a-vol"
                className="field"
                value={alarm.volume ?? ''}
                disabled={busy}
                onChange={(e) => change({ alarm: { volume: e.target.value === '' ? null : Number(e.target.value) } })}
              >
                <option value="">Текущая громкость колонки</option>
                {[1, 2, 3, 4, 5, 6, 7, 8, 9, 10].map((v) => (
                  <option key={v} value={v}>
                    {v}
                  </option>
                ))}
              </select>
            </div>
          </div>
          <div className="r">
            <div className="r-text">
              <b>Послушать</b>
              <span>Как на колонке{alarm.ring > 30 ? ', первые 30 секунд' : ''}</span>
            </div>
            <div className="r-ctl">
              <Listen preview={preview} name={alarm.sound} seconds={Math.min(alarm.ring, 30)} />
            </div>
          </div>
        </div>
        {error && <p className="err">{error}</p>}
      </section>

      <section className="card" aria-labelledby="snd-lib">
        <h3 id="snd-lib">Библиотека</h3>
        {!sounds && <Loading />}
        {sounds && (
          <ul className="sounds">
            {sounds.map((s) => (
              <SoundItem key={s.name} s={s} used={usedBy(s.name)} preview={preview} />
            ))}
          </ul>
        )}
        <Uploads />
      </section>
    </Pane>
  )
}
