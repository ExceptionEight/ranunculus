//! The stations: what the web UI shows of each, its name, volume, player and a sound tried on it.
use super::*;
use crate::ring::{self, Stop};
use crate::session::volume_json;

/// A device as the UI shows it. Also what session.rs sends when one connects or leaves.
pub fn device(shared: &Shared, id: &str) -> Value {
    let f = shared.stations.lock().unwrap().get(id).cloned();
    let stored = shared.store.devices().ok().and_then(|v| v.into_iter().find(|d| d.id == id));
    let player = shared.players.lock().unwrap().get(id).cloned(); // not made here: one exists once the device connected
    let music = player.and_then(|p| p.status());
    let ring = shared.rings.lock().unwrap().get(id).map(|r| r.json());
    json!({"id": id, "name": stored.as_ref().map(|d| d.name.as_str()).unwrap_or(""), "online": f.is_some(),
           "panel": f.as_ref().map(|f| f.panel_state()), "volume": f.as_ref().and_then(|f| f.volume_known()).map(volume_json),
           "music": music, "ring": ring, "timers": shared.timers.pending(id).len(),
           "alarms": shared.store.alarms().map(|v| v.iter().filter(|a| a.device == id && a.enabled).count()).unwrap_or(0),
           "first_seen": stored.as_ref().map(|d| d.first_seen), "last_seen": stored.as_ref().map(|d| d.last_seen),
           "exchanges": shared.store.count(Some(id))})
}

pub(super) fn all_devices(shared: &Shared) -> Value {
    let mut ids: Vec<String> = shared.store.devices().unwrap_or_default().into_iter().map(|d| d.id).collect();
    for id in shared.stations.lock().unwrap().keys() {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    json!(ids.iter().map(|id| device(shared, id)).collect::<Vec<_>>())
}

pub(super) fn known(app: &App, id: &str) -> Result<(), Fail> {
    let seen =
        app.store.devices().map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))?.iter().any(|d| d.id == id);
    if seen || app.shared.stations.lock().unwrap().contains_key(id) {
        Ok(())
    } else {
        Err(Fail::new(StatusCode::NOT_FOUND, format!("нет колонки «{id}»")))
    }
}

pub(super) fn station(app: &App, id: &str) -> Result<crate::session::Station, Fail> {
    known(app, id)?;
    app.shared
        .stations
        .lock()
        .unwrap()
        .get(id)
        .cloned()
        .ok_or_else(|| Fail::new(StatusCode::CONFLICT, format!("колонка «{id}» не подключена")))
}

/// The music on each device, every half second: it moves on by itself (the next track), so it is watched, not told.
/// Told when the track, the state or the repeat change — and when where it is in the track is not where the UI makes it
/// (the UI moves the place on by itself while `live`): a seek, the repeat's next round, a stream that stalled.
pub async fn watch_music(shared: Arc<Shared>) {
    // device → (what was told, without the place; the place then; when)
    let mut last: HashMap<String, (Value, f64, std::time::Instant)> = HashMap::new();
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let players: Vec<_> = shared.players.lock().unwrap().iter().map(|(k, p)| (k.clone(), p.clone())).collect();
        for (id, p) in players {
            let now = p.status();
            let mut rest = now.clone().unwrap_or(Value::Null);
            let pos = rest.as_object_mut().and_then(|o| o.remove("pos")).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let told = last.get(&id);
            let moved = told.is_some_and(|(r, p0, t0)| {
                let expected = if r["live"] == json!(true) { p0 + t0.elapsed().as_secs_f64() } else { *p0 };
                (pos - expected).abs() > 1.5
            });
            if told.map(|(r, _, _)| r) != Some(&rest) || moved {
                if told.is_some() || now.is_some() {
                    events::emit("device", json!({"id": id, "music": now}));
                }
                last.insert(id, (rest, pos, std::time::Instant::now()));
            }
        }
    }
}

pub(super) async fn devices(State(app): State<App>) -> R {
    ok(all_devices(&app.shared))
}

pub(super) async fn one_device(State(app): State<App>, Path(id): Path<String>) -> R {
    known(&app, &id)?;
    ok(device(&app.shared, &id))
}

/// PATCH /api/devices/{id} {"name": "…"}: the station's name in the web UI.
pub(super) async fn rename_device(State(app): State<App>, Path(id): Path<String>, h: HeaderMap, body: Bytes) -> R {
    let v = json_body(&h, &body)?;
    known(&app, &id)?;
    let name = v.get("name").and_then(|n| n.as_str()).ok_or_else(|| bad("нужно {\"name\": \"…\"}"))?.trim().to_string();
    if name.chars().count() > 60 || name.chars().any(|c| c.is_control()) {
        return Err(bad("имя: до 60 знаков, без служебных"));
    }
    let wrote = (|| -> Result<(), String> {
        // connected, but its «seen» did not get written: now (a change from the web is not «seen» otherwise: the times stay)
        if !app.store.devices()?.iter().any(|d| d.id == id) {
            app.store.seen(&id, now_s())?;
        }
        app.store.rename(&id, &name)?;
        Ok(())
    })();
    wrote.map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    log(&id, "   (web: the station is named)");
    let d = device(&app.shared, &id);
    events::emit("device", json!({"id": id, "name": d["name"]}));
    ok(d)
}

pub(super) async fn set_volume(State(app): State<App>, Path(id): Path<String>, h: HeaderMap, body: Bytes) -> R {
    let v = json_body(&h, &body)?;
    let raw = match (v["level"].as_f64(), v["raw"].as_f64()) {
        (Some(l), None) if (0.0..=10.0).contains(&l) && (l * 2.0).fract() == 0.0 => (l * 25.5).round_ties_even() as u8,
        (None, Some(r)) if (0.0..=255.0).contains(&r) && r.fract() == 0.0 => r as u8,
        _ => return Err(bad("нужно {\"level\": 0…10, шаг 0,5} или {\"raw\": 0…255}")),
    };
    let f = station(&app, &id)?;
    let got = f.volume_set(raw).await.map_err(|e| Fail::new(StatusCode::BAD_GATEWAY, e))?;
    log(&id, format!("   (web: the volume {raw} → {got})"));
    ok(json!({"volume": volume_json(got)}))
}

pub(super) async fn music(State(app): State<App>, Path(id): Path<String>, h: HeaderMap, body: Bytes) -> R {
    let v = json_body(&h, &body)?;
    let action = v["action"].as_str().unwrap_or("");
    if !["pause", "resume", "next", "previous", "restart", "stop", "repeat", "repeat_off", "seek"].contains(&action) {
        return Err(bad("action: pause | resume | next | previous | restart | stop | repeat | repeat_off | seek"));
    }
    let (to, by) = (v["to"].as_f64(), v["by"].as_f64());
    if action == "seek" && to.is_none() == by.is_none() {
        return Err(bad("seek: to (секунда трека) или by (на сколько, ±)"));
    }
    known(&app, &id)?;
    let p = app
        .shared
        .players
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| Fail::new(StatusCode::CONFLICT, "музыка здесь не играла"))?;
    let r = if action == "seek" { p.seek(by, to) } else { p.control(action) };
    log(&id, format!("   (web: the player, {action}: {})", if r["ok"] == json!(true) { "done" } else { "refused" }));
    if r["ok"] != json!(true) {
        return Err(Fail::new(StatusCode::CONFLICT, r["error"].as_str().unwrap_or("не вышло")));
    }
    ok(r)
}

pub(super) async fn try_ring(State(app): State<App>, Path(id): Path<String>, h: HeaderMap, body: Bytes) -> R {
    let v = json_body(&h, &body)?;
    let name = match &v["sound"] {
        Value::Null => app.shared.settings.timer().sound,
        Value::String(s) => s.clone(),
        _ => return Err(bad("sound: имя звука")),
    };
    let seconds = match &v["seconds"] {
        Value::Null => 10.0,
        s => s
            .as_f64()
            .filter(|s| (ring::MIN..=ring::MAX).contains(s))
            .ok_or_else(|| bad(format!("seconds: от {} до {}", ring::MIN, ring::MAX)))?,
    };
    station(&app, &id)?;
    let sound = crate::sounds::load(&app.shared.sounds_dir, &name)
        .await
        .map_err(|e| Fail::new(StatusCode::UNPROCESSABLE_ENTITY, e))?;
    let shared = app.shared.clone();
    tokio::spawn(async move {
        ring::ring(&shared, &id, sound, seconds, (ring::Kind::Test, ""), now_s() + 5.0, || {}).await;
    });
    Ok((StatusCode::ACCEPTED, Json(json!({"ok": true, "sound": name, "seconds": seconds}))).into_response())
}

pub(super) async fn stop_ring(State(app): State<App>, Path(id): Path<String>) -> R {
    known(&app, &id)?;
    let stopped = app.shared.stop_ring(&id, Stop::Web);
    ok(json!({"stopped": stopped}))
}
