//! The API over a server.db of its own.
use super::devices::*;
use super::providers::*;
use super::sounds::*;
use super::*;
use crate::brain::Brain;
use crate::fillers::Fillers;
use crate::providers::Providers;
use crate::session::Shared;
use crate::settings::Settings;
use crate::sounds::{Sound, RATE};
use crate::store::{ProviderRow, Store};
use crate::timers::Timers;

/// The web's App on a server.db of its own (a temporary folder): the providers made as on the first start.
fn test_app(name: &str) -> (App, std::path::PathBuf) {
    let d = std::env::temp_dir().join(format!("server-webapi-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let store = Store::open(&d.join("server.db").to_string_lossy()).unwrap();
    let dir = d.to_string_lossy().to_string();
    let providers = Providers::load(store.clone(), &crate::config::Services::default());
    let settings = Settings::load(store.clone(), &dir);
    let shared = Arc::new(Shared {
        brain: Arc::new(Brain::new(None, "").unwrap()),
        fillers: Fillers::new(&dir),
        pages: String::new(),
        search: String::new(),
        music: None,
        stations: Default::default(),
        players: Default::default(),
        active: Default::default(),
        timers: Timers::load(None),
        sounds_dir: dir.clone(),
        settings,
        store: store.clone(),
        wakes_dir: dir,
        providers,
        rings: Default::default(),
        dialogs: Default::default(),
        listens: crate::listens::Writer::start(store.clone()),
    });
    (
        App {
            shared,
            store,
            ui: Arc::new(d.clone()),
            prewarm: false,
            lengths: Default::default(),
            auth: Default::default(),
            net: Default::default(),
            origin: None,
            password: true,
            oidc: None,
        },
        d,
    )
}

fn json_h() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    h
}

fn status(r: R) -> u16 {
    match r {
        Ok(r) => r.status().as_u16(),
        Err(f) => f.0.as_u16(),
    }
}

/// The `kind` events told so far whose data `pick` takes (the bus is the process's: other tests emit too).
fn told(rx: &mut broadcast::Receiver<events::Ev>, kind: &str, pick: impl Fn(&Value) -> bool) -> Vec<Value> {
    let mut out = vec![];
    loop {
        match rx.try_recv() {
            Ok(e) if e.kind == kind => {
                let v: Value = serde_json::from_str(&e.data).unwrap();
                if pick(&v) {
                    out.push(v);
                }
            }
            Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => return out,
        }
    }
}

#[tokio::test]
async fn a_device_is_renamed() {
    let (app, d) = test_app("device");
    let id = format!("webapi-{}", std::process::id());
    app.store.seen(&id, 1.0).unwrap();
    let mut rx = events::subscribe();
    let patch = |v: Value| rename_device(State(app.clone()), Path(id.clone()), json_h(), Bytes::from(v.to_string()));
    let mine = |v: &Value| v["id"] == json!(id);
    for bad in [
        json!({"name": null}),
        json!({"name": "x".repeat(61)}),
        json!({"name": "a\u{7}b"}),
        json!({}),
        json!({"volume": 3}),
    ] {
        assert_eq!(status(patch(bad.clone()).await), 400, "{bad}");
    }
    let row = |app: &App| app.store.devices().unwrap().into_iter().find(|x| x.id == id).unwrap();
    assert_eq!(row(&app).name, "");
    assert!(told(&mut rx, "device", mine).is_empty());
    // named, told once; a change from the web is not «seen»
    assert_eq!(status(patch(json!({"name": "  Кухня "})).await), 200);
    assert_eq!((row(&app).name.as_str(), row(&app).first_seen, row(&app).last_seen), ("Кухня", 1.0, 1.0));
    let ev = told(&mut rx, "device", mine);
    assert!(ev.len() == 1 && ev[0]["name"] == json!("Кухня"), "{ev:?}");
    assert_eq!(
        status(
            rename_device(State(app.clone()), Path("nowhere".into()), json_h(), Bytes::from(r#"{"name": "x"}"#)).await
        ),
        404
    );
    let _ = std::fs::remove_dir_all(&d);
}

fn llm_row(name: &str) -> ProviderRow {
    ProviderRow {
        id: 0,
        kind: "llm".into(),
        name: name.into(),
        url: "http://127.0.0.1:9/v1".into(),
        model: "m".into(),
        key: String::new(),
        voice: String::new(),
        speed: None,
        extra: json!({}),
        updated: 1.0,
    }
}

#[tokio::test]
async fn the_choice_is_all_or_nothing() {
    let (app, d) = test_app("choice");
    let ps = app.shared.providers.clone();
    let first = app.store.save_provider(&llm_row("Выбор-1")).unwrap();
    let other = app.store.save_provider(&llm_row("Выбор-2")).unwrap();
    ps.changed();
    let asr = ps.list().into_iter().find(|p| p.kind == "asr").unwrap().id;
    let before = ps.json()["in_use"].clone();
    assert_eq!(before["llm"], json!(first));
    let mut rx = events::subscribe();
    let set = |v: Value| choose_providers(State(app.clone()), json_h(), Bytes::from(v.to_string()));
    for (bad, code) in [
        (json!({"llm": other, "asr": 999}), 404),
        (json!({"llm": other, "asr": other}), 400),
        (json!({"llm": other, "tts": "1"}), 400),
        (json!({"llm": other, "oven": asr}), 400),
    ] {
        assert_eq!(status(set(bad.clone()).await), code, "{bad}");
        assert_eq!(ps.json()["in_use"], before, "{bad}: nothing kept");
    }
    // all of it, and the UI told
    assert_eq!(status(set(json!({"llm": other, "asr": asr})).await), 200);
    assert_eq!((ps.json()["in_use"]["llm"].as_i64(), ps.json()["in_use"]["asr"].as_i64()), (Some(other), Some(asr)));
    // (this test's own event: the bus is the process's, and other tests' providers have the same numbers)
    let mine = |v: &Value| {
        v["in_use"]["llm"] == json!(other)
            && v["items"].as_array().is_some_and(|l| l.iter().any(|p| p["name"] == json!("Выбор-2")))
    };
    assert_eq!(told(&mut rx, "providers", mine).len(), 1);
    let _ = std::fs::remove_dir_all(&d);
}

#[tokio::test]
async fn what_is_in_use_is_not_deleted_from_under_the_stations() {
    let (app, d) = test_app("delete");
    let ps = app.shared.providers.clone();
    let del = |id: i64| delete_provider(State(app.clone()), Path(id));
    // the only recognition and the only voice stay
    for kind in ["asr", "tts"] {
        let id = ps.list().into_iter().find(|p| p.kind == kind).unwrap().id;
        assert_eq!(status(del(id).await), 409, "{kind}");
    }
    let first = app.store.save_provider(&llm_row("Первая")).unwrap();
    let other = app.store.save_provider(&llm_row("Другая")).unwrap();
    ps.changed();
    // in use while another could take its place: refused; the other one goes
    assert_eq!((status(del(first).await), status(del(other).await)), (409, 200));
    // the only model may go: the stations say there is none
    assert_eq!(status(del(first).await), 200);
    assert!(ps.llm().is_none());
    assert_eq!(status(del(first).await), 404);
    let _ = std::fs::remove_dir_all(&d);
}

#[tokio::test]
async fn a_listener_behind_is_said_hello_again() {
    let (tx, rx) = broadcast::channel(4);
    let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let snap = {
        let n = n.clone();
        move || json!({"snapshot": n.fetch_add(1, std::sync::atomic::Ordering::SeqCst)})
    };
    let mut s = Box::pin(live(rx, snap));
    let ev = |i: usize| events::Ev { kind: "device", data: format!("{{\"n\":{i}}}").into() };
    let said = |x: Option<(&'static str, Arc<str>)>| x.map(|(k, d)| format!("{k} {d}"));
    assert_eq!(said(s.next().await).as_deref(), Some(r#"hello {"snapshot":0}"#));
    tx.send(ev(0)).unwrap();
    assert_eq!(said(s.next().await).as_deref(), Some(r#"device {"n":0}"#));
    // it does not read while ten come; the bus keeps four
    for i in 1..=10 {
        tx.send(ev(i)).unwrap();
    }
    // a new snapshot, taken now — not the oldest four kept, with a hole before them
    assert_eq!(said(s.next().await).as_deref(), Some(r#"hello {"snapshot":1}"#));
    tx.send(ev(11)).unwrap();
    assert_eq!(said(s.next().await).as_deref(), Some(r#"device {"n":11}"#), "what comes after it, nothing from before");
    drop(tx);
    assert_eq!(said(s.next().await), None);
}

#[test]
fn a_keys_site() {
    assert_eq!(site("https://openrouter.ai/api/v1"), "https://openrouter.ai");
    assert_eq!(site("https://OpenRouter.ai/v1"), site("https://openrouter.ai/api/v1"));
    assert_ne!(site("https://openrouter.ai.evil.lan/v1"), site("https://openrouter.ai/api/v1"));
    assert_ne!(site("http://openrouter.ai/v1"), site("https://openrouter.ai/v1"), "not over plain http");
    assert_ne!(site("http://127.0.0.1:52521/v1"), site("http://127.0.0.1:9999/v1"));
}

#[test]
fn only_models_that_can_answer_are_offered() {
    // OpenRouter's list, and a local server's, which says nothing but the names
    let list = json!({"data": [
        {"id": "a/chat", "name": "A", "pricing": {"prompt": "0.0000003", "completion": "0.0000012"}, "supported_parameters": ["tools"],
         "endpoints": ["/api/v1/chat/completions", "/api/v1/responses"]},
        {"id": "a/chat:batch", "name": "A (batch)", "pricing": {"prompt": "0.0000001", "completion": "0.0000003"}, "endpoints": ["/api/v1/batches"]},
        {"id": "local"},
    ]});
    let items = offered(&list);
    assert_eq!(items.iter().map(|m| m["id"].as_str().unwrap()).collect::<Vec<_>>(), ["a/chat", "local"]);
    assert_eq!(
        (items[0]["tools"].clone(), items[1]["tools"].clone(), items[1]["name"].clone()),
        (json!(true), Value::Null, json!("local"))
    );
    assert!((items[0]["in"].as_f64().unwrap() - 0.3).abs() < 1e-9 && items[1]["in"].is_null());
}

#[test]
fn once_cut_and_faded() {
    let s = Sound { name: "x".into(), pcm: Arc::new(vec![1000i16; RATE as usize * 2]) };
    let whole = once(&s, 5.0);
    assert_eq!(whole.len(), RATE as usize * 4);
    assert!(whole.chunks(2).all(|c| i16::from_le_bytes([c[0], c[1]]) == 1000), "not cut: not faded");
    let cut = once(&s, 1.0);
    assert_eq!(cut.len(), RATE as usize * 2);
    let last = i16::from_le_bytes([cut[cut.len() - 2], cut[cut.len() - 1]]);
    let before = i16::from_le_bytes([cut[cut.len() - 2 - 2 * 14400], cut[cut.len() - 1 - 2 * 14400]]);
    assert!(last.abs() < 5 && before == 1000, "{last} {before}");
}
