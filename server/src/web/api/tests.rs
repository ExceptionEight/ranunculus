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
use crate::store::Store;
use crate::timers::Timers;

/// The web's App on a server.db of its own (a temporary folder): the connection to OpenRouter made as on the first
/// start, without a key.
fn test_app(name: &str) -> (App, std::path::PathBuf) {
    let d = std::env::temp_dir().join(format!("server-webapi-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let store = Store::open(&d.join("server.db").to_string_lossy()).unwrap();
    let dir = d.to_string_lossy().to_string();
    let providers = Providers::load(store.clone(), None);
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

/// A service at home, as a JSON body for a new connection.
fn home(name: &str) -> Value {
    json!({"name": name, "url": "http://127.0.0.1:1/v1"})
}

async fn body(r: R) -> Value {
    let r = r.unwrap_or_else(|f| f.into_response());
    serde_json::from_slice(&axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap()).unwrap()
}

#[tokio::test]
async fn a_key_once_then_a_model_for_every_role() {
    let (app, d) = test_app("roles");
    let ps = app.shared.providers.clone();
    let or = ps.connections()[0].id;
    let patch =
        |id: i64, v: Value| change_connection(State(app.clone()), Path(id), json_h(), Bytes::from(v.to_string()));
    let put = |role: &str, v: Value| {
        set_role(State(app.clone()), Path(role.to_string()), json_h(), Bytes::from(v.to_string()))
    };
    // nothing chosen, nothing to check
    assert_eq!(status(check_role(State(app.clone()), Path("llm".into()), json_h(), Bytes::new()).await), 404);
    assert_eq!(status(put("llm", json!({"connection": 999, "model": "m"})).await), 404);
    let mut rx = events::subscribe();
    // the key of OpenRouter: every role gets its model, and the UI is told (this test's own event: the bus is the
    // process's)
    assert_eq!(status(patch(or, json!({"key": "sk-or-v1-roles-test-wxyz"})).await), 200);
    let mine = |v: &Value| v["connections"][0]["key_tail"] == json!("wxyz");
    let told_now = told(&mut rx, "providers", mine);
    assert_eq!(told_now.len(), 1);
    assert!(
        crate::providers::ROLES.iter().all(|r| told_now[0]["roles"][*r]["connection"] == json!(or)),
        "{}",
        told_now[0]
    );
    assert_eq!(ps.llm().unwrap().model, "deepseek/deepseek-v4.1-flash");
    // a role changed: a field that is not its own, a speed out of range — refused, nothing kept
    let before = ps.json();
    for (role, v, code) in [
        ("oven", json!({"connection": or, "model": "m"}), 404),
        ("llm", json!({"connection": or, "model": "m", "voice": "sarah"}), 400),
        ("tts", json!({"connection": or, "speed": 9}), 400),
        ("asr", json!({"connection": "1"}), 400),
        ("asr", json!({"connection": or, "extra": []}), 400),
    ] {
        assert_eq!(status(put(role, v.clone()).await), code, "{role} {v}");
        assert_eq!(ps.json(), before, "{role} {v}: nothing kept");
    }
    let j = body(
        put("tts", json!({"connection": or, "model": "x-ai/grok-voice-tts-1.0", "voice": "eve", "speed": 1.2})).await,
    )
    .await;
    assert_eq!(
        j["roles"]["tts"],
        json!({"connection": or, "model": "x-ai/grok-voice-tts-1.0", "voice": "eve", "speed": 1.2, "extra": {}, "cloud": true})
    );
    // only a part named: the rest stays
    let j = body(put("tts", json!({"voice": "ara"})).await).await;
    assert_eq!(
        (j["roles"]["tts"]["voice"].clone(), j["roles"]["tts"]["model"].clone()),
        (json!("ara"), json!("x-ai/grok-voice-tts-1.0"))
    );
    // another model: the last one's voice and request fields stay with it; DEFAULTS' model gets its own back
    let j = body(put("tts", json!({"model": "openai/gpt-4o-mini-tts"})).await).await;
    assert_eq!((j["roles"]["tts"]["voice"].clone(), j["roles"]["tts"]["speed"].clone()), (json!(""), json!(1.2)));
    let j = body(put("tts", json!({"model": "x-ai/grok-voice-tts-1.0", "voice": "ara"})).await).await;
    assert_eq!(j["roles"]["tts"]["voice"], json!("ara"));
    assert_eq!(ps.json()["roles"]["embeddings"]["extra"], json!({"dimensions": 1024}));
    let j = body(put("embeddings", json!({"model": "openai/text-embedding-3-small"})).await).await;
    assert_eq!(j["roles"]["embeddings"]["extra"], json!({}));
    let j = body(put("embeddings", json!({"model": "google/gemini-embedding-2"})).await).await;
    assert_eq!(j["roles"]["embeddings"]["extra"], json!({"dimensions": 1024}));
    // the key stays with its site: another address takes a key of its own
    assert_eq!(status(patch(or, json!({"url": "https://example.org/v1"})).await), 400);
    assert_eq!(status(patch(or, json!({"url": "https://openrouter.ai/api/v2"})).await), 200);
    assert!(!ps.json().to_string().contains("sk-or-v1"), "the key never comes back");
    let _ = std::fs::remove_dir_all(&d);
}

#[tokio::test]
async fn a_connection_in_use_stays_and_only_the_memory_is_given_up() {
    let (app, d) = test_app("delete");
    let ps = app.shared.providers.clone();
    let del = |id: i64| delete_connection(State(app.clone()), Path(id));
    let r = add_connection(State(app.clone()), json_h(), Bytes::from(home("Дома").to_string())).await;
    assert_eq!(status(r), 201);
    assert_eq!(
        status(
            add_connection(
                State(app.clone()),
                json_h(),
                Bytes::from(json!({"name": "x", "url": "ftp://x"}).to_string())
            )
            .await
        ),
        400
    );
    let home_id = ps.connections().into_iter().find(|c| c.name == "Дома").unwrap().id;
    // a service at home gets no models by itself, key or no key: they are the user's to name
    assert!(ps.roles().is_empty());
    for role in ["llm", "embeddings"] {
        let r = set_role(
            State(app.clone()),
            Path(role.into()),
            json_h(),
            Bytes::from(json!({"connection": home_id, "model": "m"}).to_string()),
        );
        assert_eq!(status(r.await), 200);
    }
    assert!(!ps.llm().unwrap().cloud());
    assert_eq!(status(del(home_id).await), 409, "its roles would go silent");
    let clear = |role: &str| clear_role(State(app.clone()), Path(role.to_string()));
    assert_eq!(
        (status(clear("llm").await), status(clear("embeddings").await), status(clear("oven").await)),
        (400, 200, 404)
    );
    assert_eq!(status(del(home_id).await), 409, "the answers still use it");
    let or = ps.connections()[0].id;
    let r =
        set_role(State(app.clone()), Path("llm".into()), json_h(), Bytes::from(json!({"connection": or}).to_string()));
    assert_eq!(status(r.await), 200);
    assert_eq!((status(del(home_id).await), status(del(home_id).await)), (200, 404));
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
fn only_models_that_do_the_job_are_offered() {
    // OpenRouter's list, and a local server's, which says nothing but the names
    let list = json!({"data": [
        {"id": "b/dear", "name": "B", "pricing": {"prompt": "0.000003", "completion": "0.000012"}, "endpoints": ["/api/v1/chat/completions"]},
        {"id": "a/chat", "name": "A", "pricing": {"prompt": "0.0000003", "completion": "0.0000012"}, "supported_parameters": ["tools"],
         "endpoints": ["/api/v1/chat/completions", "/api/v1/responses"]},
        {"id": "a/chat:batch", "name": "A (batch)", "pricing": {"prompt": "0.0000001", "completion": "0.0000003"}, "endpoints": ["/api/v1/batches"]},
        {"id": "deepseek/deepseek-v4.1-flash", "pricing": {"prompt": "0.00001", "completion": "0.00001"}, "endpoints": ["/api/v1/chat/completions"]},
        {"id": "x-ai/grok-voice-tts-1.0", "pricing": {"prompt": "0.000015", "completion": "0"}, "supported_voices": ["eve", "ara"],
         "endpoints": ["/api/v1/audio/speech"]},
        {"id": "local"},
    ]});
    let ids = |items: &[Value]| items.iter().map(|m| m["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    // the answers: the cheapest first, the default model among them like any other; what is not reached by a chat
    // request is left out
    let items = offered(&list, "llm");
    assert_eq!(ids(&items), ["a/chat", "b/dear", "deepseek/deepseek-v4.1-flash", "local"]);
    assert_eq!(
        (items[0]["tools"].clone(), items[3]["tools"].clone(), items[3]["name"].clone()),
        (json!(true), Value::Null, json!("local"))
    );
    assert!((items[0]["in"].as_f64().unwrap() - 0.3).abs() < 1e-9 && items[3]["in"].is_null());
    assert!(items.iter().all(|m| m.get("ours").is_none()), "nothing is put forward");
    // the voices: by name, with their voices, and no price (the list does not say what it is for)
    let items = offered(&list, "tts");
    assert_eq!(ids(&items), ["local", "x-ai/grok-voice-tts-1.0"]);
    assert_eq!((items[1]["voices"].clone(), items[1].get("in")), (json!(["eve", "ara"]), None));
    assert_eq!(ids(&offered(&list, "asr")), ["local"]);
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
