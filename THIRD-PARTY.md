# Сторонние компоненты

Код Ranunculus распространяется по лицензии MIT. Ниже то, что в него включено или что он скачивает и запускает.
У каждого компонента своя лицензия.

## В репозитории и в собранных программах

| Что | Где | Лицензия |
|---|---|---|
| Веса Silero VAD 6.2.3 | `crates/vad/assets/silero_vad_16k.safetensors` | MIT, © Silero Team |
| Шрифт Rubik | веб-интерфейс, пакет `@fontsource-variable/rubik` | SIL Open Font License 1.1 |
| Иконки Lucide | веб-интерфейс, пакет `lucide-react` | ISC |
| React, React Router | веб-интерфейс | MIT |
| Библиотеки Rust | сервер и клиент колонки, список в `Cargo.lock` | в основном MIT и Apache-2.0 |

Модель слова активации `station/models/mrazota.rkw` обучена с нуля для этого проекта и распространяется вместе с
кодом по лицензии MIT.

Поиск музыки через InnerTube сделан по материалам проекта
[Marker284/yandex-station-extended](https://github.com/Marker284/yandex-station-extended) (CC BY 4.0).

## Скачивается и запускается сервисами

| Что | Сервис | Лицензия |
|---|---|---|
| GigaAM v3, модель и код | `asr` | MIT |
| Голоса Silero `v5_6_ru` | `tts` | CC BY-NC-SA 4.0: только некоммерческое использование |
| multilingual-e5-large-instruct | `embeddings` | MIT |
| PyTorch | `asr`, `tts`, `embeddings` | BSD-3-Clause |
| sentence-transformers | `embeddings` | Apache-2.0 |
| num2words | `tts` | LGPL-2.1 |
| pymorphy3 и словари OpenCorpora | `tts` | MIT; словари CC BY-SA 3.0 |
| yt-dlp | `music` | Unlicense |
| FFmpeg из пакетов Debian | `server`, `music` | LGPL и GPL, по сборке Debian |
| SearXNG, готовый образ без изменений | `search` | AGPL-3.0 |

Прогноз погоды и поиск городов: [Open-Meteo](https://open-meteo.com), данные по лицензии CC BY 4.0.

## На колонке

Ranunculus не содержит файлов прошивки Яндекс Станции. Клиент читает на самой колонке её собственные файлы:
анимации панели и настройки запуска.
