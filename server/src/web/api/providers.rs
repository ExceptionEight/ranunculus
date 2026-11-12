//! Who does what (crate::providers): the connections — a service's address and its key, given once — and the roles:
//! which model of which connection hears, answers, speaks and makes the memory's vectors. A check of a role, and a
//! connection's list of models for one.
use super::*;
use crate::providers::{cloud, endpoint, llm_of, public, site, DEFAULTS, ROLES};
use crate::services::Services;
use crate::store::{ConnectionRow, RoleRow};

pub(super) async fn list_providers(State(app): State<App>) -> R {
    ok(app.shared.providers.json())
}

fn text(x: &Value, k: &str, max: usize) -> Result<String, Fail> {
    let t = x.as_str().ok_or_else(|| bad(format!("{k}: текст")))?.trim().to_string();
    if t.chars().count() > max || t.chars().any(|c| c.is_control()) {
        return Err(bad(format!("{k}: до {max} знаков")));
    }
    Ok(t)
}

fn failed(e: String) -> Fail {
    Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e)
}

// ---------------------------------------------------------------- connections

/// The fields of `v` into `c` (a new one: name and url required). `key`: absent — kept, "" — removed.
pub(super) fn connection_fields(v: &Value, c: &mut ConnectionRow) -> Result<(), Fail> {
    let o = v.as_object().ok_or_else(|| bad("нужен объект"))?;
    for (k, x) in o {
        match k.as_str() {
            "name" => c.name = text(x, "name", 60)?,
            "url" => {
                let u = text(x, "url", 300)?;
                if !(u.starts_with("http://") || u.starts_with("https://")) || u.contains(' ') {
                    return Err(bad("url: адрес http:// или https://, например https://openrouter.ai/api/v1"));
                }
                c.url = u.trim_end_matches('/').to_string();
            }
            "key" => c.key = text(x, "key", 400)?,
            other => return Err(bad(format!("{other}: такого поля нет"))),
        }
    }
    if c.name.is_empty() || c.url.is_empty() {
        return Err(bad("нужны name и url"));
    }
    Ok(())
}

/// A key just given to OpenRouter, nothing chosen yet: every role gets its model.
fn first_models(app: &App, c: &ConnectionRow) {
    let made = app.shared.providers.defaults(c);
    if !made.is_empty() {
        log("web", format!("models chosen with the key of «{}»: {}", c.name, made.join(", ")));
    }
}

pub(super) async fn add_connection(State(app): State<App>, h: HeaderMap, body: Bytes) -> R {
    let v = json_body(&h, &body)?;
    let mut c = ConnectionRow { id: 0, name: String::new(), url: String::new(), key: String::new(), updated: now_s() };
    connection_fields(&v, &mut c)?;
    c.id = app.store.save_connection(&c).map_err(failed)?;
    log("web", format!("connection «{}» added ({})", c.name, c.url));
    first_models(&app, &c);
    app.shared.providers.changed();
    Ok((StatusCode::CREATED, Json(public(&c))).into_response())
}

pub(super) async fn change_connection(State(app): State<App>, Path(id): Path<i64>, h: HeaderMap, body: Bytes) -> R {
    let ps = &app.shared.providers;
    let v = json_body(&h, &body)?;
    let mut c = ps.connection(id).ok_or_else(|| Fail::new(StatusCode::NOT_FOUND, "нет такого подключения"))?;
    let was = c.clone();
    connection_fields(&v, &mut c)?;
    // the key was given for that site: to another one it goes only when given again (else anyone in the house could
    // point the connection at their own server and have the key sent there)
    if site(&was.url) != site(&c.url) && v.get("key").is_none() && !c.key.is_empty() {
        return Err(bad("Адрес сменился на другой сайт: введите ключ заново или удалите его."));
    }
    c.updated = now_s();
    app.store.save_connection(&c).map_err(failed)?;
    log("web", format!("connection «{}» changed", c.name));
    if was.key.is_empty() {
        first_models(&app, &c);
    }
    ps.changed();
    ok(public(&c))
}

pub(super) async fn delete_connection(State(app): State<App>, Path(id): Path<i64>) -> R {
    let ps = &app.shared.providers;
    let c = ps.connection(id).ok_or_else(|| Fail::new(StatusCode::NOT_FOUND, "нет такого подключения"))?;
    // a role left without its service would go silent unasked
    if ps.roles().iter().any(|r| r.connection == id) {
        return Err(Fail::new(
            StatusCode::CONFLICT,
            "Это подключение используется. Сначала выберите для его ролей другое.",
        ));
    }
    app.store.delete_connection(id).map_err(failed)?;
    log("web", format!("connection «{}» deleted", c.name));
    ps.changed();
    ok(json!({"deleted": id}))
}

// ---------------------------------------------------------------- roles

fn known(role: &str) -> Result<(), Fail> {
    if ROLES.contains(&role) {
        Ok(())
    } else {
        Err(Fail::new(StatusCode::NOT_FOUND, "роли: asr, llm, tts, embeddings"))
    }
}

/// The fields of `v` into `r`: a connection there is, a model; a voice and its speed for the voice's role only.
/// Another model (or the same name elsewhere) does not get the last one's voice and request fields unless they are
/// given again: they were that model's (`dimensions` another may refuse, a voice it has not got); DEFAULTS' model
/// gets its own.
fn role_fields(app: &App, v: &Value, r: &mut RoleRow) -> Result<ConnectionRow, Fail> {
    let o = v.as_object().ok_or_else(|| bad("нужен объект"))?;
    let was = (r.connection, r.model.clone());
    for (k, x) in o {
        match k.as_str() {
            "connection" => r.connection = x.as_i64().ok_or_else(|| bad("connection: номер подключения"))?,
            "model" => r.model = text(x, "model", 200)?,
            "voice" if r.role == "tts" => r.voice = text(x, "voice", 100)?,
            "speed" if r.role == "tts" => {
                r.speed = opt_num(x, |s| (0.5..=3.0).contains(&s), "speed: от 0,5 до 3 или null")?
            }
            "extra" => {
                if !x.is_object() {
                    return Err(bad("extra: объект"));
                }
                r.extra = x.clone();
            }
            other => return Err(bad(format!("{other}: такого поля нет"))),
        }
    }
    if (r.connection, r.model.as_str()) != (was.0, was.1.as_str()) {
        let ours = DEFAULTS.iter().find(|(role, model, ..)| *role == r.role && *model == r.model);
        if !o.contains_key("extra") {
            r.extra = ours.and_then(|d| serde_json::from_str(d.3).ok()).unwrap_or(json!({}));
        }
        if !o.contains_key("voice") {
            r.voice = ours.map(|d| d.2.to_string()).unwrap_or_default();
        }
    }
    app.shared
        .providers
        .connection(r.connection)
        .ok_or_else(|| Fail::new(StatusCode::NOT_FOUND, format!("нет подключения {}", r.connection)))
}

/// What the role uses now, or an empty one to fill in.
fn role_now(app: &App, role: &str) -> RoleRow {
    app.shared.providers.role(role).map(|(_, r)| r).unwrap_or(RoleRow {
        role: role.into(),
        connection: 0,
        model: String::new(),
        voice: String::new(),
        speed: None,
        extra: json!({}),
        updated: now_s(),
    })
}

/// PUT /api/roles/{role} {connection, model, …}: what every station uses for it from the next request.
pub(super) async fn set_role(State(app): State<App>, Path(role): Path<String>, h: HeaderMap, body: Bytes) -> R {
    known(&role)?;
    let v = json_body(&h, &body)?;
    let mut r = role_now(&app, &role);
    let c = role_fields(&app, &v, &mut r)?;
    r.updated = now_s();
    app.store.save_role(&r).map_err(failed)?;
    log(
        "web",
        format!("in use for {role}: {} of «{}»", if r.model.is_empty() { "the model" } else { &r.model }, c.name),
    );
    app.shared.providers.changed();
    ok(app.shared.providers.json())
}

/// DELETE /api/roles/embeddings: the memory is searched by words. The other roles are changed, not given up:
/// without them the stations do not work.
pub(super) async fn clear_role(State(app): State<App>, Path(role): Path<String>) -> R {
    known(&role)?;
    if role != "embeddings" {
        return Err(bad("без этой роли колонки не работают: выберите для неё другую модель"));
    }
    app.store.delete_role(&role).map_err(failed)?;
    log("web", format!("nothing in use for {role}"));
    app.shared.providers.changed();
    ok(app.shared.providers.json())
}

/// The check phrase for recognition: said by the voice in use, heard by the recognition.
pub(super) const HEAR: &str = "Проверка распознавания речи.";

/// POST /api/roles/{role}/check: the role as it is in use, or — with a body as for PUT — as it would be (a voice
/// listened to before it is chosen). A model answers, recognition hears a phrase, a voice speaks (WAV), the memory
/// gets its vectors.
pub(super) async fn check_role(State(app): State<App>, Path(role): Path<String>, h: HeaderMap, body: Bytes) -> R {
    known(&role)?;
    let ps = &app.shared.providers;
    let mut r = role_now(&app, &role);
    let c = if body.is_empty() {
        ps.connection(r.connection)
            .ok_or_else(|| Fail::new(StatusCode::NOT_FOUND, "для этой роли ничего не выбрано"))?
    } else {
        role_fields(&app, &json_body(&h, &body)?, &mut r)?
    };
    let t0 = now_s();
    let fail = |e: String| Fail::new(StatusCode::UNPROCESSABLE_ENTITY, e);
    let e = endpoint(&c, &r);
    match role.as_str() {
        "llm" => {
            // room for a model that thinks before it answers: 20 tokens may all go on thinking
            let body = json!({"messages": [{"role": "user", "content": "Ответь одним словом: «готово»."}], "max_tokens": 400, "temperature": 0,
                              "chat_template_kwargs": {"enable_thinking": false}});
            let v = llm_of(&c, &r).complete(&body, Duration::from_secs(40)).await.map_err(|e| fail(e.to_string()))?;
            let text = v["choices"][0]["message"]["content"].as_str().unwrap_or("").trim().to_string();
            ok(json!({"ok": true, "text": text, "seconds": now_s() - t0}))
        }
        "tts" => {
            let voice = Services::new(None, Some(e), r.speed.map(|s| s as f32));
            let s = voice.speak("Проверка голоса: раз, два, три.").await.map_err(|e| fail(e.to_string()))?;
            Ok((
                [
                    (header::CONTENT_TYPE, "audio/wav".to_string()),
                    (header::HeaderName::from_static("x-seconds"), format!("{:.2}", now_s() - t0)),
                ],
                crate::services::wav(&s.pcm, s.rate),
            )
                .into_response())
        }
        "asr" => {
            // the voice in use says it; 16 kHz, as the station's microphone gives it
            let voice = ps.services();
            if voice.tts.is_none() {
                return Err(fail("Фразу для проверки произносит голос: сначала выберите его.".into()));
            }
            let s = voice.speak(HEAR).await.map_err(|e| fail(format!("голос для проверки: {e}")))?;
            let wav16 = resample16(&crate::services::wav(&s.pcm, s.rate)).await.map_err(fail)?;
            let t1 = now_s();
            let text = Services::new(Some(e), None, None).transcribe_wav(wav16).await.map_err(fail)?;
            ok(json!({"ok": true, "text": text, "expected": HEAR, "seconds": now_s() - t1}))
        }
        _ => {
            let v = crate::services::embed(&e, &["Проверка памяти.".into(), "Кота зовут Барсик.".into()])
                .await
                .map_err(fail)?;
            ok(json!({"ok": true, "size": v[0].len(), "seconds": now_s() - t0}))
        }
    }
}

/// Any WAV → 16 kHz mono WAV (ffmpeg).
pub(super) async fn resample16(wav: &[u8]) -> Result<Vec<u8>, String> {
    // files of this check's own: two checks at once (two tabs) do not write over each other's
    let (dir, me) =
        (std::env::temp_dir(), format!("server-check-{}-{:08x}", std::process::id(), rand::random::<u32>()));
    let (i, o) = (dir.join(format!("{me}.wav")), dir.join(format!("{me}-16k.wav")));
    tokio::fs::write(&i, wav).await.map_err(|e| e.to_string())?;
    let run = tokio::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(&i)
        .args(["-ac", "1", "-ar", "16000"])
        .arg(&o)
        .kill_on_drop(true)
        .output();
    let out = match tokio::time::timeout(std::time::Duration::from_secs(30), run).await {
        Ok(r) => r.map_err(|e| format!("ffmpeg: {e}")),
        Err(_) => Err("ffmpeg: не уложился в 30 с".into()),
    };
    let r = if out.as_ref().is_ok_and(|o| o.status.success()) {
        tokio::fs::read(&o).await.map_err(|e| e.to_string())
    } else {
        Err(out.err().unwrap_or_else(|| "ffmpeg не справился".into()))
    };
    let _ = tokio::fs::remove_file(&i).await;
    let _ = tokio::fs::remove_file(&o).await;
    r
}

// ---------------------------------------------------------------- a connection's models

/// (how a role's models are reached, what OpenRouter's list is asked for)
fn reached(role: &str) -> (&'static str, &'static str) {
    match role {
        "asr" => ("/audio/transcriptions", "transcription"),
        "tts" => ("/audio/speech", "speech"),
        "embeddings" => ("/embeddings", "embeddings"),
        _ => ("/chat/completions", ""),
    }
}

/// POST /api/connections/{id}/models {"role"}: the service's models for the role. OpenRouter says the prices of the
/// models that answer and of the embeddings ($ per million tokens), whether a model can call tools — the assistant
/// needs that — and a voice model's voices; a server at home says the names only, and all of them.
pub(super) async fn models(State(app): State<App>, Path(id): Path<i64>, h: HeaderMap, body: Bytes) -> R {
    let v = json_body(&h, &body)?;
    let role = v["role"].as_str().unwrap_or("");
    known(role)?;
    let c = app
        .shared
        .providers
        .connection(id)
        .ok_or_else(|| Fail::new(StatusCode::NOT_FOUND, "нет такого подключения"))?;
    let http = crate::http().timeout(Duration::from_secs(20)).build().map_err(|e| failed(e.to_string()))?;
    let mut req = http.get(format!("{}/models", c.url));
    // OpenRouter lists the models that answer unless asked for another kind; a server that does not know the
    // question lists everything
    let (_, kind) = reached(role);
    if !kind.is_empty() {
        req = req.query(&[("output_modalities", kind)]);
    }
    if !c.key.is_empty() {
        req = req.bearer_auth(&c.key);
    }
    let gone = |e: String| Fail::new(StatusCode::BAD_GATEWAY, e);
    let r = req.send().await.map_err(|e| gone(format!("{}: {e}", c.url)))?;
    if !r.status().is_success() {
        return Err(gone(format!("{}/models: HTTP {}", c.url, r.status())));
    }
    let j: Value = r.json().await.map_err(|e| gone(format!("{}/models: {e}", c.url)))?;
    ok(json!({"items": offered(&j, role), "cloud": cloud(&c.url)}))
}

/// The models of a service's list that do the role's job, the cheapest first where the list says the price, else
/// by name: none is put forward, the user chooses. OpenRouter says how each is reached, and its batch variants (the
/// cheapest of the list) are not reached by chat requests at all; a list that does not say is offered whole.
pub(super) fn offered(list: &Value, role: &str) -> Vec<Value> {
    let per_m = |x: &Value| x.as_str().and_then(|s| s.parse::<f64>().ok()).or(x.as_f64()).map(|p| p * 1e6);
    let (path, _) = reached(role);
    let does = |m: &Value| {
        m["endpoints"].as_array().is_none_or(|e| e.iter().any(|p| p.as_str().is_some_and(|p| p.ends_with(path))))
    };
    // a price per million tokens is what the models that take text are sold by; speech is sold by the character,
    // the second, the byte or the token, and the list does not say which
    let priced = matches!(role, "llm" | "embeddings");
    let mut items: Vec<Value> = list["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| does(m))
        .filter_map(|m| {
            let id = m["id"].as_str()?;
            let mut item = json!({"id": id, "name": m["name"].as_str().unwrap_or(id)});
            if priced {
                item["in"] = json!(per_m(&m["pricing"]["prompt"]));
                item["out"] = json!(per_m(&m["pricing"]["completion"]));
            }
            if role == "llm" {
                item["context"] = m["context_length"].clone();
                item["tools"] = json!(m["supported_parameters"].as_array().map(|a| a.iter().any(|x| x == "tools")));
            }
            if role == "tts" {
                item["voices"] = json!(m["supported_voices"].as_array().cloned().unwrap_or_default());
            }
            Some(item)
        })
        .collect();
    items.sort_by(|a, b| {
        let p = |x: &Value| x["in"].as_f64().unwrap_or(f64::MAX) + x["out"].as_f64().unwrap_or(0.0);
        (if priced { p(a).total_cmp(&p(b)) } else { std::cmp::Ordering::Equal })
            .then(a["id"].as_str().cmp(&b["id"].as_str()))
    });
    items
}
