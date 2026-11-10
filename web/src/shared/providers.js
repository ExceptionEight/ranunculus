// The three kinds of models (providers.rs) in words, for Settings → Модели.

/** In the order they are listed. */
export const KINDS = ['llm', 'asr', 'tts']

/** A kind's name: a section's heading, a choice's label. */
export const KIND_TITLE = { llm: 'Ответы', asr: 'Распознавание речи', tts: 'Синтез речи' }

/** What it does. */
export const KIND_ABOUT = {
  llm: 'Языковая модель, которая отвечает на запросы.',
  asr: 'Переводит сказанное в текст.',
  tts: 'Озвучивает ответы.',
}

/** What leaves the house if this one is chosen. */
export function cloudNote(kind) {
  if (kind === 'llm') return 'Облачный сервис: получает разговоры, память ассистента и заметки о пользователях.'
  if (kind === 'asr') return 'Облачный сервис: получает запись каждой команды.'
  return 'Облачный сервис: получает текст каждого ответа.'
}
