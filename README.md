<h1 align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://github.com/ExceptionEight/ranunculus/raw/main/.github/assets/wordmark-dark.svg">
    <img src="https://github.com/ExceptionEight/ranunculus/raw/main/.github/assets/wordmark-light.svg" alt="Ranunculus" height="56">
  </picture>
</h1>

<h3 align="center">Замена бэкенда Яндекс Станции 2 для вашего хоумлаба</h3>

<p align="center">
  <a href="https://github.com/ExceptionEight/ranunculus/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/ExceptionEight/ranunculus/ci.yml?branch=main&label=CI" alt="CI"></a>
  <a href="https://github.com/ExceptionEight/ranunculus/releases/latest"><img src="https://img.shields.io/github/v/release/ExceptionEight/ranunculus?label=release&color=blue" alt="Релиз"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="Лицензия MIT"></a>
</p>

Ranunculus заменяет облачный бэкенд Яндекс Станции 2 вашим собственным сервером. На колонке с root-доступом вместо
Алисы работает небольшой клиент: он слушает слово активации «Мразота» (ну или можете обучить на свое любое и поменять системный промпт запросов) и передаёт звук на сервер. Музыка,
поиск, будильники и веб-интерфейс работают у вас дома в Docker. Распознают речь, отвечают и говорят модели по вашему
выбору. Проще всего взять облачные через OpenRouter. Подойдёт и любой свой сервер с API OpenAI,
например Ollama или llama.cpp.

<p align="center">
  <img src=".github/assets/screenshots/station.webp" alt="Страница колонки: состояние, громкость, таймер и будильники">
</p>
<p align="center">
  <img src=".github/assets/screenshots/analytics.webp" width="49%" alt="Аналитика: запросы по дням и по времени суток, что делал ассистент">
  <img src=".github/assets/screenshots/models.webp" width="49%" alt="Настройки: модели для распознавания, ответов, голоса и памяти">
</p>

## Возможности

- Отвечает голосом, ищет в интернете и читает найденные страницы.
- Включает музыку с YouTube Music, ставит на паузу, переключает, перематывает.
- Ставит таймеры и будильники, говорит погоду, меняет громкость.
- Помнит прошлые разговоры и ведёт заметки о том, что вы о себе рассказали.
- Показывает на панели колонки, что делает: слушает, думает, ищет, говорит, играет музыку.
- Веб-интерфейс: колонки, история разговоров, будильники, звуки, статистика, настройки.

## Что нужно

- Яндекс Станция 2 (YNDX-00051) с root-доступом по SSH. Проверено на прошивке со сборкой 4778263334. Как получить
  root, описано в вики [hackalice.me](https://hackalice.me).
- Сервер с Docker Compose. Хватит слабого компьютера: модели на нём не запускаются.
- Ключ [OpenRouter](https://openrouter.ai/keys) или свои сервисы с API OpenAI. OpenRouter берёт плату за
  использование, цены каждой модели есть на её странице.

## Установка

1. Сервер:

   ```sh
   git clone https://github.com/ExceptionEight/ranunculus.git
   cd ranunculus
   docker compose up -d
   ```

2. Откройте `https://АДРЕС_СЕРВЕРА:52702`. Сертификат самоподписанный, поэтому браузер один раз предупредит. Создайте
   пользователя и вставьте ключ OpenRouter в «Настройки → Модели». Модели для распознавания, ответов, голоса и
   памяти подберутся сами, любую потом можно сменить.

3. Колонка:

   ```sh
   station/install.sh АДРЕС_КОЛОНКИ
   ```

   Скрипт заходит на колонку по SSH (порт 2222, root), ставит клиент в `/data/ranunculus`, отключает Алису и
   перезагружает колонку. В прошивке добавляется только один файл автозапуска. Вернуть Алису:
   `station/install.sh --uninstall АДРЕС_КОЛОНКИ`.

Скажите: «Мразота, привет».

## Обновление

```sh
git pull && docker compose pull && docker compose up -d
```

Колонки обновляются сами, когда подключаются к серверу.

## Настройка

В «Настройки → Модели» есть подключения и роли. Подключение — это адрес сервиса и ключ к нему, ключ вводится один
раз. У каждой роли одна модель из одного подключения. Кроме OpenRouter подойдёт любой сервис с API OpenAI, в том числе
у вас дома:

| Роль | Что нужно от сервиса | Что выбирается на OpenRouter |
|---|---|---|
| Распознавание | `POST /v1/audio/transcriptions`: файл WAV, в ответе `{"text": "…"}` | AssemblyAI Universal-3.6 Pro |
| Ответы | `POST /v1/chat/completions` с потоковой выдачей и вызовом инструментов | DeepSeek V4.1 Flash |
| Голос | `POST /v1/audio/speech` с `response_format: "pcm"`: 16-битный PCM, частота в `Content-Type` (`audio/pcm;rate=24000`) или в `X-Sample-Rate`, без них 24 кГц. WAV и MP3 тоже подойдут | Grok Voice TTS 1.0, голос Eve |
| Память | `POST /v1/embeddings` | Gemini Embedding 2 |

- Список моделей сервер берёт из `GET /v1/models`. Если сервис его не отдаёт, название вводится вручную.
- Сервис на этом же компьютере доступен по имени `host.docker.internal`, например
  `http://host.docker.internal:11434/v1` для Ollama. Он должен слушать не только `127.0.0.1`.
- Память можно не использовать: тогда ассистент ищет в прошлых разговорах только по словам. При смене модели памяти
  векторы всех разговоров пересчитываются.
- Свой сертификат, обратный прокси и вход через OIDC задаются в `server.toml`. Ключи описаны в начале
  [`server/src/config.rs`](server/src/config.rs), файл монтируется в контейнер как `/app/server.toml`.

## Данные

Всё хранится в томе `ranunculus_data`: история и настройки, память, записи срабатываний слова активации, звуки, ключ и
сертификат. Сказанное в журнал сервера не пишется.

Облачные модели получают то, с чем работают: распознавание — запись каждого обращения, ответы — разговоры и заметки,
голос — текст ответов, память — текст разговоров. То, что прозвучало до слова активации, в облако не отправляется.
Любую роль можно перенести на свой сервер. Кроме моделей наружу ходят поиск, YouTube и погода Open-Meteo.

## Что стоит знать

- Музыка идёт через неофициальный клиент YouTube (yt-dlp). YouTube может сломать его в любой момент, и правила сервиса
  такого использования не разрешают.
- С облачными моделями колонка без интернета не слышит и не отвечает.
- Сброс колонки кнопками вернёт заводскую прошивку, а она обновится и закроет root.

## Разработка

```sh
docker compose up -d --build        # образы из этой копии, а не опубликованные
cargo test --workspace              # нужен ffmpeg
cd web && npm ci && npm run lint && npm run build
```

Код оформляют `cargo fmt --all`, `ruff format services tools` и `npm run format` в `web/`. CI это проверяет.

## Лицензия

[MIT](LICENSE). Сторонние компоненты и их лицензии перечислены в [THIRD-PARTY.md](THIRD-PARTY.md).
