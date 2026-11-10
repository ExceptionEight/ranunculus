//! Connections (crate::providers): the models, recognition and voices; which is in use; a check of one; a service's
//! list of models.
use super::*;

pub(super) async fn list_providers(State(app): State<App>) -> R {
    ok(app.shared.providers.json())
}

/// The fields of `v` into `p` (a new one: kind, name and url required). `key`: absent — kept, "" — removed.
pub(super) fn provider_fields(v: &Value, p: &mut crate::store::ProviderRow, new: bool) -> Result<(), Fail> {
    let o = v.as_object().ok_or_else(|| bad("нужен объект"))?;
    let text = |x: &Value, k: &str, max: usize| -> Result<String, Fail> {
        let t = x.as_str().ok_or_else(|| bad(format!("{k}: текст")))?.trim().to_string();
        if t.chars().count() > max || t.chars().any(|c| c.is_control()) {
            return Err(bad(format!("{k}: до {max} знаков")));
        }
        Ok(t)
    };
    for (k, x) in o {
        match k.as_str() {
            "kind" if new => {
                let kind = x.as_str().unwrap_or("");
                if !crate::providers::KINDS.contains(&kind) {
                    return Err(bad("kind: llm, asr или tts"));
                }
                p.kind = kind.into();
            }
            "kind" => return Err(bad("вид подключения не меняется")),
            "name" => p.name = text(x, "name", 60)?,
            "url" => {
                let u = text(x, "url", 300)?;
                if !(u.starts_with("http://") || u.starts_with("https://")) || u.contains(' ') {
                    return Err(bad("url: адрес http:// или https://, например https://openrouter.ai/api/v1"));
                }
                p.url = u.trim_end_matches('/').to_string();
            }
            "model" => p.model = text(x, "model", 200)?,
            "voice" => p.voice = text(x, "voice", 100)?,
            "key" => p.key = text(x, "key", 400)?,
            "speed" => p.speed = opt_num(x, |s| (0.5..=3.0).contains(&s), "speed: от 0,5 до 3 или null")?,
            "extra" => {
                if !x.is_object() {
                    return Err(bad("extra: объект"));
                }
                p.extra = x.clone();
            }
            other => return Err(bad(format!("{other}: такого поля нет"))),
        }
    }
    if p.name.is_empty() || p.url.is_empty() || p.kind.is_empty() {
        return Err(bad("нужны kind, name и url"));
    }
    Ok(())
}

pub(super) async fn add_provider(State(app): State<App>, h: HeaderMap, body: Bytes) -> R {
    let ps = &app.shared.providers;
    let v = json_body(&h, &body)?;
    let mut p = crate::store::ProviderRow {
        id: 0,
        kind: String::new(),
        name: String::new(),
        url: String::new(),
        model: String::new(),
        key: String::new(),
        voice: String::new(),
        speed: None,
        extra: json!({}),
        updated: now_s(),
    };
    provider_fields(&v, &mut p, true)?;
    p.id = app.store.save_provider(&p).map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    log("web", format!("connection «{}» added ({}, {})", p.name, p.kind, p.url));
    ps.changed();
    Ok((StatusCode::CREATED, Json(crate::providers::public(&p))).into_response())
}

/// «https://openrouter.ai» of «https://openrouter.ai/api/v1»: where a key goes.
pub(super) fn site(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split('/').next().unwrap_or("");
    format!("{}://{}", url.split("://").next().unwrap_or(""), host.to_ascii_lowercase())
}

pub(super) async fn change_provider(State(app): State<App>, Path(id): Path<i64>, h: HeaderMap, body: Bytes) -> R {
    let ps = &app.shared.providers;
    let v = json_body(&h, &body)?;
    let mut p = ps.get(id).ok_or_else(|| Fail::new(StatusCode::NOT_FOUND, "нет такого подключения"))?;
    let was = p.url.clone();
    provider_fields(&v, &mut p, false)?;
    // the key was given for that site: to another one it goes only when given again (else anyone in the house could
    // point the connection at their own server and have the key sent there)
    if site(&was) != site(&p.url) && v.get("key").is_none() && !p.key.is_empty() {
        return Err(bad("Адрес сменился на другой сайт: введите ключ заново или удалите его."));
    }
    p.updated = now_s();
    app.store.save_provider(&p).map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    log("web", format!("connection «{}» changed", p.name));
    ps.changed();
    ok(crate::providers::public(&p))
}

pub(super) async fn delete_provider(State(app): State<App>, Path(id): Path<i64>) -> R {
    let ps = &app.shared.providers;
    let p = ps.get(id).ok_or_else(|| Fail::new(StatusCode::NOT_FOUND, "нет такого подключения"))?;
    if ps.uses(id) {
        // another one would take its place unasked; without recognition or a voice the stations do not work at all
        if ps.list().iter().any(|x| x.kind == p.kind && x.id != id) {
            return Err(Fail::new(
                StatusCode::CONFLICT,
                "Это подключение сейчас используется. Сначала выберите другое.",
            ));
        }
        if p.kind != "llm" {
            return Err(Fail::new(
                StatusCode::CONFLICT,
                "Это единственное подключение такого вида. Сначала добавьте другое.",
            ));
        }
    }
    app.store.delete_provider(id).map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    log("web", format!("connection «{}» deleted", p.name));
    ps.changed();
    ok(json!({"deleted": id}))
}

/// PATCH /api/providers/in-use {"llm": 3, …}: the provider of a kind every station uses from the next request. All of
/// it checked first, then written, then told: only the database itself failing can stop it halfway, and then what
/// was written is told all the same.
pub(super) async fn choose_providers(State(app): State<App>, h: HeaderMap, body: Bytes) -> R {
    let ps = &app.shared.providers;
    let v = json_body(&h, &body)?;
    let mut chosen = vec![];
    for (kind, x) in v.as_object().into_iter().flatten() {
        let id = x.as_i64().ok_or_else(|| bad(format!("{kind}: номер подключения")))?;
        let p = ps.get(id).ok_or_else(|| Fail::new(StatusCode::NOT_FOUND, format!("нет подключения {id}")))?;
        if p.kind != *kind {
            return Err(bad(format!("подключение {id} — не {kind}")));
        }
        chosen.push(p);
    }
    let mut failed = None;
    for p in &chosen {
        if let Err(e) = ps.choose(&p.kind, p.id) {
            failed = Some(Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e));
            break;
        }
        log("web", format!("in use for {}: «{}»", p.kind, p.name));
    }
    ps.changed();
    match failed {
        Some(f) => Err(f),
        None => ok(ps.json()),
    }
}

/// The check phrase for recognition: said by the voice in use, heard by the provider.
pub(super) const HEAR: &str = "Проверка распознавания речи.";

pub(super) async fn check_provider(State(app): State<App>, Path(id): Path<i64>) -> R {
    let ps = &app.shared.providers;
    let p = ps.get(id).ok_or_else(|| Fail::new(StatusCode::NOT_FOUND, "нет такого подключения"))?;
    let t0 = now_s();
    let fail = |e: String| Fail::new(StatusCode::UNPROCESSABLE_ENTITY, e);
    match p.kind.as_str() {
        "llm" => {
            // room for a model that thinks before it answers: 20 tokens may all go on thinking
            let body = json!({"messages": [{"role": "user", "content": "Ответь одним словом: «готово»."}], "max_tokens": 400, "temperature": 0,
                              "chat_template_kwargs": {"enable_thinking": false}});
            let v = crate::providers::llm_of(&p)
                .complete(&body, Duration::from_secs(40))
                .await
                .map_err(|e| fail(e.to_string()))?;
            let text = v["choices"][0]["message"]["content"].as_str().unwrap_or("").trim().to_string();
            ok(json!({"ok": true, "text": text, "seconds": now_s() - t0}))
        }
        "tts" => {
            let base = ps.services();
            let services = crate::services::Services::with(
                base.asr.clone(),
                crate::providers::endpoint(&p),
                p.speed.map(|s| s as f32).unwrap_or(base.speed()),
            );
            let s = services.speak("Проверка голоса: раз, два, три.").await.map_err(|e| fail(e.to_string()))?;
            Ok((
                [
                    (header::CONTENT_TYPE, "audio/wav".to_string()),
                    (header::HeaderName::from_static("x-seconds"), format!("{:.2}", now_s() - t0)),
                ],
                crate::services::wav(&s.pcm, s.rate),
            )
                .into_response())
        }
        _ => {
            // the voice in use says it; 16 kHz, as the station's microphone gives it
            let voice = ps.services();
            let s = voice.speak(HEAR).await.map_err(|e| fail(format!("голос для проверки: {e}")))?;
            let wav16 = resample16(&crate::services::wav(&s.pcm, s.rate)).await.map_err(fail)?;
            let services =
                crate::services::Services::with(crate::providers::endpoint(&p), voice.tts.clone(), voice.speed());
            let t1 = now_s();
            let (text, _) = services.transcribe_wav(wav16).await.map_err(fail)?;
            ok(json!({"ok": true, "text": text, "expected": HEAR, "seconds": now_s() - t1}))
        }
    }
}

/// Any WAV → 16 kHz mono WAV (ffmpeg).
pub(super) async fn resample16(wav: &[u8]) -> Result<Vec<u8>, String> {
    let dir = std::env::temp_dir();
    let (i, o) = (
        dir.join(format!("server-check-{}.wav", std::process::id())),
        dir.join(format!("server-check-{}-16k.wav", std::process::id())),
    );
    tokio::fs::write(&i, wav).await.map_err(|e| e.to_string())?;
    let out = tokio::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(&i)
        .args(["-ac", "1", "-ar", "16000"])
        .arg(&o)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let r = if out.status.success() {
        tokio::fs::read(&o).await.map_err(|e| e.to_string())
    } else {
        Err("ffmpeg не справился".into())
    };
    let _ = tokio::fs::remove_file(&i).await;
    let _ = tokio::fs::remove_file(&o).await;
    r
}

/// A service's models: {url, key} (a new provider), or {id} (a saved one: its key). OpenRouter says the prices
/// ($ per million tokens) and whether a model can call tools — the assistant needs that.
pub(super) async fn models(State(app): State<App>, h: HeaderMap, body: Bytes) -> R {
    let v = json_body(&h, &body)?;
    let saved = v["id"].as_i64().and_then(|id| app.shared.providers.get(id));
    let url = v["url"]
        .as_str()
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .or(saved.as_ref().map(|p| p.url.clone()))
        .ok_or_else(|| bad("нужен url или id"))?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(bad("url: адрес http:// или https://"));
    }
    // a saved key goes only to the site it was given for
    let saved_key = saved.filter(|p| site(&p.url) == site(&url)).map(|p| p.key);
    let key = v["key"].as_str().filter(|k| !k.is_empty()).map(String::from).or(saved_key).unwrap_or_default();
    let http = crate::http()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let mut req = http.get(format!("{url}/models"));
    if !key.is_empty() {
        req = req.bearer_auth(&key);
    }
    let r = req.send().await.map_err(|e| Fail::new(StatusCode::BAD_GATEWAY, format!("{url}: {e}")))?;
    if !r.status().is_success() {
        return Err(Fail::new(StatusCode::BAD_GATEWAY, format!("{url}/models: HTTP {}", r.status())));
    }
    let j: Value = r.json().await.map_err(|e| Fail::new(StatusCode::BAD_GATEWAY, format!("{url}/models: {e}")))?;
    let mut items = offered(&j);
    items.sort_by(|a, b| {
        let p = |x: &Value| x["in"].as_f64().unwrap_or(f64::MAX) + x["out"].as_f64().unwrap_or(0.0);
        p(a).total_cmp(&p(b)).then(a["id"].as_str().cmp(&b["id"].as_str()))
    });
    ok(json!({"items": items}))
}

/// The models of a service's list that can answer in a conversation. OpenRouter says how each is reached, and its
/// batch variants (the cheapest of the list) are not reached by chat requests at all; a list that does not say
/// is offered whole.
pub(super) fn offered(list: &Value) -> Vec<Value> {
    let per_m = |x: &Value| x.as_str().and_then(|s| s.parse::<f64>().ok()).or(x.as_f64()).map(|p| p * 1e6);
    let chats = |m: &Value| {
        m["endpoints"]
            .as_array()
            .is_none_or(|e| e.iter().any(|p| p.as_str().is_some_and(|p| p.ends_with("/chat/completions"))))
    };
    list["data"].as_array().into_iter().flatten().filter(|m| chats(m)).filter_map(|m| {
        let id = m["id"].as_str()?;
        let tools = m["supported_parameters"].as_array().map(|a| a.iter().any(|x| x == "tools"));
        Some(json!({"id": id, "name": m["name"].as_str().unwrap_or(id), "in": per_m(&m["pricing"]["prompt"]), "out": per_m(&m["pricing"]["completion"]),
                    "context": m["context_length"], "tools": tools}))
    }).collect()
}
