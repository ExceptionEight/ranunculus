// The server, live: who is signed in (/api/auth: no one — the sign-in form, no events), then one EventSource
// (/api/events) for the whole page. `hello` gives everything there is; then only what
// changes: a device (online, panel, volume, music, ring, name), what was heard and what is said in answer, sentence
// by sentence, an exchange written to the chats, the settings, the timers, the sounds. Fell behind — the server says
// `hello` again (a fresh snapshot).
// Components read it with useStore(selector); the selector must return a part of the state as it is (no new objects).
import { useSyncExternalStore } from 'react'
import { api, whenSignedOut } from './api.js'
import { setZone } from './zone.js'

let state = {
  auth: null, // null: not known yet; {user: {name} | null, setup, secure} (api.auth)
  link: 'connecting', // connecting | live | lost
  version: '',
  devices: {}, // id → device
  settings: null,
  timers: [],
  alarms: [], // soonest first
  providers: null, // {items, in_use}: who answers, hears, speaks
  sounds: null, // null: not fetched yet
  live: {}, // device → {turn, heard, text, said: [sentences], t}: the exchange going on now
  chats: {}, // device id, or '*' for all → {items: oldest first, more, loading, error}
}
const listeners = new Set()

function set(patch) {
  state = { ...state, ...patch }
  listeners.forEach((l) => l())
}

export function getState() {
  return state
}

export function subscribe(l) {
  listeners.add(l)
  return () => listeners.delete(l)
}

export function useStore(selector) {
  return useSyncExternalStore(subscribe, () => selector(state))
}

/** Devices in the order they were first seen. */
export function deviceList(devices) {
  return Object.values(devices).sort((a, b) => (a.first_seen ?? 0) - (b.first_seen ?? 0) || a.id.localeCompare(b.id))
}

export function deviceName(d) {
  return d?.name || d?.id || ''
}

// ---------------------------------------------------------------- signed in or not

let authTimer = null

/** Who is signed in: then the events; no one — the form (App). The server not there — again in 3 s. */
export async function checkAuth() {
  clearTimeout(authTimer)
  try {
    const a = await api.auth()
    if (!a.user) return signedOut(a)
    set({ auth: a })
    if (es?.readyState === EventSource.CLOSED) disconnect()
    connect()
  } catch {
    set({ link: 'connecting' })
    authTimer = setTimeout(checkAuth, 3000)
  }
}

export function signedIn(user) {
  set({ auth: { ...state.auth, user } })
  connect()
}

const EMPTY = {
  devices: {},
  settings: null,
  timers: [],
  alarms: [],
  providers: null,
  sounds: null,
  live: {},
  chats: {},
}

/** The session is over (signed out here or elsewhere, a new password, its time): nothing of it stays on the page.
 * `a`: the server's word already; without it — asked once (whether there is anyone to sign in as, by HTTPS…). */
export function signedOut(a) {
  disconnect()
  set({ ...EMPTY, link: 'connecting', auth: { ...(a || state.auth), user: null } })
  if (!a) checkAuth()
}

whenSignedOut(() => {
  if (state.auth?.user) signedOut()
})

// ---------------------------------------------------------------- the event stream

let es = null
let staleTimer = null
const wakeListeners = new Set()

/** Every wake recorded or changed (wakes.rs), as it happens: → unsubscribe */
export function onWake(l) {
  wakeListeners.add(l)
  return () => wakeListeners.delete(l)
}

const listenListeners = new Set()

/** A listen of music over (listens.rs): the music's numbers changed → unsubscribe */
export function onListen(l) {
  listenListeners.add(l)
  return () => listenListeners.delete(l)
}

export function connect() {
  if (es) return
  es = new EventSource('/api/events')
  es.addEventListener('hello', (e) => hello(JSON.parse(e.data)))
  es.addEventListener('device', (e) => {
    const d = JSON.parse(e.data)
    const old = state.devices[d.id] || { id: d.id }
    set({ devices: { ...state.devices, [d.id]: { ...old, ...d } } })
  })
  es.addEventListener('heard', (e) => {
    const h = JSON.parse(e.data)
    set({
      live: { ...state.live, [h.device]: { turn: h.turn, heard: h.heard, text: h.text, said: [], t: Date.now() } },
    })
  })
  es.addEventListener('said', (e) => {
    const s = JSON.parse(e.data)
    const cur = state.live[s.device]
    if (!cur || cur.turn !== s.turn) return
    set({ live: { ...state.live, [s.device]: { ...cur, said: [...cur.said, s.text], t: Date.now() } } })
  })
  es.addEventListener('exchange', (e) => {
    const { device, turn, exchange } = JSON.parse(e.data)
    const live = { ...state.live }
    if (live[device]?.turn === turn) delete live[device]
    const chats = { ...state.chats }
    for (const key of [device, '*']) {
      const c = chats[key]
      if (c && !c.items.some((x) => x.id === exchange.id)) chats[key] = { ...c, items: [...c.items, exchange] }
    }
    const d = state.devices[device]
    const devices = d ? { ...state.devices, [device]: { ...d, exchanges: (d.exchanges || 0) + 1 } } : state.devices
    set({ live, chats, devices })
  })
  es.addEventListener('settings', (e) => {
    const settings = JSON.parse(e.data)
    setZone(settings.home?.zone)
    set({ settings })
  })
  es.addEventListener('timers', (e) => set({ timers: JSON.parse(e.data) }))
  es.addEventListener('alarms', (e) => set({ alarms: JSON.parse(e.data) }))
  es.addEventListener('providers', (e) => set({ providers: JSON.parse(e.data) }))
  es.addEventListener('wake', (e) => {
    const w = JSON.parse(e.data)
    wakeListeners.forEach((l) => l(w))
  })
  es.addEventListener('listen', (e) => {
    const l = JSON.parse(e.data)
    listenListeners.forEach((f) => f(l))
  })
  es.addEventListener('sounds', (e) => set({ sounds: JSON.parse(e.data) }))
  es.onopen = () => set({ link: 'live' })
  // closed for good: the server refused (signed out: 401) or failed — which of the two, /api/auth says
  es.onerror = () => {
    if (es?.readyState !== EventSource.CLOSED) return set({ link: 'connecting' })
    set({ link: 'lost' })
    clearTimeout(authTimer)
    authTimer = setTimeout(checkAuth, 2000)
  }
  // a turn whose exchange never came (the server restarted meanwhile): not «говорит…» for ever
  staleTimer = setInterval(() => {
    const now = Date.now()
    const stale = Object.entries(state.live).filter(([, v]) => now - v.t > 120000)
    if (stale.length) {
      const live = { ...state.live }
      stale.forEach(([k]) => delete live[k])
      set({ live })
    }
  }, 10000)
}

export function disconnect() {
  es?.close()
  es = null
  clearInterval(staleTimer)
}

function hello(h) {
  setZone(h.settings?.home?.zone)
  // the home's time zone was never chosen: this browser's — whoever sets the server up does it from home, as a rule
  // (a server in a container has no zone of its own to go by)
  if (h.settings?.home?.timezone === '') {
    api.changeSettings({ home: { timezone: Intl.DateTimeFormat().resolvedOptions().timeZone } }).catch(() => {})
  }
  const devices = Object.fromEntries(h.devices.map((d) => [d.id, d]))
  // after a reconnect the chats may have missed exchanges: loaded ones are fetched again
  const reload = Object.keys(state.chats)
  set({
    link: 'live',
    version: h.version,
    devices,
    settings: h.settings,
    timers: h.timers,
    alarms: h.alarms || [],
    providers: h.providers || null,
    chats: {},
  })
  reload.forEach((k) => loadChat(k))
  if (state.sounds) refreshSounds()
}

// A page left for another (back/forward cache) keeps its EventSource open, and HTTP/1.1 gives a browser six connections
// to a server: after six pages in a tab, the seventh would hang. Closed when the page is hidden for
// good or into the cache, opened again when it is shown from there — its `hello` brings it up to date.
if (typeof window !== 'undefined') {
  window.addEventListener('pagehide', () => disconnect())
  window.addEventListener('pageshow', (e) => {
    if (e.persisted) checkAuth()
  })
}

// ---------------------------------------------------------------- the chats

const chatKey = (device) => device || '*'

/** The newest page of a device's chat (or everyone's: no device), once; later pages: loadOlder. */
export async function loadChat(device) {
  const key = chatKey(device)
  if (state.chats[key]?.loading) return
  set({ chats: { ...state.chats, [key]: { items: [], more: false, loading: true, error: null } } })
  try {
    const page = await api.exchanges({ device: key === '*' ? undefined : key, limit: 40 })
    set({
      chats: {
        ...state.chats,
        [key]: { items: page.items.slice().reverse(), more: page.more, loading: false, error: null },
      },
    })
  } catch (e) {
    set({ chats: { ...state.chats, [key]: { items: [], more: false, loading: false, error: e.message } } })
  }
}

export async function loadOlder(device) {
  const key = chatKey(device)
  const c = state.chats[key]
  if (!c || c.loading || !c.more || !c.items.length) return
  set({ chats: { ...state.chats, [key]: { ...c, loading: true } } })
  try {
    const page = await api.exchanges({ device: key === '*' ? undefined : key, before: c.items[0].id, limit: 40 })
    const cur = state.chats[key]
    set({
      chats: {
        ...state.chats,
        [key]: { ...cur, items: [...page.items.slice().reverse(), ...cur.items], more: page.more, loading: false },
      },
    })
  } catch (e) {
    set({ chats: { ...state.chats, [key]: { ...state.chats[key], loading: false, error: e.message } } })
  }
}

export function chatOf(device) {
  return (s) => s.chats[chatKey(device)]
}

// ---------------------------------------------------------------- sounds

export async function refreshSounds() {
  try {
    set({ sounds: await api.sounds() })
  } catch {
    /* the list stays as it was */
  }
}
