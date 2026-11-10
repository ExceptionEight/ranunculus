//! The sounds timers and alarms ring with: the built-in one and the user's uploads.
use super::*;
use crate::sounds::{Sound, CHIME, RATE};
use tokio::io::AsyncWriteExt;

/// «прослушать» a long file: its first minute
pub(super) const PREVIEW_MAX: f64 = 60.0;

/// One sound for the list: its length (decoded once per version of the file), or why it does not play.
pub(super) async fn sound_item(
    app: &App,
    name: &str,
    bytes: Option<u64>,
    modified: Option<std::time::SystemTime>,
) -> Value {
    if name == CHIME {
        return json!({"name": CHIME, "builtin": true, "bytes": null, "modified": null,
                      "seconds": crate::sounds::chime().pcm.len() as f64 / RATE as f64, "error": null});
    }
    let (b, m) = (bytes.unwrap_or(0), modified.unwrap_or(std::time::UNIX_EPOCH));
    let cached = app.lengths.lock().unwrap().get(name).filter(|(mm, bb, _)| *mm == m && *bb == b).map(|x| x.2.clone());
    let len = match cached {
        Some(l) => l,
        None => {
            let l = crate::sounds::load(&app.shared.sounds_dir, name).await.map(|s| s.pcm.len() as f64 / RATE as f64);
            app.lengths.lock().unwrap().insert(name.to_string(), (m, b, l.clone()));
            l
        }
    };
    let t = m.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).ok();
    json!({"name": name, "builtin": false, "bytes": b, "modified": t, "seconds": len.as_ref().ok(), "error": len.as_ref().err()})
}

pub(super) async fn sound_list(app: &App) -> Value {
    let mut v = vec![sound_item(app, CHIME, None, None).await];
    for (name, bytes, modified) in crate::sounds::files(&app.shared.sounds_dir) {
        v.push(sound_item(app, &name, Some(bytes), Some(modified)).await);
    }
    json!(v)
}

pub(super) async fn list_sounds(State(app): State<App>) -> R {
    ok(sound_list(&app).await)
}

pub(super) fn sound_path(app: &App, name: &str) -> std::path::PathBuf {
    std::path::Path::new(&app.shared.sounds_dir).join(name)
}

pub(super) fn existing(app: &App, name: &str) -> Result<std::path::PathBuf, Fail> {
    if name == CHIME {
        return Err(bad("встроенный звук не меняется"));
    }
    crate::sounds::check_name(name).map_err(bad)?;
    let p = sound_path(app, name);
    if !p.is_file() {
        return Err(Fail::new(StatusCode::NOT_FOUND, format!("нет звука «{name}»")));
    }
    Ok(p)
}

/// A temporary upload file, removed unless kept.
pub(super) struct Upload(std::path::PathBuf, bool);

impl Drop for Upload {
    fn drop(&mut self) {
        if !self.1 {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

pub(super) async fn upload(
    State(app): State<App>,
    Extension(peer): Extension<Client>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    h: HeaderMap,
    body: Body,
) -> R {
    crate::sounds::check_name(&name).map_err(bad)?;
    let replace = q.get("replace").is_some_and(|v| v == "1" || v == "true");
    let dest = sound_path(&app, &name);
    if dest.exists() && !replace {
        return Err(Fail::new(StatusCode::CONFLICT, format!("«{name}» уже есть (заменить: ?replace=1)")));
    }
    if h.get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .is_some_and(|n| n > UPLOAD_MAX)
    {
        return Err(Fail::new(StatusCode::PAYLOAD_TOO_LARGE, format!("файл больше {} МБ", UPLOAD_MAX >> 20)));
    }
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let tmp = Upload(
        sound_path(
            &app,
            &format!(".upload-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)),
        ),
        false,
    );
    let ioe = |e: std::io::Error| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, format!("запись: {e}"));
    let mut f =
        tokio::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&tmp.0).await.map_err(ioe)?;
    let mut got = 0u64;
    let mut s = body.into_data_stream();
    while let Some(chunk) = s.next().await {
        let chunk = chunk.map_err(|e| bad(format!("загрузка оборвалась: {e}")))?;
        got += chunk.len() as u64;
        if got > UPLOAD_MAX {
            return Err(Fail::new(StatusCode::PAYLOAD_TOO_LARGE, format!("файл больше {} МБ", UPLOAD_MAX >> 20)));
        }
        f.write_all(&chunk).await.map_err(ioe)?;
    }
    f.sync_all().await.map_err(ioe)?;
    drop(f);
    if got == 0 {
        return Err(bad("пустой файл"));
    }
    // it must play: decoded now, as the timer will
    let sound: Sound = crate::sounds::decode(&tmp.0, &name)
        .await
        .map_err(|e| Fail::new(StatusCode::UNPROCESSABLE_ENTITY, format!("не звук: {e}")))?;
    if dest.exists() && !replace {
        return Err(Fail::new(StatusCode::CONFLICT, format!("«{name}» уже есть (заменить: ?replace=1)")));
    }
    tokio::fs::rename(&tmp.0, &dest).await.map_err(ioe)?;
    let mut tmp = tmp;
    tmp.1 = true;
    app.lengths.lock().unwrap().remove(&name);
    let secs = sound.pcm.len() as f64 / RATE as f64;
    log(
        "web",
        format!(
            "{}: the sound «{name}» {}: {:.1} s, {:.1} MB",
            peer.ip,
            if replace { "replaced" } else { "uploaded" },
            secs,
            got as f64 / 1048576.0
        ),
    );
    let meta = std::fs::metadata(&dest).ok();
    let item = sound_item(&app, &name, meta.as_ref().map(|m| m.len()), meta.and_then(|m| m.modified().ok())).await;
    events::emit("sounds", sound_list(&app).await);
    Ok((StatusCode::CREATED, Json(item)).into_response())
}

pub(super) async fn rename_sound(State(app): State<App>, Path(name): Path<String>, h: HeaderMap, body: Bytes) -> R {
    let v = json_body(&h, &body)?;
    let from = existing(&app, &name)?;
    let to_name = v["name"].as_str().ok_or_else(|| bad("нужно {\"name\": \"…\"}"))?.to_string();
    crate::sounds::check_name(&to_name).map_err(bad)?;
    if to_name == name {
        let m = std::fs::metadata(&from).ok();
        return ok(sound_item(&app, &name, m.as_ref().map(|m| m.len()), m.and_then(|m| m.modified().ok())).await);
    }
    let to = sound_path(&app, &to_name);
    if to.exists() {
        return Err(Fail::new(StatusCode::CONFLICT, format!("«{to_name}» уже есть")));
    }
    std::fs::rename(&from, &to).map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    app.lengths.lock().unwrap().remove(&name);
    app.shared.settings.sound_replaced(&name, &to_name);
    if app.store.alarm_sound_replaced(&name, Some(&to_name)).unwrap_or(0) > 0 {
        crate::alarms::changed(&app.shared);
    }
    log("web", format!("the sound «{name}» renamed «{to_name}»"));
    let m = std::fs::metadata(&to).ok();
    let item = sound_item(&app, &to_name, m.as_ref().map(|m| m.len()), m.and_then(|m| m.modified().ok())).await;
    events::emit("sounds", sound_list(&app).await);
    ok(item)
}

pub(super) async fn delete_sound(State(app): State<App>, Path(name): Path<String>) -> R {
    let p = existing(&app, &name)?;
    std::fs::remove_file(&p).map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    app.lengths.lock().unwrap().remove(&name);
    let reset = app.shared.settings.sound_replaced(&name, CHIME);
    let alarms_reset = app.store.alarm_sound_replaced(&name, None).unwrap_or(0);
    if alarms_reset > 0 {
        crate::alarms::changed(&app.shared);
    }
    log("web", format!("the sound «{name}» deleted{}", if reset { ": the built-in one rings instead" } else { "" }));
    events::emit("sounds", sound_list(&app).await);
    ok(json!({"deleted": name, "settings": app.shared.settings.all(), "alarms_reset": alarms_reset}))
}

/// The sound as the station plays it, as WAV (48 kHz mono): once — up to PREVIEW_MAX s, faded if cut — or, with
/// `seconds`, looped that long and faded out, as a ring.
pub(super) async fn audio(
    State(app): State<App>,
    Path(name): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> R {
    let seconds = q
        .get("seconds")
        .map(|s| {
            s.parse::<f64>()
                .ok()
                .filter(|s| *s > 0.0 && *s <= crate::ring::MAX)
                .ok_or_else(|| bad(format!("seconds: до {}", crate::ring::MAX)))
        })
        .transpose()?;
    let sound = crate::sounds::load(&app.shared.sounds_dir, &name).await.map_err(|e| {
        if e.contains("нет такого файла") {
            Fail::new(StatusCode::NOT_FOUND, e)
        } else {
            Fail::new(StatusCode::UNPROCESSABLE_ENTITY, e)
        }
    })?;
    let pcm = match seconds {
        Some(s) => crate::ring::render(&sound, s),
        None => once(&sound, PREVIEW_MAX),
    };
    Ok(([(header::CONTENT_TYPE, "audio/wav")], crate::services::wav(&pcm, RATE)).into_response())
}

/// The sound once, at most `max` s; cut — the last 0.3 s faded.
pub(super) fn once(s: &Sound, max: f64) -> Vec<u8> {
    let n = s.pcm.len().min((max * RATE as f64) as usize);
    let fade = if n < s.pcm.len() { (0.3 * RATE as f64) as usize } else { 0 };
    s.pcm[..n]
        .iter()
        .enumerate()
        .flat_map(|(i, &v)| {
            let left = n - i;
            let g = if left <= fade { left as f32 / fade as f32 } else { 1.0 };
            ((v as f32 * g) as i16).to_le_bytes()
        })
        .collect()
}
