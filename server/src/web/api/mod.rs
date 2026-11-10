//! The web UI's API: JSON in and out, errors as {"error": "…"} (a text the UI may show) with a fitting status.
//!   GET    /api/info                       the server's version, the time, the limits
//!   GET    /api/events                     server-sent events: `hello` (everything now), then what changes (events.rs)
//!   GET    /api/devices                    every device seen: online, panel, volume, music, ring, timers
//!   GET    /api/devices/{id}               one of them
//!   PATCH  /api/devices/{id}               {"name"}: what the UI calls it
//!   POST   /api/devices/{id}/volume        {"level": 0…10 (halves)} or {"raw": 0…255}
//!   POST   /api/devices/{id}/music         {"action": pause | resume | next | previous | restart | stop | repeat | repeat_off}
//!                                          or {"action": "seek", "to": s} / {"action": "seek", "by": ±s}
//!   POST   /api/devices/{id}/ring          {"sound"?, "seconds"?}: a sound tried on the station (as a timer rings)
//!   DELETE /api/devices/{id}/ring          the ring stopped
//!   GET    /api/exchanges?device=&before=&limit=   the chats, newest first: {"items", "more"}
//!   GET    /api/settings · PATCH /api/settings     {"timer": {"sound", "ring"}}; a change is all or nothing
//!   GET    /api/sounds                     the built-in and the user's: name, bytes, seconds, or why it cannot play
//!   PUT    /api/sounds/{name}[?replace=1]  the file as the body (anything ffmpeg reads): checked by decoding it
//!   PATCH  /api/sounds/{name}              {"name"}: renamed (the timer's setting follows)
//!   DELETE /api/sounds/{name}              deleted (the timer's, if it was: the chime)
//!   GET    /api/sounds/{name}/audio[?seconds=]   WAV as the station plays it: once (up to 60 s), or looped and faded
//!   GET    /api/timers · DELETE /api/timers/{id} the pending timers; one cancelled
//!   GET    /api/alarms · POST /api/alarms · PATCH /api/alarms/{id} · DELETE /api/alarms/{id}   alarms (alarms.rs)
//!   GET    /api/stats?device=&days=30      the exchanges per day, by the hour, per device (analytics)
//!   GET    /api/music?device=&days=30      the music listened to: per day, the totals, songs, the most skipped, artists
//!   GET    /api/providers · POST /api/providers · PATCH|DELETE /api/providers/{id}   who answers, hears, speaks
//!   PATCH  /api/providers/in-use           {"llm"|"asr"|"tts": id}: the one of a kind every station uses
//!   POST   /api/providers/{id}/check       a model answers, a voice speaks (WAV), recognition hears a phrase
//!   POST   /api/providers/models           {url, key? | id}: a service's models (with prices, if it says them)
//!   GET    /api/wakes?device=&label=&before=&limit=   every wake recorded (wakes.rs), newest first, and the counts
//!   GET    /api/wakes/{id}/audio · PATCH /api/wakes/{id} {"label": "me"|"not"|null} · DELETE /api/wakes/{id}
mod alarms;
mod devices;
mod providers;
mod sounds;
#[cfg(test)]
mod tests;
mod wakes;

pub use devices::{device, watch_music};

use super::{auth, json_body, App, Client, Fail};
use crate::events;
use crate::session::{log, Shared};
use crate::text::now_s;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// An upload: a ten-minute WAV of CD quality is 106 MB
pub const UPLOAD_MAX: u64 = 200 << 20;
const PAGE: usize = 50;
const PAGE_MAX: usize = 200;

type R = Result<Response, Fail>;
pub fn routes() -> Router<App> {
    Router::new()
        .route("/api/auth", get(auth::state))
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/password", post(auth::change_password))
        .route("/api/users", get(auth::users).post(auth::add_user))
        .route("/api/users/{id}", axum::routing::delete(auth::remove_user))
        .route(super::oidc::START, get(super::oidc::start))
        .route(super::oidc::CALLBACK, get(super::oidc::callback))
        .route("/api/info", get(info))
        .route("/api/events", get(stream))
        .route("/api/devices", get(devices::devices))
        .route("/api/devices/{id}", get(devices::one_device).patch(devices::rename_device))
        .route("/api/devices/{id}/volume", post(devices::set_volume))
        .route("/api/devices/{id}/music", post(devices::music))
        .route("/api/devices/{id}/ring", post(devices::try_ring).delete(devices::stop_ring))
        .route("/api/exchanges", get(exchanges))
        .route("/api/settings", get(settings).patch(change_settings))
        .route("/api/sounds", get(sounds::list_sounds))
        .route("/api/sounds/{name}", put(sounds::upload).patch(sounds::rename_sound).delete(sounds::delete_sound))
        .route("/api/sounds/{name}/audio", get(sounds::audio))
        .route("/api/timers", get(timers))
        .route("/api/timers/{id}", axum::routing::delete(cancel_timer))
        .route("/api/alarms", get(alarms::list_alarms).post(alarms::add_alarm))
        .route("/api/alarms/{id}", axum::routing::patch(alarms::change_alarm).delete(alarms::delete_alarm))
        .route("/api/stats", get(stats))
        .route("/api/music", get(music_stats))
        .route("/api/providers", get(providers::list_providers).post(providers::add_provider))
        .route("/api/providers/in-use", axum::routing::patch(providers::choose_providers))
        .route("/api/providers/models", post(providers::models))
        .route(
            "/api/providers/{id}",
            axum::routing::patch(providers::change_provider).delete(providers::delete_provider),
        )
        .route("/api/providers/{id}/check", post(providers::check_provider))
        .route("/api/wakes", get(wakes::list_wakes))
        .route("/api/wakes/{id}", axum::routing::patch(wakes::mark_wake).delete(wakes::delete_wake))
        .route("/api/wakes/{id}/audio", get(wakes::wake_audio))
}

fn ok(v: Value) -> R {
    Ok(Json(v).into_response())
}

fn bad(msg: impl Into<String>) -> Fail {
    Fail::new(StatusCode::BAD_REQUEST, msg)
}

// ---------------------------------------------------------------- the server, live

async fn info() -> R {
    ok(
        json!({"name": "Ranunculus", "version": ranunculus_proto::VERSION, "proto": ranunculus_proto::PROTO, "now": now_s(), "tz": crate::text::tz().name(),
              "limits": {"ring": {"min": crate::ring::MIN, "max": crate::ring::MAX}, "upload_bytes": UPLOAD_MAX, "sound_name": crate::sounds::NAME_MAX,
                         "sound_seconds": 600, "preview_seconds": sounds::PREVIEW_MAX, "page": PAGE_MAX, "ramp": crate::settings::RAMP_MAX,
                         "preferences": crate::settings::PREFS_MAX, "alarm_label": alarms::LABEL_MAX}}),
    )
}

fn everything(app: &App) -> Value {
    json!({"version": ranunculus_proto::VERSION, "now": now_s(), "devices": devices::all_devices(&app.shared), "settings": app.shared.settings.all(),
           "timers": app.shared.timers.json(), "alarms": crate::alarms::list(&app.shared),
           "providers": app.shared.providers.json()})
}

/// `hello` first (all there is now: devices, settings, timers…), then every change; a comment every 15 s keeps proxies
/// from closing it. Its session is looked at every 30 s: ended («выйти везде», a new password, its time out) — the
/// stream ends (the page then finds it is signed out); open — it counts as used (a page left open on a wall).
async fn stream(
    State(app): State<App>,
    Extension(a): Extension<auth::Authed>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let rx = events::subscribe(); // before the snapshot: nothing between them is lost
    let store = app.store.clone();
    let ended = async move {
        loop {
            tokio::time::sleep(SESSION_CHECK).await;
            if !matches!(store.session(&a.session, now_s()), Ok(Some(_))) {
                break;
            }
        }
    };
    let s = live(rx, move || everything(&app))
        .map(|(kind, data)| Ok(Event::default().event(kind).data(&*data)))
        .take_until(ended);
    Sse::new(s).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

const SESSION_CHECK: Duration = Duration::from_secs(30);

/// What a listener is sent: `hello` with `snapshot()` (taken after subscribing: nothing between is lost), then the
/// events. One that fell behind (more than the bus keeps) is subscribed anew and sent a fresh `hello`: fetching
/// everything again with several GETs, a client would overwrite what came meanwhile with their older answers.
fn live(
    rx: broadcast::Receiver<events::Ev>,
    snapshot: impl Fn() -> Value + Send + 'static,
) -> impl futures_util::Stream<Item = (&'static str, Arc<str>)> {
    let hello: Arc<str> = snapshot().to_string().into();
    let rest = futures_util::stream::unfold((rx, snapshot), |(mut rx, snapshot)| async move {
        use tokio::sync::broadcast::error::RecvError;
        match rx.recv().await {
            Ok(e) => Some(((e.kind, e.data), (rx, snapshot))),
            Err(RecvError::Lagged(n)) => {
                rx = rx.resubscribe(); // from now on: the snapshot below has all before
                log("web", format!("a listener of the events fell {n} behind: sent everything anew"));
                let hello: Arc<str> = snapshot().to_string().into();
                Some((("hello", hello), (rx, snapshot)))
            }
            Err(RecvError::Closed) => None,
        }
    });
    futures_util::stream::once(async move { ("hello", hello) }).chain(rest)
}

fn opt_num(x: &Value, ok: impl Fn(f64) -> bool, why: &str) -> Result<Option<f64>, Fail> {
    match x {
        Value::Null => Ok(None),
        v => v.as_f64().filter(|n| n.is_finite() && ok(*n)).map(Some).ok_or_else(|| bad(why)),
    }
}

// ---------------------------------------------------------------- the chats

async fn exchanges(State(app): State<App>, Query(q): Query<HashMap<String, String>>) -> R {
    let device = q.get("device").filter(|d| !d.is_empty());
    let before = q.get("before").map(|b| b.parse::<i64>().map_err(|_| bad("before: номер обмена"))).transpose()?;
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
        .exchanges(device.map(|d| d.as_str()), before, limit + 1)
        .map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let more = items.len() > limit;
    items.truncate(limit);
    ok(json!({"items": items, "more": more}))
}

// ---------------------------------------------------------------- settings

async fn settings(State(app): State<App>) -> R {
    ok(app.shared.settings.all())
}

async fn change_settings(State(app): State<App>, h: HeaderMap, body: Bytes) -> R {
    let v = json_body(&h, &body)?;
    match app.shared.settings.change(&v).await {
        Ok(all) => {
            if app.shared.brain.set_profile(app.shared.settings.profile()) && app.prewarm {
                // the prompt changed: back into the cache of a model at home while nobody waits
                if let Some(llm) = app.shared.providers.llm() {
                    let b = app.shared.brain.clone();
                    tokio::spawn(async move {
                        if let Some(s) = b.prewarm(&llm).await {
                            log("server", format!("   (the prompt with the new settings warmed up in {s:.1} s)"));
                        }
                    });
                }
            }
            ok(all)
        }
        Err(e) => Err(Fail::new(StatusCode::UNPROCESSABLE_ENTITY, e.0)),
    }
}

// ---------------------------------------------------------------- timers

async fn timers(State(app): State<App>) -> R {
    ok(app.shared.timers.json())
}

async fn cancel_timer(State(app): State<App>, Path(id): Path<u64>) -> R {
    let t = app.shared.timers.all().into_iter().find(|t| t.id == id);
    let Some(t) = t else {
        return Err(Fail::new(StatusCode::NOT_FOUND, "нет такого таймера (или он уже звенит)"));
    };
    app.shared.timers.remove(id);
    log(&t.device, "   (web: a timer cancelled)");
    ok(json!({"cancelled": id}))
}

// ---------------------------------------------------------------- analytics

/// The last `days` days (1 … 366, the home's), a device's or everyone's: per day (requests, cut, the ring stopped, not
/// heard, average timings, tools), by the hour of the day, per device.
async fn stats(State(app): State<App>, Query(q): Query<HashMap<String, String>>) -> R {
    let device = q.get("device").filter(|d| !d.is_empty()).map(|d| d.as_str());
    let days = q
        .get("days")
        .map(|d| d.parse::<u32>().ok().filter(|d| (1..=366).contains(d)).ok_or_else(|| bad("days: 1…366")))
        .transpose()?
        .unwrap_or(30);
    let since = crate::text::day_start(days - 1);
    let e = |e: String| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e);
    ok(
        json!({"since": since, "days": app.store.stats(device, since).map_err(e)?, "hours": app.store.hours(device, since).map_err(e)?,
              "devices": app.store.by_device(since).map_err(e)?.into_iter().map(|(d, n)| json!({"device": d, "requests": n})).collect::<Vec<_>>()}),
    )
}

async fn music_stats(State(app): State<App>, Query(q): Query<HashMap<String, String>>) -> R {
    let device = q.get("device").filter(|d| !d.is_empty()).map(|d| d.as_str());
    let days = q
        .get("days")
        .map(|d| d.parse::<u32>().ok().filter(|d| (1..=366).contains(d)).ok_or_else(|| bad("days: 1…366")))
        .transpose()?
        .unwrap_or(30);
    let since = crate::text::day_start(days - 1);
    let mut v = app.store.music_stats(device, since).map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    v["since"] = json!(since);
    ok(v)
}
