// The roles a model is chosen for (providers.rs) in words, for Settings → Модели.

/** In the order a request goes through them; the memory last. */
export const ROLES = ['asr', 'llm', 'tts', 'embeddings']

/** A role's name: a section's heading. */
export const ROLE_TITLE = { asr: 'Распознавание речи', llm: 'Ответы', tts: 'Голос', embeddings: 'Память' }

/** What it does. */
export const ROLE_ABOUT = {
  asr: 'Переводит сказанное в текст.',
  llm: 'Языковая модель, которая отвечает на запросы.',
  tts: 'Озвучивает ответы.',
  embeddings: 'Находит прошлые разговоры.',
}

/** What is said while a role has no model. */
export const ROLE_NONE = {
  asr: 'Не выбрано: колонка не поймёт, что ей говорят.',
  llm: 'Не выбрано: отвечать некому.',
  tts: 'Не выбрано: ответы не будут слышны.',
  embeddings: 'Не выбрано: в прошлых разговорах ассистент ищет только по словам.',
}

/** What leaves the house if a service in the internet does the role. */
export function cloudNote(role) {
  if (role === 'llm') return 'Облачный сервис: получает разговоры, память ассистента и заметки о пользователях.'
  if (role === 'asr') return 'Облачный сервис: получает запись каждого обращения.'
  if (role === 'embeddings') return 'Облачный сервис: получает текст всех разговоров, прошлых и новых.'
  return 'Облачный сервис: получает текст каждого ответа.'
}

/** Nothing chosen yet: the key of OpenRouter alone chooses every model (providers.rs, DEFAULTS). */
export function nothingChosen(providers) {
  return !!providers && ROLES.every((r) => !providers.roles[r])
}

/** Away from the house by its address, as the server counts it (providers.rs, cloud): not a container's name, this
 * computer, a private address, a name of the local network or Tailscale. An address not complete yet: not away. */
export function away(url) {
  let host
  try {
    host = new URL(url).hostname.toLowerCase().replace(/^\[|\]$/g, '')
  } catch {
    return false
  }
  const v4 = host.match(/^(\d+)\.(\d+)\.\d+\.\d+$/)
  if (v4) {
    const [a, b] = [Number(v4[1]), Number(v4[2])]
    const home =
      a === 10 ||
      a === 127 ||
      (a === 172 && b >= 16 && b <= 31) ||
      (a === 192 && b === 168) ||
      (a === 169 && b === 254) ||
      (a === 100 && b >= 64 && b <= 127)
    return !home
  }
  if (host.includes(':')) return !(host === '::1' || /^f[cd]/.test(host) || /^fe[89ab]/.test(host))
  return host.includes('.') && !['.local', '.lan', '.home.arpa', '.internal', '.ts.net'].some((s) => host.endsWith(s))
}

/** The roles the stations do not work without, of those that have no model yet. */
export function missing(providers) {
  if (!providers) return []
  return ['asr', 'llm', 'tts'].filter((r) => !providers.roles[r])
}
