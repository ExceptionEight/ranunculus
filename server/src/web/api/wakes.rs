//! The wakes recorded (crate::wakes): listed, marked, deleted, listened to.
use super::*;

pub(super) async fn list_wakes(State(app): State<App>, Query(q): Query<HashMap<String, String>>) -> R {
    let device = q.get("device").filter(|d| !d.is_empty()).map(|d| d.as_str());
    let label = q.get("label").filter(|l| !l.is_empty()).map(|l| l.as_str());
    if label.is_some_and(|l| !["me", "not", "none"].contains(&l)) {
        return Err(bad("label: me, not или none"));
    }
    let before = q.get("before").map(|b| b.parse::<i64>().map_err(|_| bad("before: номер"))).transpose()?;
    let limit = q
        .get("limit")
        .map(|l| {
            l.parse::<usize>()
                .ok()
                .filter(|l| (1..=PAGE_MAX).contains(l))
                .ok_or_else(|| bad(format!("limit: 1…{PAGE_MAX}")))
        })
        .transpose()?
        .unwrap_or(PAGE);
    let mut items = app
        .store
        .wakes(device, label, before, limit + 1)
        .map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let more = items.len() > limit;
    items.truncate(limit);
    ok(json!({"items": items, "more": more, "counts": app.store.wake_counts(device)}))
}

pub(super) async fn mark_wake(State(app): State<App>, Path(id): Path<i64>, h: HeaderMap, body: Bytes) -> R {
    let v = json_body(&h, &body)?;
    let label = match &v["label"] {
        Value::Null => None,
        Value::String(l) if l == "me" || l == "not" => Some(l.as_str()),
        _ => return Err(bad("label: \"me\" (это я), \"not\" (не мне) или null")),
    };
    if !app.store.set_wake(id, "label", label).map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))? {
        return Err(Fail::new(StatusCode::NOT_FOUND, "нет такого срабатывания"));
    }
    crate::wakes::changed(&app.store, id);
    ok(app.store.wake(id).unwrap_or(Value::Null))
}

pub(super) async fn delete_wake(State(app): State<App>, Path(id): Path<i64>) -> R {
    let Some(file) = app.store.delete_wake(id).map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))? else {
        return Err(Fail::new(StatusCode::NOT_FOUND, "нет такого срабатывания"));
    };
    let _ = std::fs::remove_file(std::path::Path::new(&app.shared.wakes_dir).join(&file));
    events::emit("wake", json!({"id": id, "deleted": true}));
    ok(json!({"deleted": id}))
}

pub(super) async fn wake_audio(State(app): State<App>, Path(id): Path<i64>) -> R {
    let w = app.store.wake(id).ok_or_else(|| Fail::new(StatusCode::NOT_FOUND, "нет такого срабатывания"))?;
    let file = w["file"].as_str().unwrap_or("");
    if file.is_empty() || file.contains('/') || file.starts_with('.') {
        return Err(Fail::new(StatusCode::NOT_FOUND, "нет записи"));
    }
    let b = tokio::fs::read(std::path::Path::new(&app.shared.wakes_dir).join(file))
        .await
        .map_err(|e| Fail::new(StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(([(header::CONTENT_TYPE, "audio/wav")], b).into_response())
}
