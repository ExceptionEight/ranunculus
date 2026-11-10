// What the pages need beyond the store: listening to a sound in the browser, uploading files, a ticking clock, data
// loaded for what is chosen (useLoad), an action's pending state and error (useAction), a chat window's scroll.
import { useCallback, useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from 'react'
import { api, ApiError } from './api.js'
import { chatOf, checkAuth, getState, loadChat, loadOlder, refreshSounds, useStore } from './store.js'
import './theme.js'

/** Who is signed in, then the server's live stream. */
export function useLiveStore() {
  useEffect(() => {
    checkAuth()
  }, [])
}

// ---------------------------------------------------------------- listening in the browser

const preview = { name: null, seconds: null, key: null, audio: null, listeners: new Set() }
const notify = () => preview.listeners.forEach((l) => l())

/** One sound at a time on the page. play(name) — once; play(name, seconds) — looped as a ring that long; the same
 * again stops it, another (or the same in the other way) switches to it.
 * → {playing: name | null, seconds: the playing one's seconds | null, isPlaying(name, seconds?), play, stop} */
export function usePreview() {
  const key = useSyncExternalStore(
    (l) => {
      preview.listeners.add(l)
      return () => preview.listeners.delete(l)
    },
    () => preview.key,
  )
  const stop = useCallback(() => {
    preview.audio?.pause()
    preview.audio = null
    preview.name = preview.seconds = preview.key = null
    notify()
  }, [])
  const play = useCallback(
    (name, seconds = null) => {
      const k = `${name}\u0000${seconds ?? ''}`
      const same = preview.key === k
      stop()
      if (same) return
      const a = new Audio(api.audioUrl(name, seconds))
      Object.assign(preview, { audio: a, name, seconds, key: k })
      a.onended = () => preview.audio === a && stop()
      a.onerror = () => preview.audio === a && stop()
      a.play().catch(() => preview.audio === a && stop())
      notify()
    },
    [stop],
  )
  const isPlaying = useCallback((name, seconds = null) => key === `${name}\u0000${seconds ?? ''}`, [key])
  return { playing: key ? preview.name : null, seconds: key ? preview.seconds : null, isPlaying, play, stop }
}

// ---------------------------------------------------------------- uploads

/** Files in, one by one: {name, progress 0…1, state: 'up' | 'done' | 'error' | 'exists', error, replaced, file}. */
export function useUploads() {
  const [items, setItems] = useState([])
  const update = (key, patch) => setItems((xs) => xs.map((x) => (x.key === key ? { ...x, ...patch } : x)))

  const send = useCallback(async (item, replace = false) => {
    update(item.key, { state: 'up', progress: 0, error: null })
    try {
      await api.upload(item.name, item.file, { replace, onProgress: (p) => update(item.key, { progress: p }) })
      update(item.key, { state: 'done', progress: 1, replaced: replace })
      refreshSounds()
    } catch (e) {
      update(item.key, { state: e instanceof ApiError && e.status === 409 ? 'exists' : 'error', error: e.message })
    }
  }, [])

  const add = useCallback(
    (files) => {
      const fresh = [...files].map((f, i) => ({
        key: `${Date.now()}-${i}-${f.name}`,
        name: cleanName(f.name),
        file: f,
        progress: 0,
        state: 'up',
        error: null,
      }))
      setItems((xs) => [...fresh, ...xs].slice(0, 8))
      fresh.reduce((p, it) => p.then(() => send(it)), Promise.resolve())
    },
    [send],
  )
  const replace = useCallback((item) => send(item, true), [send])
  const dismiss = useCallback((key) => setItems((xs) => xs.filter((x) => x.key !== key)), [])
  return { items, add, replace, dismiss }
}

// eslint-disable-next-line no-control-regex -- control characters are what is being taken out
const NOT_IN_A_NAME = /[/\\\u0000-\u001f]/g

/** A browser's file name as the server takes it: no slashes, no dot in front, at most 100 characters. */
export function cleanName(n) {
  let s = n.replace(NOT_IN_A_NAME, ' ').trim().replace(/^\.+/, '')
  if (s.toLowerCase() === 'chime') s = 'chime (свой)'
  if ([...s].length > 100) {
    const dot = s.lastIndexOf('.')
    const ext = dot > 0 && s.length - dot <= 6 ? s.slice(dot) : ''
    s =
      [...s]
        .slice(0, 100 - ext.length)
        .join('')
        .trim() + ext
  }
  return s || 'звук'
}

/** Drag and drop onto an element: → [props to spread, whether a file is over it] */
export function useDrop(onFiles) {
  const [over, setOver] = useState(false)
  const depth = useRef(0)
  const props = {
    onDragEnter: (e) => {
      if (![...(e.dataTransfer?.types || [])].includes('Files')) return
      e.preventDefault()
      depth.current += 1
      setOver(true)
    },
    onDragOver: (e) => {
      if ([...(e.dataTransfer?.types || [])].includes('Files')) e.preventDefault()
    },
    onDragLeave: () => {
      depth.current = Math.max(0, depth.current - 1)
      if (!depth.current) setOver(false)
    },
    onDrop: (e) => {
      e.preventDefault()
      depth.current = 0
      setOver(false)
      if (e.dataTransfer?.files?.length) onFiles(e.dataTransfer.files)
    },
  }
  return [props, over]
}

// ---------------------------------------------------------------- time

/** The time now (s), re-rendering every `ms`. */
export function useNow(ms = 1000) {
  const [now, setNow] = useState(() => Date.now() / 1000)
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now() / 1000), ms)
    return () => clearInterval(t)
  }, [ms])
  return now
}

/** The sounds' list, fetched once when a page needs it (then kept live by the `sounds` event). */
export function useSounds() {
  const sounds = useStore((s) => s.sounds)
  useEffect(() => {
    if (!getState().sounds) refreshSounds()
  }, [])
  return sounds
}

// ---------------------------------------------------------------- a chat window's scroll

/** A station's chat in its own scroller, the newest at the bottom: stays at the bottom as lines come while it is
 * there; older pages load when the top comes into view, with no jump. → {chat, live, scrollRef, topRef, atBottom,
 * toBottom} */
export function useChat(device) {
  const chat = useStore(chatOf(device))
  const live = useStore((s) => (device ? s.live[device] : null))
  const scrollRef = useRef(null)
  const topRef = useRef(null)
  const stick = useRef(true)
  const [atBottom, setAtBottom] = useState(true)
  const keep = useRef(null) // the height under the top before older lines came in

  useEffect(() => {
    if (!getState().chats[device || '*']) loadChat(device)
    stick.current = true
  }, [device])

  useEffect(() => {
    const el = scrollRef.current
    if (!el) return
    const onScroll = () => {
      const bottom = el.scrollHeight - el.scrollTop - el.clientHeight < 48
      stick.current = bottom
      setAtBottom(bottom)
    }
    el.addEventListener('scroll', onScroll, { passive: true })
    return () => el.removeEventListener('scroll', onScroll)
  }, [device])

  useEffect(() => {
    const el = scrollRef.current
    const top = topRef.current
    if (!el || !top) return
    const io = new IntersectionObserver(
      ([e]) => {
        const c = getState().chats[device || '*']
        if (e.isIntersecting && c?.more && !c.loading && c.items.length) {
          keep.current = el.scrollHeight - el.scrollTop
          loadOlder(device)
        }
      },
      { root: el, rootMargin: '300px 0px 0px 0px' },
    )
    io.observe(top)
    return () => io.disconnect()
  }, [device, chat?.items.length])

  useLayoutEffect(() => {
    const el = scrollRef.current
    if (!el) return
    if (keep.current != null) {
      el.scrollTop = el.scrollHeight - keep.current
      keep.current = null
    } else if (stick.current) {
      el.scrollTop = el.scrollHeight
    }
  }, [chat?.items, live])

  // from far up: a jump to near the end first, then the last stretch smoothly (a smooth scroll over a long history
  // stopped short of the end)
  const toBottom = useCallback(() => {
    const el = scrollRef.current
    if (!el) return
    const far = el.scrollHeight - el.clientHeight * 3
    if (el.scrollTop < far) el.scrollTop = far
    el.scrollTo({ top: el.scrollHeight, behavior: 'smooth' })
  }, [])

  return { chat, live, scrollRef, topRef, atBottom, toBottom }
}

// ---------------------------------------------------------------- loading

/** Data from the server for `deps` (plain values: ids, a filter, a period). An answer is taken only while its deps are
 * still the current ones, and `data` / `error` are only ever those of the current deps: undefined / null while they
 * load (else, the period switched, the old answer is drawn with the new days — 365 bars from a month, a month's sum
 * «за год» — and a filter's late answer overwrites the new filter's).
 * `reload()` — the same deps again: the data stays until the new answer. `set(fn)` — the data changed in place (a
 * mark, an event, the next page): only while the deps it was rendered with are current, so a late answer never lands
 * in another list. `last` — the last answer with the deps it is for ({data, deps}), to keep drawing it, by its own
 * deps, while the next one loads. → {data, error, loading, reload, set, last} */
export function useLoad(fetcher, deps) {
  const key = JSON.stringify(deps)
  const [st, setSt] = useState({ key: null, data: undefined, error: null, busy: true, last: null })
  const [round, setRound] = useState(0)
  const seq = useRef(0)
  useEffect(() => {
    const my = ++seq.current
    setSt((s) => (s.key === key ? { ...s, busy: true } : { ...s, key, data: undefined, error: null, busy: true }))
    Promise.resolve()
      .then(fetcher)
      .then(
        (data) => my === seq.current && setSt({ key, data, error: null, busy: false, last: { data, deps } }),
        (e) =>
          my === seq.current &&
          setSt((s) => ({ ...s, key, data: s.key === key ? s.data : undefined, error: e.message, busy: false })),
      )
    // the fetcher is this render's: it is what `key` asks for
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, round])
  const reload = useCallback(() => setRound((n) => n + 1), [])
  const set = useCallback(
    (fn) =>
      setSt((s) => {
        if (s.key !== key || s.data === undefined) return s
        const data = fn(s.data)
        return { ...s, data, last: { data, deps: s.last.deps } }
      }),
    [key],
  )
  const mine = st.key === key
  return {
    data: mine ? st.data : undefined,
    error: mine ? st.error : null,
    loading: !mine || st.busy,
    reload,
    set,
    last: st.last,
  }
}

/** An action against the server with its pending state and error: [run, busy, error, clearError] */
export function useAction(fn) {
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState(null)
  const run = useCallback(
    async (...args) => {
      setBusy(true)
      setError(null)
      try {
        return await fn(...args)
      } catch (e) {
        setError(e.message)
      } finally {
        setBusy(false)
      }
    },
    [fn],
  )
  return [run, busy, error, () => setError(null)]
}
