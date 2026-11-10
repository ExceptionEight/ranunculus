// What the assistant did with its tools, in words: a short label (a chip) and a stage direction («Ставит таймер на
// 3 минуты.»). A call is [name, args, result] as the chats keep it; only the arguments are relied on.
import { plural, span, trackTime } from './format.js'

const PLAYER = {
  pause: ['пауза', 'Ставит музыку на паузу'],
  resume: ['дальше играть', 'Снимает с паузы'],
  next: ['следующий трек', 'Переключает на следующий трек'],
  previous: ['предыдущий трек', 'Возвращает предыдущий трек'],
  restart: ['трек сначала', 'Включает трек сначала'],
  stop: ['музыка выключена', 'Выключает музыку'],
  repeat: ['на повторе', 'Ставит трек на повтор'],
  repeat_off: ['без повтора', 'Убирает повтор'],
  back: ['назад на 15 с', 'Перематывает на 15 секунд назад'],
  forward: ['вперёд на 15 с', 'Перематывает на 15 секунд вперёд'],
}

const DAY = { today: 'сегодня', tomorrow: 'завтра', after_tomorrow: 'послезавтра' }

function ok(res) {
  return !(res && typeof res === 'object' && res.ok === false)
}

/** → {kind, label, direction, failed} */
export function describe([name, args = {}, res]) {
  const a = args || {}
  let label, direction
  switch (name) {
    case 'set_timer':
      label = `таймер ${span(a.seconds || 0)}${a.label && a.label.toLowerCase() !== 'таймер' ? ` «${a.label}»` : ''}`
      direction = `Ставит таймер на ${span(a.seconds || 0)}${a.label ? ` («${a.label}»)` : ''}`
      break
    case 'set_volume':
      if (a.level != null) {
        label = `громкость ${a.level}`
        direction = `Делает громкость ${a.level}`
      } else if (a.change != null) {
        label = `громкость ${a.change > 0 ? '+' : '−'}${Math.abs(a.change)}`
        direction = a.change > 0 ? `Делает громче на ${a.change}` : `Делает тише на ${-a.change}`
      } else {
        label = 'узнала громкость'
        direction = 'Смотрит, какая громкость'
      }
      break
    case 'play_music':
      label = a.query || 'музыка'
      direction = `Включает «${a.query || 'музыку'}»`
      break
    case 'player_control': {
      if (a.action === 'seek') {
        const by = Number(a.seconds)
        if (a.position != null && a.position !== '') {
          label = `перемотка на ${trackTime(Number(a.position))}`
          direction = `Перематывает на ${trackTime(Number(a.position))}`
        } else {
          const where = by < 0 ? 'назад' : 'вперёд'
          label = `${where} на ${span(Math.abs(by || 0))}`
          direction = `Перематывает на ${span(Math.abs(by || 0))} ${where}`
        }
        break
      }
      const p = PLAYER[a.action] || [a.action, `Плеер: ${a.action}`]
      ;[label, direction] = p
      break
    }
    case 'get_weather':
      label = `погода${a.city ? `, ${a.city}` : ''}`
      direction = `Смотрит погоду${a.city ? ` в городе ${a.city}` : ''}${a.day && a.day !== 'today' ? ` на ${DAY[a.day] || a.day}` : ''}`
      break
    case 'web_search':
      label = a.query || 'поиск'
      direction = `Ищет в интернете «${a.query || ''}»`
      break
    case 'read_page': {
      let host = a.url || ''
      try {
        host = new URL(a.url).hostname.replace(/^www\./, '')
      } catch {
        /* not a URL: as is */
      }
      label = host
      direction = `Читает ${host}${a.part > 1 ? `, часть ${a.part}` : ''}`
      break
    }
    case 'recall':
      label = 'вспоминает'
      direction = `Вспоминает${a.query ? `: «${a.query}»` : ''}`
      break
    case 'calculate':
      label = a.expression || 'считает'
      direction = `Считает ${a.expression || ''}`.trim()
      break
    case 'date_calc':
      {
        const n = a.days_from_today
        const days = n == null ? '' : `${Math.abs(n)} ${plural(n, ['день', 'дня', 'дней'])}`
        label = a.date ? `до ${a.date}` : n < 0 ? `${days} назад` : `через ${days}`
        direction = a.date
          ? `Считает дни до ${a.date}`
          : n < 0
            ? `Смотрит, какой день был ${days} назад`
            : `Смотрит, какой день будет через ${days}`
      }
      break
    default:
      label = name
      direction = name
  }
  return { kind: name, label, direction: `${direction}.`, failed: !ok(res) }
}
