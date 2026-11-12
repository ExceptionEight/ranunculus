// The server's API (server/src/web/api): JSON in and out; a refusal is an ApiError with the server's own words.

export class ApiError extends Error {
  constructor(status, message) {
    super(message)
    this.status = status
  }
}

let onSignedOut = () => {}

/** What to do when the server says the session is over (401 from anything but the sign-in): set by the store. */
export function whenSignedOut(f) {
  onSignedOut = f
}

const signedOutBy = (status, path) => status === 401 && !path.startsWith('/api/auth')

async function call(method, path, body) {
  const opts = { method, headers: {} }
  if (body !== undefined) {
    opts.headers['Content-Type'] = 'application/json'
    opts.body = JSON.stringify(body)
  }
  let r
  try {
    r = await fetch(path, opts)
  } catch {
    throw new ApiError(0, 'Сервер не отвечает. Проверьте, что он запущен.')
  }
  const isJson = (r.headers.get('content-type') || '').includes('application/json')
  const data = isJson ? await r.json() : await r.text()
  if (!r.ok) {
    if (signedOutBy(r.status, path)) onSignedOut()
    throw new ApiError(r.status, (isJson && data.error) || `Ошибка ${r.status}`)
  }
  return data
}

const enc = encodeURIComponent

export const api = {
  /** {user: {name} | null, setup: no one to sign in as yet, secure: this connection may sign in} */
  auth: () => call('GET', '/api/auth'),
  login: (name, password) => call('POST', '/api/auth/login', { name, password }),
  logout: (all = false) => call('POST', '/api/auth/logout', { all }),
  /** the first user, while there is none: → {user} (signed in) */
  register: (name, password) => call('POST', '/api/auth/register', { name, password }),
  changePassword: (current, password) => call('POST', '/api/auth/password', { current, password }),
  users: () => call('GET', '/api/users'),
  addUser: (name, password) => call('POST', '/api/users', { name, password }),
  removeUser: (id) => call('DELETE', `/api/users/${id}`),
  info: () => call('GET', '/api/info'),
  devices: () => call('GET', '/api/devices'),
  renameDevice: (id, name) => call('PATCH', `/api/devices/${enc(id)}`, { name }),
  setVolume: (id, level) => call('POST', `/api/devices/${enc(id)}/volume`, { level }),
  music: (id, action, extra) => call('POST', `/api/devices/${enc(id)}/music`, { action, ...extra }),
  tryRing: (id, sound, seconds) => call('POST', `/api/devices/${enc(id)}/ring`, { sound, seconds }),
  stopRing: (id) => call('DELETE', `/api/devices/${enc(id)}/ring`),
  exchanges: ({ device, before, limit = 50 } = {}) => {
    const q = new URLSearchParams({ limit: String(limit) })
    if (device) q.set('device', device)
    if (before) q.set('before', String(before))
    return call('GET', `/api/exchanges?${q}`)
  },
  settings: () => call('GET', '/api/settings'),
  changeSettings: (patch) => call('PATCH', '/api/settings', patch),
  sounds: () => call('GET', '/api/sounds'),
  renameSound: (name, to) => call('PATCH', `/api/sounds/${enc(name)}`, { name: to }),
  deleteSound: (name) => call('DELETE', `/api/sounds/${enc(name)}`),
  audioUrl: (name, seconds) => `/api/sounds/${enc(name)}/audio${seconds ? `?seconds=${seconds}` : ''}`,
  timers: () => call('GET', '/api/timers'),
  cancelTimer: (id) => call('DELETE', `/api/timers/${id}`),
  alarms: () => call('GET', '/api/alarms'),
  addAlarm: (a) => call('POST', '/api/alarms', a),
  changeAlarm: (id, patch) => call('PATCH', `/api/alarms/${id}`, patch),
  deleteAlarm: (id) => call('DELETE', `/api/alarms/${id}`),
  /** {connections: [{id, name, url, key_set, key_tail, cloud, openrouter}], roles: {asr | llm | tts | embeddings:
   * {connection, model, voice, speed, extra, cloud} | null}} */
  providers: () => call('GET', '/api/providers'),
  addConnection: (c) => call('POST', '/api/connections', c),
  changeConnection: (id, patch) => call('PATCH', `/api/connections/${id}`, patch),
  deleteConnection: (id) => call('DELETE', `/api/connections/${id}`),
  /** a connection's models for a role: {items: [{id, name, in?, out?, tools?, voices?}]} */
  models: (id, role) => call('POST', `/api/connections/${id}/models`, { role }),
  setRole: (role, r) => call('PUT', `/api/roles/${role}`, r),
  clearRole: (role) => call('DELETE', `/api/roles/${role}`),
  /** The role as it is in use, or — `as`: {connection, model, …} — as it would be. A model answers ({text, seconds});
   * recognition hears ({text, expected}); the memory gets vectors ({size, seconds}); a voice → a WAV (Blob). */
  async checkRole(role, as) {
    const path = `/api/roles/${role}/check`
    if (role !== 'tts') return call('POST', path, as)
    const opts = { method: 'POST', headers: {} }
    if (as) {
      opts.headers['Content-Type'] = 'application/json'
      opts.body = JSON.stringify(as)
    }
    let r
    try {
      r = await fetch(path, opts)
    } catch {
      throw new ApiError(0, 'Сервер не отвечает. Проверьте, что он запущен.')
    }
    if (signedOutBy(r.status, path)) onSignedOut()
    if (!r.ok) {
      const j = await r.json().catch(() => ({}))
      throw new ApiError(r.status, j.error || `Ошибка ${r.status}`)
    }
    return { audio: await r.blob(), seconds: Number(r.headers.get('x-seconds')) }
  },
  wakes: ({ device, label, before, limit = 50 } = {}) => {
    const q = new URLSearchParams({ limit: String(limit) })
    if (device) q.set('device', device)
    if (label) q.set('label', label)
    if (before) q.set('before', String(before))
    return call('GET', `/api/wakes?${q}`)
  },
  markWake: (id, label) => call('PATCH', `/api/wakes/${id}`, { label }),
  deleteWake: (id) => call('DELETE', `/api/wakes/${id}`),
  stats: ({ device, days = 30 } = {}) => {
    const q = new URLSearchParams({ days: String(days) })
    if (device) q.set('device', device)
    return call('GET', `/api/stats?${q}`)
  },
  musicStats: ({ device, days = 30 } = {}) => {
    const q = new URLSearchParams({ days: String(days) })
    if (device) q.set('device', device)
    return call('GET', `/api/music?${q}`)
  },

  /** The file as the body, with progress (fetch has none for uploads). → the sound's entry */
  upload(name, file, { replace = false, onProgress } = {}) {
    return new Promise((resolve, reject) => {
      const x = new XMLHttpRequest()
      x.open('PUT', `/api/sounds/${enc(name)}${replace ? '?replace=1' : ''}`)
      x.setRequestHeader('Content-Type', file.type || 'application/octet-stream')
      x.upload.onprogress = (e) => e.lengthComputable && onProgress?.(e.loaded / e.total)
      x.onload = () => {
        let data = {}
        try {
          data = JSON.parse(x.responseText)
        } catch {
          /* not JSON: the status says it */
        }
        if (signedOutBy(x.status, '/api/sounds')) onSignedOut()
        if (x.status >= 200 && x.status < 300) resolve(data)
        else reject(new ApiError(x.status, data.error || `Ошибка ${x.status}`))
      }
      x.onerror = () => reject(new ApiError(0, 'Загрузка оборвалась'))
      x.send(file)
    })
  },
}
