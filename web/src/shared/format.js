// Russian words for numbers, times and states. Times are in the home's time zone (zone.js), as the server counts them.
import { tz } from './zone.js'

export function plural(n, [one, few, many]) {
  const a = Math.abs(n) % 100
  const b = a % 10
  if (a > 10 && a < 20) return many
  if (b > 1 && b < 5) return few
  if (b === 1) return one
  return many
}

const num = (x, d = 1) => x.toLocaleString('ru-RU', { maximumFractionDigits: d })

/** «13:04» */
export function clock(t) {
  return new Date(t * 1000).toLocaleTimeString('ru-RU', { timeZone: tz(), hour: '2-digit', minute: '2-digit' })
}

function dayKey(t) {
  return new Date(t * 1000).toLocaleDateString('en-CA', { timeZone: tz() })
}

export function sameDay(a, b) {
  return dayKey(a) === dayKey(b)
}

/** «Сегодня», «Вчера», «5 ноября», «5 ноября 2025» */
export function day(t, now = Date.now() / 1000) {
  if (sameDay(t, now)) return 'Сегодня'
  if (sameDay(t, now - 86400)) return 'Вчера'
  const d = new Date(t * 1000)
  const opts = { timeZone: tz(), day: 'numeric', month: 'long' }
  if (
    d.toLocaleDateString('en-CA', { timeZone: tz(), year: 'numeric' }) !==
    new Date(now * 1000).toLocaleDateString('en-CA', { timeZone: tz(), year: 'numeric' })
  ) {
    opts.year = 'numeric'
  }
  return d.toLocaleDateString('ru-RU', opts)
}

/** «суббота» */
export function weekday(t) {
  return new Date(t * 1000).toLocaleDateString('ru-RU', { timeZone: tz(), weekday: 'long' })
}

/** 90 → «1 мин 30 с», 600 → «10 мин», 3 → «3 с» */
export function span(s) {
  s = Math.round(s)
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const sec = s % 60
  const out = []
  if (h) out.push(`${h} ч`)
  if (m) out.push(`${m} мин`)
  if (sec || !out.length) out.push(`${sec} с`)
  return out.join(' ')
}

/** 90 → «1 минута 30 секунд», 7200 → «2 часа» */
/** A place in a track, as players show it: «3:07», «1:02:45». */
export function trackTime(s) {
  s = Math.max(0, Math.floor(s || 0))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const sec = String(s % 60).padStart(2, '0')
  return h ? `${h}:${String(m).padStart(2, '0')}:${sec}` : `${m}:${sec}`
}

/** Minutes as «47 мин», «5 ч 12 мин». */
export function minutes(x) {
  const n = Math.round(x || 0)
  const h = Math.floor(n / 60)
  return h ? `${h} ч${n % 60 ? ` ${n % 60} мин` : ''}` : `${n} мин`
}

export function spanLong(s) {
  s = Math.round(s)
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const sec = s % 60
  const out = []
  if (h) out.push(`${h} ${plural(h, ['час', 'часа', 'часов'])}`)
  if (m) out.push(`${m} ${plural(m, ['минута', 'минуты', 'минут'])}`)
  if (sec || !out.length) out.push(`${sec} ${plural(sec, ['секунда', 'секунды', 'секунд'])}`)
  return out.join(' ')
}

/** seconds with a comma: 1.6 → «1,6 с» */
export function secs(x) {
  return `${num(x, x < 10 ? 1 : 0)} с`
}

export function bytes(n) {
  if (n == null) return ''
  if (n < 1024) return `${n} Б`
  if (n < 1048576) return `${num(n / 1024, 0)} КБ`
  return `${num(n / 1048576, 1)} МБ`
}

/** «через 4 мин», «через 12 с» */
export function until(at, now = Date.now() / 1000) {
  const left = Math.max(0, at - now)
  return left < 1 ? 'сейчас' : `через ${span(left)}`
}

/** What a device is doing, in a word or two (never «тихо»: it read as the volume). */
export function status(d) {
  if (!d?.online) return 'Не в сети'
  if (d.ring) return { alarm: 'Звенит будильник', test: 'Проверка звука' }[d.ring.kind] || 'Звенит таймер'
  return (
    {
      listen: 'Слушает',
      think: 'Думает',
      search: 'Ищет в интернете',
      speak: 'Отвечает',
      music: 'Играет музыка',
    }[d.panel] || 'Ждёт команды'
  )
}

/** «Была в сети сегодня в 01:35», «… вчера в 23:10», «… 5 ноября в 09:00» */
export function lastSeen(t, now = Date.now() / 1000) {
  if (!t) return ''
  const d = day(t, now)
  return `Была в сети ${d === 'Сегодня' || d === 'Вчера' ? d.toLowerCase() : d} в ${clock(t)}`
}

/** Busy right now (the glyph moves): listening, thinking, talking, ringing. */
export function active(d) {
  return !!d?.online && (!!d.ring || ['listen', 'think', 'search', 'speak'].includes(d.panel))
}

const WD = ['пн', 'вт', 'ср', 'чт', 'пт', 'сб', 'вс']
// «во вторник», not «в вторник»
const ON_DAY = ['в понедельник', 'во вторник', 'в среду', 'в четверг', 'в пятницу', 'в субботу', 'в воскресенье']
export const WEEKDAYS = WD

/** An alarm's days (1 = Monday … 7) in words: «по будням», «пн, вт, чт, пт», «один раз» */
export function daysWords(days) {
  const k = [...days].sort().join('')
  if (!days.length) return 'один раз'
  if (k === '1234567') return 'каждый день'
  if (k === '12345') return 'по будням'
  if (k === '67') return 'по выходным'
  return [...days]
    .sort()
    .map((d) => WD[d - 1])
    .join(', ')
}

/** Only the day of an alarm's next ring, its time being shown beside: «сегодня», «завтра», «в среду», «12 ноября» */
export function ringDay(t, now = Date.now() / 1000) {
  if (!t) return 'выключен'
  if (sameDay(t, now)) return 'сегодня'
  if (sameDay(t, now + 86400)) return 'завтра'
  if (t - now < 7 * 86400) {
    const wd = new Date(t * 1000).toLocaleDateString('en-US', { timeZone: tz(), weekday: 'short' })
    return ON_DAY[['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'].indexOf(wd)]
  }
  return new Date(t * 1000).toLocaleDateString('ru-RU', { timeZone: tz(), day: 'numeric', month: 'long' })
}

/** The ring's duration: stops a slider moves over (3 s … 10 min), finer where it matters. */
export const RING_STOPS = [3, 5, 10, 15, 20, 30, 45, 60, 90, 120, 180, 240, 300, 420, 600]

export function nearestStop(s) {
  let best = 0
  RING_STOPS.forEach((v, i) => {
    if (Math.abs(v - s) < Math.abs(RING_STOPS[best] - s)) best = i
  })
  return best
}

/** The volume as a level, 0…10 by halves: «5», «3,5» */
export function level(v) {
  return v == null ? '' : num(v.level, 1)
}

/** A file's name without its extension, for a label. */
export function soundTitle(name) {
  if (name === 'chime') return 'Колокольчик'
  const i = name.lastIndexOf('.')
  return i > 0 ? name.slice(0, i) : name
}
