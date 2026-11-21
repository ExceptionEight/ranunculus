//! The home's devices, from Home Assistant: its rooms and devices followed over one connection, shown to the web UI
//! (GET /api/house, the `house` events), set from there and by the voice (the tools home_device and home_set).
//!
//! Home Assistant is where the home is kept: nothing of it is stored here but its address and a token (server.db,
//! the setting «house»; of the token only its last characters go to a browser). The connection is this server's
//! alone — the web UI asks this server, never Home Assistant — and is opened anew when it ends: the devices last
//! seen stay on the page meanwhile, marked as not answering.
//!
//! A control is set in one place (`set`), for the web UI and the voice alike: the value is checked against what the
//! control takes, the service is called, and the control is then waited for to read what was asked. Home Assistant
//! answers «done» to a kettle told 130° and to a lamp that is away; here that is an error, said as one.
pub mod ha;
#[cfg(test)]
mod live;
pub mod model;

use crate::events;
use crate::session::log;
use crate::store::Store;
use crate::text::now_s;
use ha::{Address, Fault};
use model::{Control, Device, Home, Kind, Raw};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tokio::time::Instant;

/// server.db, settings: {"url", "token"}
const KEY: &str = "house";
/// the language the home's words are asked in
const LANG: &str = "ru";
/// how long Home Assistant's answer to a call is waited for: it answers when the device's integration has (a cloud's
/// takes seconds)
const REPLY: Duration = Duration::from_secs(15);
/// …and then the control's new reading
const CONFIRM: Duration = Duration::from_secs(4);
const PING: Duration = Duration::from_secs(30);
/// registries change in bursts (a device added is a dozen entities): they are read once after one
const SETTLE: Duration = Duration::from_millis(600);
/// the longest pause between tries to connect: Home Assistant is in the same network, and back in a minute
const RETRY_MAX: u64 = 10;
/// how long the home's lists are waited for on a new connection
const READY: Duration = Duration::from_secs(30);
/// devices told to the model at once (a name that fits a whole room)
const TOLD: usize = 6;

const NO_LINK: &str = "умный дом сейчас не отвечает";
const NOT_SET: &str = "умный дом не подключён";
pub const NO_CONTROL: &str = "нет такой ручки";

#[derive(Clone, Debug, PartialEq)]
enum Status {
    /// no address kept
    Off,
    Connecting,
    On,
    Failed(String),
}

/// What of the home has been read on this connection.
#[derive(Default)]
struct Lists {
    areas: bool,
    devices: bool,
    entities: bool,
    states: bool,
    /// asked for and not answered yet (the lists and the words): the home is made when none is
    waited: usize,
}

impl Lists {
    fn all(&self) -> bool {
        self.areas && self.devices && self.entities && self.states && self.waited == 0
    }
}

struct Inner {
    address: Option<Address>,
    token: String,
    /// counts the changes of the address and the token: a connection of an older count has nothing more to say
    settings: u64,
    status: Status,
    /// Home Assistant's name of the home and its version
    name: String,
    version: String,
    raw: Raw,
    /// entity → the device it is of (model::owner)
    owner: HashMap<String, String>,
    home: Home,
    /// the home as last listed for the model
    listed: String,
}

impl Inner {
    fn room(&self, d: &Device) -> Option<String> {
        self.home.rooms.iter().find(|r| Some(&r.id) == d.room.as_ref()).map(|r| r.name.clone())
    }
}

struct Job {
    call: model::Call,
    reply: oneshot::Sender<Result<(), String>>,
}

/// What an id sent to Home Assistant asked for.
enum Asked {
    Config,
    Areas,
    Devices,
    Entities,
    States,
    /// the words, kept under a prefix ("": Russian; `model::OWN`: Home Assistant's own language)
    Words(&'static str),
    Subscribed(&'static str),
    Call(oneshot::Sender<Result<(), String>>),
}

const REGISTRIES: [(&str, fn() -> Asked); 3] = [
    ("config/area_registry/list", || Asked::Areas),
    ("config/device_registry/list", || Asked::Devices),
    ("config/entity_registry/list", || Asked::Entities),
];

pub struct House {
    store: Arc<Store>,
    inner: Mutex<Inner>,
    calls: mpsc::Sender<Job>,
    jobs: Mutex<Option<mpsc::Receiver<Job>>>,
    /// the address or the token changed: connect anew
    changed: Notify,
    /// every change of the home: who waits for a control to read a value looks again
    tick: watch::Sender<u64>,
    on_list: Mutex<Option<Box<dyn Fn(String) + Send + Sync>>>,
}

impl House {
    /// With what is kept in the store; `run` then connects.
    pub fn load(store: Arc<Store>) -> Arc<House> {
        let kept = store.setting_kept(KEY).unwrap_or_else(|e| {
            log("house", format!("the kept connection is not read: {e}"));
            None
        });
        let address = kept.as_ref().and_then(|k| Address::parse(k["url"].as_str().unwrap_or("")).ok());
        let token = kept.as_ref().and_then(|k| k["token"].as_str()).unwrap_or("").to_string();
        let (calls, jobs) = mpsc::channel(32);
        Arc::new(House {
            store,
            inner: Mutex::new(Inner {
                status: if address.is_some() { Status::Connecting } else { Status::Off },
                address,
                token,
                settings: 0,
                name: String::new(),
                version: String::new(),
                raw: Raw::default(),
                owner: HashMap::new(),
                home: Home::default(),
                listed: String::new(),
            }),
            calls,
            jobs: Mutex::new(Some(jobs)),
            changed: Notify::new(),
            tick: watch::channel(0).0,
            on_list: Mutex::new(None),
        })
    }

    /// Called with the home as it is listed for the model, now and whenever that changes ("": no home).
    pub fn on_list(&self, f: impl Fn(String) + Send + Sync + 'static) {
        let now = self.inner.lock().unwrap().listed.clone();
        f(now);
        *self.on_list.lock().unwrap() = Some(Box::new(f));
    }

    /// For the web UI: the connection (of the token, its last characters) and the home.
    pub fn json(&self) -> Value {
        let i = self.inner.lock().unwrap();
        let (status, error) = match &i.status {
            Status::Off => ("off", None),
            Status::Connecting => ("connecting", None),
            Status::On => ("on", None),
            Status::Failed(e) => ("failed", Some(e.clone())),
        };
        json!({"status": status, "error": error, "url": i.address.as_ref().map(|a| a.url()).unwrap_or_default(),
               "token_tail": crate::providers::tail(&i.token), "name": i.name, "version": i.version,
               "rooms": i.home.rooms, "devices": i.home.devices})
    }

    /// The address and the token tried, and kept if Home Assistant takes them. An empty token: the one kept, for
    /// the same Home Assistant.
    pub async fn connect(&self, url: &str, token: &str) -> Result<(), String> {
        let address = Address::parse(url).map_err(|f| f.text())?;
        let token = match token.trim() {
            "" => {
                let i = self.inner.lock().unwrap();
                if i.token.is_empty() || i.address.as_ref() != Some(&address) {
                    return Err(if i.token.is_empty() {
                        "Нужен токен"
                    } else {
                        "Для другого адреса нужен его токен"
                    }
                    .into());
                }
                i.token.clone()
            }
            t => t.to_string(),
        };
        ha::open(&address, &token).await.map_err(|f| f.text())?;
        self.store.set_setting(KEY, &json!({"url": address.url(), "token": token}), now_s())?;
        {
            let mut i = self.inner.lock().unwrap();
            let other = i.address.as_ref() != Some(&address);
            (i.address, i.token, i.status) = (Some(address), token, Status::Connecting);
            i.settings += 1;
            if other {
                clear(&mut i); // another home: not the old one's devices under its name
            }
        }
        self.told();
        self.changed.notify_one();
        Ok(())
    }

    /// The connection given up: the address and the token are forgotten.
    pub fn forget(&self) -> Result<(), String> {
        self.store.set_setting(KEY, &Value::Null, now_s())?;
        {
            let mut i = self.inner.lock().unwrap();
            (i.address, i.status) = (None, Status::Off);
            i.token.clear();
            i.settings += 1;
            clear(&mut i);
        }
        self.told();
        self.changed.notify_one();
        Ok(())
    }

    fn emit(&self) {
        if events::heard() {
            events::emit("house", self.json());
        }
    }

    /// Everything said anew: to the web UI, to who waits for a control, to the model's prompt if its list changed.
    fn told(&self) {
        self.emit();
        self.tick.send_modify(|n| *n += 1);
        let list = {
            let mut i = self.inner.lock().unwrap();
            let list = listed(&i);
            if list == i.listed {
                return;
            }
            i.listed = list.clone();
            list
        };
        if let Some(f) = self.on_list.lock().unwrap().as_ref() {
            f(list);
        }
    }

    /// `at`: the settings' count the status is of — one of a connection since replaced is not said.
    fn status(&self, s: Status, at: u64) {
        {
            let mut i = self.inner.lock().unwrap();
            if i.settings != at || i.status == s {
                return;
            }
            i.status = s;
        }
        self.emit();
    }

    // ------------------------------------------------------------ the connection

    /// Connected for as long as there is an address: opened, followed, opened anew when it ends.
    pub async fn run(self: Arc<Self>) {
        let mut jobs = self.jobs.lock().unwrap().take().expect("the house runs once");
        let mut wait = 1;
        loop {
            let (kept, at) = {
                let i = self.inner.lock().unwrap();
                (i.address.clone().map(|a| (a, i.token.clone())), i.settings)
            };
            let Some((address, token)) = kept else {
                self.status(Status::Off, at);
                self.idle(&mut jobs, None, NOT_SET).await;
                continue;
            };
            match self.session(&address, &token, at, &mut jobs).await {
                Ok(()) => wait = 1, // the settings changed
                Err(fault) => {
                    let (was_on, same) = {
                        let i = self.inner.lock().unwrap();
                        (i.status == Status::On, i.settings == at)
                    };
                    if !same {
                        wait = 1; // …while this one was failing: its failure is not the new one's
                        continue;
                    }
                    log("house", format!("Home Assistant at {}: {fault:?}", address.url()));
                    self.status(Status::Failed(fault.text()), at);
                    if was_on {
                        wait = 1;
                    }
                    // a refused token is not tried again by itself: Home Assistant bans an address for that
                    let pause = (fault != Fault::Token).then(|| Duration::from_secs(wait));
                    self.idle(&mut jobs, pause, NO_LINK).await;
                    wait = (wait * 2).min(RETRY_MAX);
                }
            }
        }
    }

    /// Until the settings change (or `pause` is over): whoever sets a control meanwhile is told why not.
    async fn idle(&self, jobs: &mut mpsc::Receiver<Job>, pause: Option<Duration>, why: &str) {
        let until = pause.map(|p| Instant::now() + p);
        loop {
            tokio::select! {
                _ = self.changed.notified() => return,
                _ = tokio::time::sleep_until(until.unwrap_or_else(Instant::now)), if until.is_some() => return,
                Some(j) = jobs.recv() => { let _ = j.reply.send(Err(why.into())); }
            }
        }
    }

    /// One connection, from the token to its end. Ok: the settings changed (connect anew at once). `at`: the
    /// settings' count it was opened for.
    async fn session(
        &self,
        address: &Address,
        token: &str,
        at: u64,
        jobs: &mut mpsc::Receiver<Job>,
    ) -> Result<(), Fault> {
        let (mut ws, version) = ha::open(address, token).await?;
        // it answers again: no more the failure it was, while its lists are read
        if matches!(self.inner.lock().unwrap().status, Status::Failed(_)) {
            self.status(Status::Connecting, at);
        }
        let ready = Instant::now() + READY;
        let mut id = 0u64;
        let mut asked: HashMap<u64, Asked> = HashMap::new();
        let mut got = Lists::default();
        macro_rules! ask {
            ($what:expr, $msg:expr) => {{
                id += 1;
                let (what, mut m): (Asked, Value) = ($what, $msg);
                m["id"] = json!(id);
                if !matches!(what, Asked::Subscribed(_) | Asked::Call(_) | Asked::Config) {
                    got.waited += 1;
                }
                asked.insert(id, what);
                ha::send(&mut ws, &m).await?;
            }};
        }
        let words = |language: &str, category: &str| json!({"type": "frontend/get_translations", "language": language, "category": category});
        // what changes first, then what is: a change that comes after a list is newer than the list
        ask!(Asked::Subscribed("state_changed"), json!({"type": "subscribe_events", "event_type": "state_changed"}));
        for kind in ["area_registry_updated", "device_registry_updated", "entity_registry_updated"] {
            ask!(Asked::Subscribed(kind), json!({"type": "subscribe_events", "event_type": kind}));
        }
        ask!(Asked::Config, json!({"type": "get_config"}));
        ask!(Asked::Words(""), words(LANG, "entity_component"));
        for (list, what) in REGISTRIES {
            ask!(what(), json!({"type": list}));
        }
        ask!(Asked::States, json!({"type": "get_states"}));
        self.inner.lock().unwrap().version = version;
        // Home Assistant's own language (its entities are named in it), and the integrations whose words are had
        let mut language = String::new();
        let mut worded: BTreeSet<String> = BTreeSet::new();
        let mut reread: Option<Instant> = None;
        let mut ping = tokio::time::interval_at(Instant::now() + PING, PING);
        let mut unanswered = false;
        loop {
            tokio::select! {
                m = ha::next(&mut ws) => {
                    let m = m?;
                    match m["type"].as_str() {
                        Some("pong") => unanswered = false,
                        Some("event") => match m["event"]["event_type"].as_str() {
                            Some("state_changed") if got.states => self.state_changed(&m["event"]["data"], at),
                            Some("state_changed") | None => {}
                            // once after a burst — and once in a while during one that does not end
                            Some(_) => { reread.get_or_insert(Instant::now() + SETTLE); }
                        },
                        Some("result") => {
                            let Some(what) = m["id"].as_u64().and_then(|i| asked.remove(&i)) else { continue };
                            let failed = (m["success"] != true).then(|| m["error"]["message"].as_str().unwrap_or("отказ").to_string());
                            let r = &m["result"];
                            let list = || r.as_array().cloned().unwrap_or_default();
                            match what {
                                Asked::Call(reply) => {
                                    let _ = reply.send(failed.map_or(Ok(()), Err));
                                    continue;
                                }
                                Asked::Subscribed(kind) => {
                                    if let Some(e) = failed {
                                        log("house", format!("Home Assistant does not tell of «{kind}»: {e}"));
                                    }
                                    continue;
                                }
                                Asked::Config => {
                                    if let Some(e) = failed {
                                        return Err(Fault::NotHa(e));
                                    }
                                    language = r["language"].as_str().unwrap_or("en").to_string();
                                    ask!(Asked::Words(model::OWN), words(&language, "entity_component"));
                                    let mut i = self.inner.lock().unwrap();
                                    if i.settings != at {
                                        return Ok(());
                                    }
                                    i.name = r["location_name"].as_str().unwrap_or("").to_string();
                                    if let Some(v) = r["version"].as_str() {
                                        i.version = v.to_string();
                                    }
                                    continue;
                                }
                                _ => got.waited -= 1,
                            }
                            // the entities' own words, of the integrations there are (asked again when one is new):
                            // in Russian, and in the language Home Assistant named the entities in
                            if matches!(what, Asked::Entities) && failed.is_none() && !language.is_empty() {
                                let platforms: BTreeSet<String> =
                                    list().iter().filter_map(|e| e["platform"].as_str().map(String::from)).collect();
                                if !platforms.is_subset(&worded) {
                                    for (prefix, language) in [("", LANG), (model::OWN, language.as_str())] {
                                        let mut m = words(language, "entity");
                                        m["integration"] = json!(platforms);
                                        ask!(Asked::Words(prefix), m);
                                    }
                                    worded = platforms;
                                }
                            }
                            let first = {
                                let mut i = self.inner.lock().unwrap();
                                if i.settings != at {
                                    return Ok(()); // of a home since replaced or given up
                                }
                                match what {
                                    // the words are for the look of it: a home without them is still a home
                                    Asked::Words(_) if failed.is_some() => {
                                        log("house", format!("Home Assistant gave no words: {}", failed.unwrap_or_default()));
                                    }
                                    Asked::Words(prefix) => {
                                        for (k, v) in r["resources"].as_object().into_iter().flatten() {
                                            if let Some(v) = v.as_str() {
                                                i.raw.words.insert(format!("{prefix}{k}"), v.to_string());
                                            }
                                        }
                                    }
                                    _ if failed.is_some() => return Err(Fault::NotHa(failed.unwrap_or_default())),
                                    Asked::Areas => (i.raw.areas, got.areas) = (list(), true),
                                    Asked::Devices => (i.raw.devices, got.devices) = (list(), true),
                                    Asked::Entities => {
                                        i.raw.entities = list();
                                        i.owner = i.raw.entities.iter().filter_map(model::owner).collect();
                                        got.entities = true;
                                    }
                                    Asked::States => {
                                        i.raw.states = list().into_iter().filter_map(|s| Some((s["entity_id"].as_str()?.to_string(), s))).collect();
                                        got.states = true;
                                    }
                                    Asked::Config | Asked::Subscribed(_) | Asked::Call(_) => {}
                                }
                                if !got.all() {
                                    continue;
                                }
                                i.home = model::home(&i.raw);
                                let first = (i.status != Status::On).then(|| (i.version.clone(), i.home.devices.len(), i.home.rooms.len()));
                                i.status = Status::On;
                                first
                            };
                            if let Some((version, devices, rooms)) = first {
                                log("house", format!("Home Assistant {version} at {}: {devices} devices in {rooms} rooms", address.url()));
                            }
                            self.told();
                        }
                        _ => {}
                    }
                }
                Some(j) = jobs.recv() => {
                    // whoever asked has stopped waiting: not done behind their back
                    if j.reply.is_closed() {
                        continue;
                    }
                    let c = &j.call;
                    let m = json!({"type": "call_service", "domain": c.domain, "service": c.service, "service_data": c.data,
                                   "target": {"entity_id": c.entity}});
                    ask!(Asked::Call(j.reply), m);
                }
                _ = ping.tick() => {
                    if unanswered {
                        return Err(Fault::Lost("не отвечает".into()));
                    }
                    // (calls nobody waits for any more are not kept for an answer that may never come)
                    asked.retain(|_, a| !matches!(a, Asked::Call(reply) if reply.is_closed()));
                    unanswered = true;
                    id += 1;
                    ha::send(&mut ws, &json!({"id": id, "type": "ping"})).await?;
                }
                _ = tokio::time::sleep_until(reread.unwrap_or_else(Instant::now)), if reread.is_some() => {
                    reread = None;
                    for (list, what) in REGISTRIES {
                        ask!(what(), json!({"type": list}));
                    }
                }
                _ = tokio::time::sleep_until(ready), if !(got.areas && got.devices && got.entities && got.states) => {
                    return Err(Fault::Lost("не прислал список устройств".into()));
                }
                _ = self.changed.notified() => return Ok(()),
            }
        }
    }

    /// An entity's state changed: its device is made anew and, if it shows another thing now, told. A device that is
    /// not shown (Home Assistant's own: the sun, a backup) changes without a word.
    fn state_changed(&self, data: &Value, at: u64) {
        let Some(entity) = data["entity_id"].as_str() else { return };
        let changed = {
            let mut i = self.inner.lock().unwrap();
            if i.settings != at {
                return;
            }
            match &data["new_state"] {
                Value::Null => i.raw.states.remove(entity),
                st => i.raw.states.insert(entity.to_string(), st.clone()),
            };
            // (one Home Assistant keeps no record of is a device of its own)
            let id = i.owner.get(entity).cloned().unwrap_or_else(|| entity.to_string());
            let now = model::one(&i.raw, &id);
            let was = i.home.devices.iter().position(|d| d.id == id);
            match (was, now) {
                (None, None) => return,
                (Some(n), Some(now)) if i.home.devices[n] == now => return,
                // the same device where it stood (not another room's now, nor under another name)
                (Some(n), Some(now)) if i.home.devices[n].room == now.room && i.home.devices[n].name == now.name => {
                    i.home.devices[n] = now.clone();
                    Some(now)
                }
                // a device appears, goes or moves: the whole home anew
                _ => {
                    i.home = model::home(&i.raw);
                    None
                }
            }
        };
        match changed {
            Some(d) => {
                if events::heard() {
                    events::emit("house_device", json!(d));
                }
                self.tick.send_modify(|n| *n += 1);
            }
            None => self.told(),
        }
    }

    // ------------------------------------------------------------ setting a control

    fn control(i: &Inner, id: &str) -> Option<Control> {
        i.home.devices.iter().find_map(|d| d.controls.iter().find(|c| c.id == id)).cloned()
    }

    /// A control set and waited for: Ok with what it reads now (null: nothing that says). `voice`: asked by the
    /// voice (not everything is its to set). Err: why not, in words for the user.
    pub async fn set(&self, id: &str, value: &Value, voice: bool) -> Result<Value, String> {
        let (call, want, before, entity) = {
            let i = self.inner.lock().unwrap();
            match i.status {
                Status::On => {}
                Status::Off => return Err(NOT_SET.into()),
                _ => return Err(NO_LINK.into()),
            }
            let c = House::control(&i, id).ok_or(NO_CONTROL)?;
            if voice && !c.voice {
                return Err("этим управляют только вручную, в веб-интерфейсе".into());
            }
            let (call, want) = model::call(&c, i.raw.states.get(&c.entity).unwrap_or(&Value::Null), value)?;
            // as asked already: nothing is sent. Not only to spare a call — «on» said to a kettle that was just put
            // into «heat to 70» would, by its integration, start a boil instead
            let state = i.raw.states.get(&c.entity).and_then(|s| s["state"].as_str()).unwrap_or("");
            if want.already(&c.value, state) {
                return Ok(c.value);
            }
            (call, want, c.value, c.entity)
        };
        let mut seen = self.tick.subscribe();
        let (reply, answer) = oneshot::channel();
        log("house", format!("{}.{} {} {}", call.domain, call.service, call.entity, call.data));
        self.calls.try_send(Job { call, reply }).map_err(|_| NO_LINK.to_string())?;
        match tokio::time::timeout(REPLY, answer).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => return Err(format!("Home Assistant отказал: {e}")),
            Ok(Err(_)) => return Err(NO_LINK.into()), // the connection ended with the call on it
            Err(_) => return Err("команда отправлена, но Home Assistant ещё не ответил, выполнена ли она".into()),
        }
        if want == model::Want::Done {
            return Ok(Value::Null);
        }
        let until = Instant::now() + CONFIRM;
        loop {
            let (now, state) = {
                let i = self.inner.lock().unwrap();
                let state = i.raw.states.get(&entity).and_then(|s| s["state"].as_str()).unwrap_or("").to_string();
                (House::control(&i, id).map(|c| c.value).unwrap_or(Value::Null), state)
            };
            if want.met(&now, &state, &before) {
                return Ok(now);
            }
            if !matches!(tokio::time::timeout_at(until, seen.changed()).await, Ok(Ok(()))) {
                return Err("команда отправлена, но устройство её не подтвердило".into());
            }
        }
    }

    // ------------------------------------------------------------ for the model

    /// The devices a request speaks of, as they are this second, for the request's stamp: the history only knows what
    /// was true then — a lamp switched on by the voice and off by hand since is «уже горит» to a model that reads it
    /// there. With their controls: the model sets them at once, without asking for the device first.
    pub fn mentioned(&self, text: &str) -> Option<String> {
        let i = self.inner.lock().unwrap();
        if i.status != Status::On {
            return None;
        }
        let devices: Vec<&Device> = i.home.devices.iter().filter(|d| spoken(d)).collect();
        let found = spoken_of(text, &devices, &|d| i.room(d));
        (!found.is_empty()).then(|| format!("{STAMP} {}", said(&found, &|d| i.room(d))))
    }

    /// There is a home: a device the voice has to do with.
    pub fn any(&self) -> bool {
        self.inner.lock().unwrap().home.devices.iter().any(spoken)
    }

    /// The request is about the home: it names a device, a room, or a kind of device that is there («свет», however
    /// many lamps). «Свет выключи» while music plays is then not the player's «выключи», and after a page was read a
    /// device is set only when the user's own words are about the home. Known while Home Assistant is away too.
    pub fn about(&self, text: &str) -> bool {
        let i = self.inner.lock().unwrap();
        let devices: Vec<&Device> = i.home.devices.iter().filter(|d| spoken(d)).collect();
        about(text, &devices, &|d| i.room(d))
    }

    /// A device (or a room's devices) by name, with what the voice may read and set: the tool home_device.
    pub fn describe(&self, name: &str) -> Value {
        let i = self.inner.lock().unwrap();
        match i.status {
            Status::On => {}
            Status::Off => return json!({"ok": false, "error": NOT_SET}),
            _ => return json!({"ok": false, "error": NO_LINK}),
        }
        let devices: Vec<&Device> = i.home.devices.iter().filter(|d| spoken(d)).collect();
        let found = find(&model::plain(name), &devices, &|d| i.room(d));
        if found.is_empty() {
            return json!({"ok": false, "error": "такого устройства нет", "devices": devices.iter().map(|d| &d.name).collect::<Vec<_>>()});
        }
        said(&found, &|d| i.room(d))
    }
}

/// What begins the line of a request's stamp that tells of the home's devices.
pub const STAMP: &str = "умный дом сейчас:";

fn clear(i: &mut Inner) {
    (i.raw, i.home) = (Raw::default(), Home::default());
    i.owner.clear();
    i.name.clear();
    i.version.clear();
}

/// Devices as the model is told of them: whether each is there, and what the voice may read and set on it.
fn said(devices: &[&Device], room: &dyn Fn(&Device) -> Option<String>) -> Value {
    let told: Vec<Value> = devices
        .iter()
        .take(TOLD)
        .map(|d| {
            let controls: Vec<Value> = d
                .controls
                .iter()
                .filter(|c| model::told(c))
                .map(|c| {
                    // what is not the voice's to set is a reading to it
                    let kind = if c.voice { json!(c.kind) } else { json!(Kind::Reading) };
                    let mut v = json!({"control": c.id, "name": c.name, "kind": kind, "value": c.value});
                    for (k, x) in [("min", c.min), ("max", c.max)] {
                        if let Some(x) = x.filter(|_| c.voice) {
                            v[k] = json!(x);
                        }
                    }
                    if let Some(u) = &c.unit {
                        v["unit"] = json!(u);
                    }
                    if !c.options.is_empty() {
                        // the value as the options are said
                        if let Some(o) = c.options.iter().find(|o| c.value == o.value.as_str()) {
                            v["value"] = json!(o.label);
                        }
                        if c.voice {
                            // a light's effects come by the hundred: the first of them, and that there are more
                            v["options"] =
                                json!(c.options.iter().take(model::OPTIONS_SAID).map(|o| &o.label).collect::<Vec<_>>());
                            if c.options.len() > model::OPTIONS_SAID {
                                v["more_options"] = json!(c.options.len() - model::OPTIONS_SAID);
                            }
                        }
                    }
                    v
                })
                .collect();
            let mut v = json!({"device": d.name, "room": room(d), "online": d.online, "controls": controls});
            if !d.online {
                v["note"] = json!("устройство не в сети, управлять им сейчас нельзя");
            }
            v
        })
        .collect();
    json!({"devices": told})
}

/// Two words that are one, however it is declined: «люстру» and «Люстра», «гостиной» and «Гостиная» — but not «гости».
fn like(a: &str, b: &str) -> bool {
    let (la, lb) = (a.chars().count(), b.chars().count());
    if la < 4 || lb < 4 {
        return a == b;
    }
    let common = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    la.abs_diff(lb) <= 2 && common >= 4.max(la.min(lb).saturating_sub(2))
}

/// What kinds of devices a word of a request means.
const KINDS: [(&str, &[&str]); 28] = [
    ("свет", &["light"]),
    ("лампа", &["light"]),
    ("лампочка", &["light"]),
    ("люстра", &["light"]),
    ("торшер", &["light"]),
    ("ночник", &["light"]),
    ("чайник", &["kettle"]),
    ("вскипяти", &["kettle"]),
    ("закипяти", &["kettle"]),
    ("кипяток", &["kettle"]),
    ("бойлер", &["water_heater"]),
    ("водонагреватель", &["water_heater"]),
    ("пылесос", &["vacuum"]),
    ("телевизор", &["tv"]),
    ("телек", &["tv"]),
    ("увлажнитель", &["humidifier"]),
    ("влажность", &["humidifier"]),
    ("кондиционер", &["climate"]),
    ("обогреватель", &["climate"]),
    ("конвектор", &["climate"]),
    ("термостат", &["climate"]),
    // a plug seldom says it is one
    ("розетка", &["outlet", "switch"]),
    ("выключатель", &["switch"]),
    ("шторы", &["cover"]),
    ("жалюзи", &["cover"]),
    ("вентилятор", &["fan"]),
    ("замок", &["lock"]),
    ("газонокосилка", &["mower"]),
];

/// The devices a request speaks of: those it names; else those of a kind it names — in the room it names, if it
/// names one, else all of them when they are few. A room alone means no device («поставь таймер на кухне»).
fn spoken_of<'a>(text: &str, devices: &[&'a Device], room: &dyn Fn(&Device) -> Option<String>) -> Vec<&'a Device> {
    let text = model::plain(text);
    let words: Vec<&str> = text.split(' ').collect();
    let by_name: Vec<&Device> = devices.iter().copied().filter(|d| named(&words, &d.name)).collect();
    if !by_name.is_empty() {
        return by_name;
    }
    let kinds = kinds(&words);
    let of_kind: Vec<&Device> = devices.iter().copied().filter(|d| kinds.contains(&d.kind)).collect();
    let in_room = |d: &&Device| room(d).is_some_and(|r| named(&words, &r));
    if devices.iter().any(in_room) {
        return of_kind.into_iter().filter(in_room).collect();
    }
    if of_kind.len() <= TOLD {
        of_kind
    } else {
        vec![]
    }
}

/// A name is in a request: each of its words, as it is said («люстру» for «Люстра»).
fn named(words: &[&str], name: &str) -> bool {
    let n = model::plain(name);
    !n.is_empty() && n.split(' ').all(|w| words.iter().any(|t| like(t, w)))
}

/// The kinds of devices a request's words mean.
fn kinds(words: &[&str]) -> Vec<&'static str> {
    KINDS.iter().filter(|(w, _)| words.iter().any(|t| like(t, w))).flat_map(|(_, k)| k.iter().copied()).collect()
}

/// A request has a word of this home in it: a device's name, its room's, or its kind. See House::about.
fn about(text: &str, devices: &[&Device], room: &dyn Fn(&Device) -> Option<String>) -> bool {
    let text = model::plain(text);
    let words: Vec<&str> = text.split(' ').collect();
    let kinds = kinds(&words);
    devices
        .iter()
        .any(|d| named(&words, &d.name) || kinds.contains(&d.kind) || room(d).is_some_and(|r| named(&words, &r)))
}

/// A device the voice has something to do with.
fn spoken(d: &Device) -> bool {
    d.controls.iter().any(model::told)
}

/// The devices a name means: one called so, a room's, then what is close to it («люстру» for «Люстра»).
fn find<'a>(q: &str, devices: &[&'a Device], room: &dyn Fn(&Device) -> Option<String>) -> Vec<&'a Device> {
    if q.is_empty() {
        return vec![];
    }
    let name = |d: &Device| model::plain(&d.name);
    let steps: [&dyn Fn(&Device) -> bool; 4] = [
        &|d| name(d) == q,
        &|d| room(d).is_some_and(|r| model::plain(&r) == q),
        &|d| name(d).contains(q) || q.contains(&name(d)),
        &|d| name(d).split(' ').any(|w| q.split(' ').any(|x| like(x, w))),
    ];
    steps
        .iter()
        .map(|f| devices.iter().copied().filter(|d| f(d)).collect::<Vec<_>>())
        .find(|v| !v.is_empty())
        .unwrap_or_default()
}

/// The home in a few lines, for the model: every room's devices by name, with what each is.
fn listed(i: &Inner) -> String {
    let h = &i.home;
    let line = |room: Option<&str>| {
        h.devices
            .iter()
            .filter(|d| d.room.as_deref() == room && spoken(d))
            .map(|d| match model::kind_word(d.kind) {
                // «Чайник (чайник)» says nothing
                word if model::plain(&d.name) == word => d.name.clone(),
                word => format!("{} ({word})", d.name),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut out: Vec<String> = h
        .rooms
        .iter()
        .map(|r| (r.name.as_str(), line(Some(r.id.as_str()))))
        .filter(|(_, l)| !l.is_empty())
        .map(|(n, l)| format!("{n}: {l}."))
        .collect();
    let rest = line(None);
    if !rest.is_empty() {
        out.push(format!("Без комнаты: {rest}."));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inner(home: Home) -> Inner {
        Inner {
            address: None,
            token: String::new(),
            settings: 0,
            status: Status::On,
            name: String::new(),
            version: String::new(),
            raw: Raw::default(),
            owner: HashMap::new(),
            home,
            listed: String::new(),
        }
    }

    #[test]
    fn the_home_as_the_model_is_told() {
        let i = inner(model::home(&model::tests::raw()));
        assert_eq!(listed(&i), "Кухня: Чайник.\nГостиная: Лампа (свет).");
        assert_eq!(listed(&inner(Home::default())), "");
        // a device by what it is called, with what the voice may set and read — an option by its word
        let all: Vec<&Device> = i.home.devices.iter().collect();
        let v = said(&find("чайник", &all, &|d| i.room(d)), &|d| i.room(d));
        let d = &v["devices"][0];
        assert_eq!(
            (d["device"].as_str(), d["room"].as_str(), d["online"].as_bool()),
            (Some("Чайник"), Some("Кухня"), Some(true))
        );
        let controls = d["controls"].as_array().unwrap();
        let by = |id: &str| {
            controls.iter().find(|c| c["control"] == id).unwrap_or_else(|| panic!("no {id} in {controls:?}"))
        };
        assert_eq!(
            by("water_heater.kettle#target"),
            &json!({"control": "water_heater.kettle#target", "name": "Water heater: целевая температура",
                   "kind": "number", "value": 30, "min": 30.0, "max": 100.0, "unit": "°C"})
        );
        assert_eq!(by("select.kettle_mode#option")["value"], "Не выбрано");
        assert_eq!(by("select.kettle_mode#option")["options"], json!(["Не выбрано", "Зелёный чай", "black"]));
        // what the kettle reports about itself is not the voice's business
        assert!(!controls.iter().any(|c| c["control"] == "sensor.kettle_rssi#now"));
        // a device that is away is said to be
        let lamp = said(&find("лампу", &all, &|d| i.room(d)), &|d| i.room(d));
        assert_eq!(lamp["devices"][0]["note"], "устройство не в сети, управлять им сейчас нельзя");
    }

    #[test]
    fn a_device_by_what_it_is_called() {
        let i = inner(model::home(&model::tests::raw()));
        let all: Vec<&Device> = i.home.devices.iter().collect();
        let names =
            |q: &str| find(&model::plain(q), &all, &|d| i.room(d)).iter().map(|d| d.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names("Чайник"), ["Чайник"]);
        assert_eq!(names("чайника"), ["Чайник"]);
        assert_eq!(names("лампу"), ["Лампа"]);
        assert_eq!(names("гостиная"), ["Лампа"]);
        assert_eq!(names("свет на кухне, чайник"), ["Чайник"]);
        assert!(names("пылесос").is_empty());
        assert!(names("").is_empty());
    }

    #[test]
    fn the_devices_a_request_speaks_of() {
        let i = inner(model::home(&model::tests::raw()));
        let all: Vec<&Device> = i.home.devices.iter().collect();
        let of = |t: &str| spoken_of(t, &all, &|d| i.room(d)).iter().map(|d| d.name.as_str()).collect::<Vec<_>>();
        assert_eq!(of("закипяти чайник до 70 градусов"), ["Чайник"]);
        assert_eq!(of("выключи подсветку чайника"), ["Чайник"]);
        assert_eq!(of("вскипяти воды"), ["Чайник"]);
        assert_eq!(of("включи лампу"), ["Лампа"]);
        // a kind: in the room named, else everywhere
        assert_eq!(of("выключи свет в гостиной"), ["Лампа"]);
        assert_eq!(of("выключи свет"), ["Лампа"]);
        assert!(of("включи свет на кухне").is_empty(), "no light there: not the one elsewhere");
        // a room alone is where something else is asked for
        for t in ["поставь таймер на кухне на пять минут", "включи музыку на кухне", "что включено на кухне"]
        {
            assert!(of(t).is_empty(), "{t}");
        }
        // nothing of the home: the stamp says nothing of it
        for t in ["сколько будет 17 умножить на 23", "вечером придут гости, включи музыку", "завари чай"]
        {
            assert!(of(t).is_empty(), "{t}");
        }
        assert!(
            like("люстру", "люстра")
                && like("гостиной", "гостиная")
                && !like("гости", "гостиная")
                && !like("чай", "чайник")
        );
        assert!(like("ac", "ac") && !like("ac", "act"));
        // about the home at all: a name, a room or a kind that is there — however many devices that is, whatever
        // is asked («заблокируй» is no worse a word than «включи»)
        let is = |t: &str| about(t, &all, &|d| i.room(d));
        for t in ["свет выключи", "заблокируй кнопки на чайнике", "выключи всё в гостиной", "включи свет на кухне"]
        {
            assert!(is(t), "{t}");
        }
        // the player's words and what only sounds like the home («Светлана», a fan that is not there)
        for t in ["пауза", "выключи", "дальше", "Светлана придёт в семь", "спасибо", "включи вентилятор", "завари чай"]
        {
            assert!(!is(t), "{t}");
        }
    }

    #[test]
    fn the_token_stays_on_the_server() {
        let store = Store::open(":memory:").unwrap();
        store.set_setting(KEY, &json!({"url": "http://192.168.1.10", "token": "abcdefgh.secret.tail"}), 0.0).unwrap();
        let j = House::load(store).json();
        assert_eq!(
            (j["url"].as_str(), j["token_tail"].as_str(), j["status"].as_str()),
            (Some("http://192.168.1.10"), Some("tail"), Some("connecting"))
        );
        assert!(!j.to_string().contains("secret"), "of the token, its last four characters only");
    }

    #[tokio::test]
    async fn nothing_is_set_with_no_home() {
        let store = Store::open(":memory:").unwrap();
        let h = House::load(store);
        assert_eq!(h.json()["status"], "off");
        assert_eq!(h.set("light.x#power", &json!(true), true).await.unwrap_err(), NOT_SET);
        assert_eq!(h.describe("чайник")["error"], NOT_SET);
        assert_eq!(h.mentioned("включи чайник"), None);
        assert_eq!(
            h.connect("ftp://x", "t").await.unwrap_err(),
            "Не похоже на адрес: схема «ftp» не подходит, нужен http или https"
        );
        assert_eq!(h.connect("192.168.1.10", " ").await.unwrap_err(), "Нужен токен");
    }
}
