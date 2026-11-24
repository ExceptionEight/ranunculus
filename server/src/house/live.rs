//! The house against a Home Assistant that is a few lines here: a real WebSocket, any token taken, the lists given,
//! the calls kept and answered as a device would (a thermostat takes its temperature at once, a blind starts moving,
//! a number that never says what it was set to), states pushed.
use super::*;
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

#[derive(Default)]
struct Fake {
    /// the calls of services received
    calls: Mutex<Vec<Value>>,
    /// the words are refused (a Home Assistant with no frontend)
    no_words: AtomicBool,
    /// a call is never answered: the socket closes 200 ms after it
    drop_on_call: AtomicBool,
}

const THERMOSTAT: &str = r#"{"hvac_modes": ["off", "heat"], "temperature": 21, "min_temp": 7, "max_temp": 35, "target_temp_step": 0.5,
                             "supported_features": 1, "current_temperature": 20}"#;
const BLIND: &str = r#"{"device_class": "blind", "supported_features": 15, "current_position": 0}"#;

fn state(entity: &str, state: &str, attrs: Value) -> Value {
    json!({"entity_id": entity, "state": state, "attributes": attrs})
}

fn changed(st: Value) -> Message {
    let data = json!({"entity_id": st["entity_id"], "new_state": st});
    Message::Text(
        json!({"id": 1, "type": "event", "event": {"event_type": "state_changed", "data": data}}).to_string().into(),
    )
}

/// → the port it listens on. One connection; `push`: states it then says have changed.
async fn home_assistant(fake: Arc<Fake>, mut push: mpsc::Receiver<Value>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (tcp, _) = l.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let tx = |v: Value| Message::Text(v.to_string().into());
        ws.send(tx(json!({"type": "auth_required", "ha_version": "2026.11.0"}))).await.unwrap();
        let _token = ws.next().await;
        ws.send(tx(json!({"type": "auth_ok", "ha_version": "2026.11.0"}))).await.unwrap();
        let entity = |id: &str, dev: &str| {
            json!({"entity_id": id, "device_id": dev, "platform": "demo", "translation_key": null, "name": null,
                                                    "original_name": null, "entity_category": null, "disabled_by": null, "hidden_by": null})
        };
        let device = |id: &str, name: &str, kind: Value| json!({"id": id, "name": name, "name_by_user": null, "area_id": "hall", "entry_type": kind, "disabled_by": null});
        loop {
            tokio::select! {
                m = ws.next() => {
                    let Some(Ok(Message::Text(t))) = m else { return };
                    let m: Value = serde_json::from_str(t.as_str()).unwrap();
                    let id = m["id"].clone();
                    let ok = |r: Value| json!({"id": id, "type": "result", "success": true, "result": r});
                    let reply = match m["type"].as_str().unwrap() {
                        "ping" => json!({"id": id, "type": "pong"}),
                        "get_config" => ok(json!({"location_name": "Дом", "version": "2026.11.0", "language": "en"})),
                        "frontend/get_translations" if fake.no_words.load(Ordering::SeqCst) => {
                            json!({"id": id, "type": "result", "success": false, "error": {"code": "unknown_command", "message": "Unknown command."}})
                        }
                        "frontend/get_translations" => ok(json!({"resources": {}})),
                        "config/area_registry/list" => ok(json!([{"area_id": "hall", "name": "Гостиная"}])),
                        "config/device_registry/list" => ok(json!([device("c", "Кондиционер", Value::Null), device("b", "Штора", Value::Null),
                                                                   device("n", "Обогреватель", Value::Null), device("sun", "Sun", json!("service"))])),
                        "config/entity_registry/list" => ok(json!([entity("climate.x", "c"), entity("cover.blind", "b"), entity("number.level", "n"),
                                                                   entity("sensor.sun_next", "sun")])),
                        "get_states" => ok(json!([
                            state("climate.x", "heat", serde_json::from_str(THERMOSTAT).unwrap()),
                            state("cover.blind", "closed", serde_json::from_str(BLIND).unwrap()),
                            state("sensor.sun_next", "2026-11-22T07:00:00", json!({})),
                            state("number.level", "1", json!({"min": 1, "max": 3, "step": 1})),
                        ])),
                        "call_service" => {
                            fake.calls.lock().unwrap().push(m.clone());
                            if fake.drop_on_call.load(Ordering::SeqCst) {
                                tokio::time::sleep(Duration::from_millis(200)).await;
                                return;
                            }
                            match m["service"].as_str().unwrap() {
                                "set_temperature" => {
                                    let mut a: Value = serde_json::from_str(THERMOSTAT).unwrap();
                                    a["temperature"] = m["service_data"]["temperature"].clone();
                                    ws.send(changed(state("climate.x", "heat", a))).await.unwrap();
                                }
                                // a blind takes its time: on its way at once, there in a minute
                                "set_cover_position" => {
                                    let mut a: Value = serde_json::from_str(BLIND).unwrap();
                                    a["current_position"] = json!(5);
                                    ws.send(changed(state("cover.blind", "opening", a))).await.unwrap();
                                }
                                _ => {}
                            }
                            ok(json!({"context": {}}))
                        }
                        _ => ok(Value::Null),
                    };
                    ws.send(tx(reply)).await.unwrap();
                }
                Some(st) = push.recv() => ws.send(changed(st)).await.unwrap(),
            }
        }
    });
    port
}

/// A house connected to a Home Assistant of its own, its home read.
async fn connected(fake: Arc<Fake>) -> (Arc<House>, mpsc::Sender<Value>) {
    let (push, pushed) = mpsc::channel(8);
    let port = home_assistant(fake, pushed).await;
    let h = House::load(Store::open(":memory:").unwrap());
    {
        let mut i = h.inner.lock().unwrap();
        (i.address, i.status) = (Some(Address { tls: false, host: "127.0.0.1".into(), port }), Status::Connecting);
        i.token = "a token".into();
    }
    tokio::spawn(h.clone().run());
    for _ in 0..150 {
        if h.json()["status"] == "on" {
            return (h, push);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("not connected: {}", h.json());
}

/// The whole home said to the web UI by this house (the events are every test's: its own are known by its address).
async fn whole_home(h: &House, ev: &mut tokio::sync::broadcast::Receiver<events::Ev>, within: Duration) -> bool {
    let url = h.json()["url"].as_str().unwrap().to_string();
    let said = async {
        loop {
            let e = ev.recv().await.unwrap();
            if e.kind == "house" && e.data.contains(&url) {
                return;
            }
        }
    };
    tokio::time::timeout(within, said).await.is_ok()
}

#[tokio::test]
async fn a_step_up_is_sent_and_what_is_so_already_is_not() {
    let fake = Arc::new(Fake::default());
    let (h, _push) = connected(fake.clone()).await;
    assert_eq!(
        h.json()["devices"].as_array().unwrap().len(),
        3,
        "Home Assistant's own «Sun» is not a device of the home"
    );
    // 21 → 22: not «22, give or take» — sent, and done when the thermostat says 22
    assert_eq!(h.set("climate.x#target", &json!(22), false).await, Ok(json!(22)));
    let sent = fake.calls.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        (&sent[0]["domain"], &sent[0]["service"], &sent[0]["service_data"], &sent[0]["target"]),
        (
            &json!("climate"),
            &json!("set_temperature"),
            &json!({"temperature": 22.0}),
            &json!({"entity_id": "climate.x"})
        )
    );
    // asked again: it is so, nothing more is sent
    assert_eq!(h.set("climate.x#target", &json!("22"), true).await, Ok(json!(22)));
    assert_eq!(fake.calls.lock().unwrap().len(), 1);
    // out of what it takes: refused here, Home Assistant not asked
    assert_eq!(h.set("climate.x#target", &json!(50), false).await.unwrap_err(), "вне диапазона: можно от 7 до 35 °C");
    assert_eq!(h.set("climate.nope#target", &json!(20), false).await.unwrap_err(), NO_CONTROL);
    assert_eq!(fake.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_blind_on_its_way_is_done() {
    let fake = Arc::new(Fake::default());
    let (h, _push) = connected(fake.clone()).await;
    let t = Instant::now();
    assert_eq!(h.set("cover.blind#position", &json!(50), false).await, Ok(json!(5)), "moving: not waited out");
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    assert_eq!(fake.calls.lock().unwrap()[0]["service_data"], json!({"position": 50.0}));
}

#[tokio::test]
async fn a_device_that_does_not_say_so_is_not_said_to_have_done_it() {
    let fake = Arc::new(Fake::default());
    let (h, _push) = connected(fake.clone()).await;
    let t = Instant::now();
    let r = h.set("number.level#value", &json!(2), false).await;
    assert_eq!(r.unwrap_err(), "команда отправлена, но устройство её не подтвердило");
    assert!(t.elapsed() >= CONFIRM && fake.calls.lock().unwrap().len() == 1);
}

#[tokio::test]
async fn what_is_not_shown_changes_without_a_word() {
    let (h, push) = connected(Arc::new(Fake::default())).await;
    tokio::time::sleep(Duration::from_millis(300)).await; // the home's first word is out by now
                                                          // the sun moves every few minutes: no device of the home is made anew for it, the web UI is not sent the home
    let (mut ev, tick) = (events::subscribe(), *h.tick.borrow());
    push.send(state("sensor.sun_next", "2026-11-23T07:00:00", json!({}))).await.unwrap();
    assert!(!whole_home(&h, &mut ev, Duration::from_millis(700)).await);
    assert_eq!(*h.tick.borrow(), tick);
    // a device of the home changes: that device is said, not the whole home
    let mut ev = events::subscribe();
    let mut a: Value = serde_json::from_str(THERMOSTAT).unwrap();
    a["current_temperature"] = json!(23);
    push.send(state("climate.x", "heat", a)).await.unwrap();
    assert!(!whole_home(&h, &mut ev, Duration::from_millis(700)).await);
    assert!(*h.tick.borrow() > tick);
    let now = h.json();
    let d = now["devices"].as_array().unwrap().iter().find(|d| d["name"] == "Кондиционер").unwrap();
    assert!(d["controls"].as_array().unwrap().iter().any(|c| c["id"] == "climate.x#now" && c["value"] == 23));
}

#[tokio::test]
async fn a_home_without_its_words_is_still_a_home() {
    let fake = Arc::new(Fake::default());
    fake.no_words.store(true, Ordering::SeqCst);
    let (h, _push) = connected(fake).await;
    assert_eq!(h.json()["devices"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn a_call_cut_off_is_answered_at_once() {
    let fake = Arc::new(Fake::default());
    let (h, _push) = connected(fake.clone()).await;
    fake.drop_on_call.store(true, Ordering::SeqCst);
    let t = Instant::now();
    assert_eq!(h.set("cover.blind#position", &json!(80), false).await.unwrap_err(), NO_LINK);
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
}

#[tokio::test]
async fn given_up_it_stays_given_up() {
    let (h, push) = connected(Arc::new(Fake::default())).await;
    h.forget().unwrap();
    // what the connection being closed still has to say is not this home's any more
    let _ = push.send(state("climate.x", "off", serde_json::from_str(THERMOSTAT).unwrap())).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let now = h.json();
    assert_eq!(
        (now["status"].as_str(), now["url"].as_str(), now["devices"].as_array().map(Vec::len)),
        (Some("off"), Some(""), Some(0))
    );
    assert_eq!(h.set("climate.x#target", &json!(22), false).await.unwrap_err(), NOT_SET);
}

#[tokio::test]
async fn a_page_does_not_set_the_home_and_the_user_does() {
    let (h, _push) = connected(Arc::new(Fake::default())).await;
    let mut t = crate::tools::Tools::new(crate::tools::Io { house: Some(h.clone()), ..Default::default() }, None);
    // a page was read in the exchange before, and the user's «спасибо» is not about the home
    t.new_request("Спасибо");
    t.web_in_prompt();
    assert!(t.guard("home_set").is_some());
    // their own words name a device — whatever they ask of it
    for said in ["заблокируй кнопки на кондиционере", "штору подними", "выключи всё в гостиной"]
    {
        t.new_request(said);
        t.web_in_prompt();
        assert!(t.guard("home_set").is_none(), "{said}");
    }
    // …and are not the player's while music plays; «выключи» alone is
    assert!(h.about("штору выключи") && !h.about("выключи") && !h.about("пауза"));
    // the request's stamp: «включи …» of a device is not a question of what plays, of music it is
    let music = crate::music::Music::new("http://127.0.0.1:9", "d", None);
    let t = crate::tools::Tools::new(
        crate::tools::Io { house: Some(h.clone()), music: Some(music), ..Default::default() },
        None,
    );
    let nothing_plays = |stamp: Vec<String>| stamp.iter().any(|s| s == "музыка сейчас не играет");
    assert!(!nothing_plays(t.context("включи кондиционер").await) && nothing_plays(t.context("включи музыку").await));
    // a home that is gone has nothing to be about, and its tool is left to say so (not «a page asked for it»)
    h.forget().unwrap();
    assert!(!h.any() && !h.about("штору подними"));
    let mut t = crate::tools::Tools::new(crate::tools::Io { house: Some(h.clone()), ..Default::default() }, None);
    t.new_request("штору подними");
    t.web_in_prompt();
    assert!(t.guard("home_set").is_none());
    assert_eq!(t.run("home_set", &json!({"control": "cover.b#position", "value": "100"})).await["error"], NOT_SET);
}

#[tokio::test]
async fn what_is_put_off_for_a_device() {
    use crate::later::{self, Later, State};
    use crate::tools::{Io, LaterHook, Tools};
    let fake = Arc::new(Fake::default());
    let (h, push) = connected(fake.clone()).await;
    let kept = Later::in_memory();
    let hook = LaterHook { later: kept.clone(), device: "d".into(), start: Arc::new(|_, _| {}) };
    let mut t = Tools::new(Io { house: Some(h.clone()), later: Some(hook), ..Default::default() }, None);
    let home = |control: &str, value: &str, seconds: u64| json!({"tool": "home_set", "args": {"control": control, "value": value}, "seconds": seconds});
    // looked at and not done: whose it is and what it will do; what the control does not take is refused now
    assert_eq!(
        h.planned("climate.x#target", &json!("23"), true).unwrap(),
        Planned {
            device: "c".into(),
            name: "Кондиционер".into(),
            what: "Целевая температура".into(),
            value: "23 °C".into()
        }
    );
    assert_eq!(h.planned("climate.x#target", &json!(50), true).unwrap_err(), "вне диапазона: можно от 7 до 35 °C");
    assert_eq!(h.planned("climate.nope#target", &json!(20), true).unwrap_err(), NO_CONTROL);
    // one request, one device, one time: one thing of two steps — another device, or another time, is another
    t.new_request("через час сделай кондиционеру 23 градуса и подними штору");
    assert_eq!(
        t.run("later", &home("climate.x#target", "23", 3600)).await,
        json!({"ok": true, "id": 1, "when": "через 1 час"})
    );
    assert_eq!(t.run("later", &home("climate.x#mode", "heat", 3600)).await["id"], json!(1));
    assert_eq!(t.run("later", &home("cover.blind#position", "50", 3600)).await["id"], json!(2));
    assert_eq!(t.run("later", &home("climate.x#target", "25", 7200)).await["id"], json!(3));
    assert_eq!(
        t.run("later", &home("climate.x#target", "50", 60)).await["error"],
        "вне диапазона: можно от 7 до 35 °C"
    );
    assert_eq!(
        t.run("later", &home("climate.nope#target", "20", 60)).await["error"],
        "нет такой ручки: возьми control из home_device"
    );
    let p = kept.pending("d");
    assert_eq!(p.len(), 3);
    assert_eq!(p[0].what(), "Кондиционер: Целевая температура: 23 °C, Режим: heat");
    assert_eq!((p[0].target.as_str(), p[1].what().as_str()), ("c", "Штора: Открыто: 50 %"));
    // the next request's calls are its own: not steps of what the one before put off
    t.new_request("и ещё поставь кондиционеру 24 через час");
    assert_eq!(t.run("later", &home("climate.x#target", "24", 3600)).await["id"], json!(4));
    // run: the steps in order, as the tool sets them — what reads so already is not sent
    let mut tools = Tools::new(Io { house: Some(h.clone()), ..Default::default() }, None);
    let due = |id: u64| {
        let mut e = kept.waiting(id).unwrap();
        e.at = crate::text::now_s();
        e
    };
    later::perform(&kept, &due(1), &mut tools).await;
    let done = kept.get(1).unwrap();
    assert_eq!((done.state, done.error.as_str()), (State::Done, ""));
    let calls: Vec<Value> = fake.calls.lock().unwrap().iter().map(|c| c["service_data"].clone()).collect();
    assert_eq!(calls, [json!({"temperature": 23.0})], "the mode is «heat» already");
    // the device away when it is due: the step that fails is named, the rest is not tried
    kept.push(
        3,
        later::Step {
            tool: "home_set".into(),
            args: json!({"control": "climate.x#mode", "value": "off"}),
            what: "Режим".into(),
            value: "Выключено".into(),
        },
    );
    push.send(state("climate.x", "unavailable", serde_json::from_str(THERMOSTAT).unwrap())).await.unwrap();
    for _ in 0..100 {
        if h.json()["devices"].as_array().unwrap().iter().any(|d| d["id"] == "c" && d["online"] == json!(false)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    later::perform(&kept, &due(3), &mut tools).await;
    let failed = kept.get(3).unwrap();
    assert_eq!((failed.state, failed.error.as_str()), (State::Failed, "устройство не в сети (Целевая температура)"));
    assert_eq!(fake.calls.lock().unwrap().len(), 1, "nothing more was sent");
    // …while what is put off for it meanwhile is taken: it is for later
    assert!(t.run("later", &home("climate.x#target", "22", 3600)).await["ok"] == json!(true));
}
