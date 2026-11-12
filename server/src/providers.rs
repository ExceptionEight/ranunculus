//! Who does what. A connection is a service's address and the key for it, given once: OpenRouter, or a server at
//! home with an OpenAI-style API. A role is a job done by a model on one of the connections: recognition (asr), the
//! model that answers (llm), the voice (tts), the vectors the memory is searched by (embeddings). Kept in server.db
//! (the keys never leave the server). Every station uses the same ones; a change counts from the next request:
//! nothing is restarted.
//! The first start makes one connection, OpenRouter, without a key. Once the key is given and nothing is chosen yet,
//! every role gets its model of DEFAULTS: the installation works at once, and each is the user's to change. Never
//! again after that: a role given up (the memory) stays given up, and a home installation that adds OpenRouter for one
//! model sends nothing else there.
use crate::events;
use crate::llm::Llm;
use crate::memory::Memory;
use crate::services::{Endpoint, Services};
use crate::session::log;
use crate::store::{ConnectionRow, RoleRow, Store};
use crate::text::now_s;
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex, RwLock};

pub const ROLES: [&str; 4] = ["asr", "llm", "tts", "embeddings"];

pub const OPENROUTER: &str = "https://openrouter.ai/api/v1";

/// What a role gets on OpenRouter until the user chooses: (role, model, voice, the request's own fields). Tried in
/// Russian on everything OpenRouter had in November 2026:
/// - asr: the fastest of the accurate ones (half a second for a command), and silent on noise, where the Whisper
///   family writes «Субтитры сделал DimaTorzok»;
/// - tts: the user's pick by ear of ten voices that read Russian well; starts speaking in about 0.7 s a sentence,
///   reads numbers and foreign names, takes a speed of its own, and is among the cheapest;
/// - embeddings: found the right exchange most often; 1024 numbers of its 3072 lose almost nothing and take a
///   third of the room;
/// - llm: what the prompt was written with.
pub const DEFAULTS: [(&str, &str, &str, &str); 4] = [
    ("asr", "assemblyai/universal-3-6-pro", "", "{}"),
    ("llm", "deepseek/deepseek-v4.1-flash", "", "{}"),
    ("tts", "x-ai/grok-voice-tts-1.0", "eve", "{}"),
    ("embeddings", "google/gemini-embedding-2", "", r#"{"dimensions": 1024}"#),
];

/// What every request uses now.
struct InUse {
    llm: Option<Arc<Llm>>,
    services: Services,
}

pub struct Providers {
    store: Arc<Store>,
    /// told which vectors to make (None: a test without a memory)
    memory: Option<Arc<Memory>>,
    /// read from the database at the start and after every change: a request does not wait for the database
    in_use: RwLock<InUse>,
    /// one refresh at a time: two at once could put the older reading in place last
    refreshing: Mutex<()>,
}

/// «https://openrouter.ai» of «https://openrouter.ai/api/v1»: where a key goes.
pub fn site(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split('/').next().unwrap_or("");
    format!("{}://{}", url.split("://").next().unwrap_or(""), host.to_ascii_lowercase())
}

pub fn is_openrouter(url: &str) -> bool {
    site(url) == site(OPENROUTER)
}

/// Away from the house (the internet): what is said there goes out of it. What the web UI calls the cloud, what the
/// wake's window is never sent to, and a model with no one slot of ours.
pub fn cloud(url: &str) -> bool {
    !at_home(url)
}

/// In the house by its address, whatever the scheme: a container's name, this computer, a private address, a name of
/// the local network, the user's own devices over Tailscale (100.64/10, *.ts.net). Where what must not leave the house
/// may go; anything else counts as away.
pub fn at_home(url: &str) -> bool {
    use std::net::IpAddr;
    let Some(host) =
        reqwest::Url::parse(url).ok().and_then(|u| Some(u.host_str()?.trim_matches(['[', ']']).to_lowercase()))
    else {
        return false;
    };
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(a)) => {
            let [x, y, ..] = a.octets();
            a.is_private() || a.is_loopback() || a.is_link_local() || (x == 100 && (64..128).contains(&y))
        }
        Ok(IpAddr::V6(a)) => a.is_loopback() || a.is_unique_local() || a.is_unicast_link_local(),
        Err(_) => {
            let local = [".local", ".lan", ".home.arpa", ".internal", ".ts.net"];
            !host.contains('.') || local.iter().any(|s| host.ends_with(s))
        }
    }
}

pub fn endpoint(c: &ConnectionRow, r: &RoleRow) -> Endpoint {
    Endpoint {
        url: c.url.trim_end_matches('/').to_string(),
        model: r.model.clone(),
        key: c.key.clone(),
        voice: r.voice.clone(),
        extra: r.extra.as_object().cloned().unwrap_or_default(),
    }
}

pub fn llm_of(c: &ConnectionRow, r: &RoleRow) -> Llm {
    Llm::new(
        &format!("{}/chat/completions", c.url.trim_end_matches('/')),
        &r.model,
        &c.key,
        r.extra.as_object().cloned().unwrap_or_default(),
    )
}

/// A connection as the API shows it: the key only as «set, ends with …».
pub fn public(c: &ConnectionRow) -> Value {
    let tail: String = c.key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    json!({"id": c.id, "name": c.name, "url": c.url, "key_set": !c.key.is_empty(),
           "key_tail": if c.key.chars().count() > 8 { tail } else { String::new() }, "cloud": cloud(&c.url),
           "openrouter": is_openrouter(&c.url)})
}

impl Providers {
    /// From server.db; the first time, the connection to OpenRouter is made, its key the user's to give.
    pub fn load(store: Arc<Store>, memory: Option<Arc<Memory>>) -> Arc<Providers> {
        let p = Providers {
            store,
            memory,
            in_use: RwLock::new(InUse { llm: None, services: Services::new(None, None, None) }),
            refreshing: Mutex::new(()),
        };
        // made only into tables that are there and empty: a read that fails for a moment must not look empty, or
        // it is made twice
        match (p.store.connections(), p.store.roles()) {
            (Ok(c), Ok(r)) if c.is_empty() && r.is_empty() => {
                let first = ConnectionRow {
                    id: 0,
                    name: "OpenRouter".into(),
                    url: OPENROUTER.into(),
                    key: String::new(),
                    updated: now_s(),
                };
                match p.store.save_connection(&first) {
                    Ok(_) => log("server", "no models yet: give the key of OpenRouter in the web UI, in the settings"),
                    Err(e) => log("server", format!("   (connections: {e})")),
                }
            }
            (Ok(_), Ok(_)) => {}
            (Err(e), _) | (_, Err(e)) => log("server", format!("connections not read ({e}): no models until they are")),
        }
        p.refresh();
        Arc::new(p)
    }

    /// All of them; a read that failed — none, said in the log.
    pub fn connections(&self) -> Vec<ConnectionRow> {
        self.store.connections().unwrap_or_else(|e| {
            log("server", format!("   (connections not read: {e})"));
            vec![]
        })
    }

    pub fn connection(&self, id: i64) -> Option<ConnectionRow> {
        self.connections().into_iter().find(|c| c.id == id)
    }

    pub fn roles(&self) -> Vec<RoleRow> {
        self.store.roles().unwrap_or_else(|e| {
            log("server", format!("   (roles not read: {e})"));
            vec![]
        })
    }

    /// What a role uses, with its connection; None: nothing chosen.
    pub fn role(&self, role: &str) -> Option<(ConnectionRow, RoleRow)> {
        let r = self.roles().into_iter().find(|r| r.role == role)?;
        Some((self.connection(r.connection)?, r))
    }

    /// Every role given OpenRouter's model, the key having just been given, if nothing is chosen yet → which ones.
    pub fn defaults(&self, c: &ConnectionRow) -> Vec<&'static str> {
        // read from the database itself: a read that fails must not look like nothing chosen
        if !is_openrouter(&c.url) || c.key.is_empty() || !self.store.roles().is_ok_and(|r| r.is_empty()) {
            return vec![];
        }
        let mut made = vec![];
        for (role, model, voice, extra) in DEFAULTS {
            let row = RoleRow {
                role: role.into(),
                connection: c.id,
                model: model.into(),
                voice: voice.into(),
                speed: None,
                extra: serde_json::from_str(extra).unwrap_or(json!({})),
                updated: now_s(),
            };
            match self.store.save_role(&row) {
                Ok(()) => made.push(role),
                Err(e) => log("server", format!("   (roles: {e})")),
            }
        }
        made
    }

    fn refresh(&self) {
        let _one = self.refreshing.lock().unwrap();
        let (connections, roles) = (self.connections(), self.roles());
        let of = |role: &str| {
            let r = roles.iter().find(|r| r.role == role)?;
            Some((connections.iter().find(|c| c.id == r.connection)?, r))
        };
        let tts = of("tts");
        let services = Services::new(
            of("asr").map(|(c, r)| endpoint(c, r)),
            tts.map(|(c, r)| endpoint(c, r)),
            tts.and_then(|(_, r)| r.speed).map(|s| s as f32),
        );
        *self.in_use.write().unwrap() = InUse { llm: of("llm").map(|(c, r)| Arc::new(llm_of(c, r))), services };
        if let Some(m) = &self.memory {
            m.embed_with(of("embeddings").map(|(c, r)| endpoint(c, r)));
        }
    }

    /// The model that answers; None: none is chosen yet.
    pub fn llm(&self) -> Option<Arc<Llm>> {
        self.in_use.read().unwrap().llm.clone()
    }

    /// The recognition and the voice.
    pub fn services(&self) -> Services {
        self.in_use.read().unwrap().services.clone()
    }

    /// For the web UI: the connections (keys hidden) and what each role uses (null: nothing).
    pub fn json(&self) -> Value {
        let (connections, roles) = (self.connections(), self.roles());
        let chosen: Map<String, Value> = ROLES
            .iter()
            .map(|role| {
                let v = roles.iter().find(|r| r.role == *role).map(|r| {
                    let cloud = connections.iter().find(|c| c.id == r.connection).is_some_and(|c| cloud(&c.url));
                    json!({"connection": r.connection, "model": r.model, "voice": r.voice, "speed": r.speed, "extra": r.extra, "cloud": cloud})
                });
                (role.to_string(), json!(v))
            })
            .collect();
        json!({"connections": connections.iter().map(public).collect::<Vec<_>>(), "roles": chosen})
    }

    /// After any change in the database: what is in use is read again, and the web UI is told.
    pub fn changed(&self) {
        self.refresh();
        events::emit("providers", self.json());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> (Arc<Store>, std::path::PathBuf) {
        let d = std::env::temp_dir().join(format!("server-providers-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        (Store::open(&d.join("server.db").to_string_lossy()).unwrap(), d)
    }

    fn role(role: &str, connection: i64, model: &str) -> RoleRow {
        RoleRow {
            role: role.into(),
            connection,
            model: model.into(),
            voice: String::new(),
            speed: None,
            extra: json!({}),
            updated: now_s(),
        }
    }

    #[test]
    fn one_key_and_everything_works() {
        let (store, d) = store("first");
        let p = Providers::load(store.clone(), None);
        // the first start: OpenRouter without a key, nothing chosen, nothing to ask
        let c = p.connections();
        assert_eq!(
            c.iter().map(|c| (c.name.as_str(), c.url.as_str(), c.key.as_str())).collect::<Vec<_>>(),
            [("OpenRouter", OPENROUTER, "")]
        );
        assert!(p.roles().is_empty() && p.llm().is_none() && p.services().asr.is_none() && p.services().tts.is_none());
        assert!(p.defaults(&c[0]).is_empty(), "no key: nothing to choose with");
        assert_eq!(p.json()["roles"], json!({"asr": null, "llm": null, "tts": null, "embeddings": null}));
        // the key given: every role gets its model
        let mut or = c[0].clone();
        or.key = "sk-or-v1-secret-abcd".into();
        store.save_connection(&or).unwrap();
        assert_eq!(p.defaults(&or), ROLES);
        p.changed();
        let (llm, s) = (p.llm().unwrap(), p.services());
        assert_eq!(
            (llm.url.as_str(), llm.model.as_str(), llm.cloud()),
            ("https://openrouter.ai/api/v1/chat/completions", "deepseek/deepseek-v4.1-flash", true)
        );
        let (asr, tts) = (s.asr.unwrap(), s.tts.unwrap());
        assert_eq!(
            (asr.url.as_str(), asr.model.as_str(), asr.key.as_str()),
            (OPENROUTER, "assemblyai/universal-3-6-pro", or.key.as_str())
        );
        assert_eq!((tts.model.as_str(), tts.voice.as_str()), ("x-ai/grok-voice-tts-1.0", "eve"));
        let j = p.json();
        assert_eq!(j["roles"]["embeddings"]["extra"], json!({"dimensions": 1024}));
        assert_eq!(
            (j["roles"]["tts"]["connection"].as_i64(), j["roles"]["tts"]["cloud"].clone()),
            (Some(or.id), json!(true))
        );
        // the key: never out
        let pj = &j["connections"][0];
        assert!(!j.to_string().contains("secret") && pj["key_set"] == json!(true) && pj["key_tail"] == json!("abcd"));
        assert!(pj["cloud"] == json!(true) && pj["openrouter"] == json!(true));
        // a model at home for the answers, the voice with a speed of its own: from the next request
        let home = store
            .save_connection(&ConnectionRow {
                id: 0,
                name: "Дома".into(),
                url: "http://127.0.0.1:11434/v1/".into(),
                key: String::new(),
                updated: 1.0,
            })
            .unwrap();
        store.save_role(&role("llm", home, "qwen")).unwrap();
        store
            .save_role(&RoleRow {
                speed: Some(1.4),
                voice: "ara".into(),
                ..role("tts", or.id, "x-ai/grok-voice-tts-1.0")
            })
            .unwrap();
        p.changed();
        let llm = p.llm().unwrap();
        assert_eq!(
            (llm.url.as_str(), llm.model.as_str(), llm.cloud()),
            ("http://127.0.0.1:11434/v1/chat/completions", "qwen", false)
        );
        assert!(!p.services().same_voice(&s_of(&p, None)) && p.services().same_voice(&s_of(&p, Some(1.4))));
        assert_eq!(p.json()["roles"]["llm"]["cloud"], json!(false));
        // chosen once: the key given again changes nothing; a role given up is not chosen for the user again
        store.delete_role("embeddings").unwrap();
        store.delete_role("asr").unwrap();
        assert!(p.defaults(&or).is_empty() && p.defaults(&p.connection(home).unwrap()).is_empty());
        // a second start: nothing is made again, the choice stays
        let p2 = Providers::load(store.clone(), None);
        assert_eq!((p2.connections().len(), p2.roles().len(), p2.llm().unwrap().model.as_str()), (2, 2, "qwen"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The voice in use, asked at another speed.
    fn s_of(p: &Providers, speed: Option<f32>) -> Services {
        Services::new(None, p.services().tts, speed)
    }

    #[test]
    fn a_failed_read_at_the_start_makes_nothing_again() {
        let (store, d) = store("fail");
        let p = Providers::load(store.clone(), None);
        assert_eq!(p.connections().len(), 1);
        // the table out of reach for a moment (another connection takes it away)
        let db = rusqlite::Connection::open(d.join("server.db")).unwrap();
        db.execute_batch("ALTER TABLE connections RENAME TO connections_away").unwrap();
        let p2 = Providers::load(store.clone(), None);
        assert!(p2.connections().is_empty() && p2.llm().is_none());
        db.execute_batch("ALTER TABLE connections_away RENAME TO connections").unwrap();
        p2.changed();
        assert_eq!(p2.connections().len(), 1, "no doubles");
        // nor are roles that cannot be read taken for none chosen: the key given then chooses nothing over them
        let mut or = p2.connections()[0].clone();
        or.key = "sk-or-v1-k".into();
        db.execute_batch("ALTER TABLE roles RENAME TO roles_away").unwrap();
        assert!(p2.defaults(&or).is_empty());
        db.execute_batch("ALTER TABLE roles_away RENAME TO roles").unwrap();
        assert_eq!(p2.defaults(&or), ROLES, "and once they are read: none was chosen");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn at_home_by_the_address() {
        for url in [
            "http://asr:8000/v1",
            "http://localhost:11434/v1",
            "http://host.docker.internal:11434/v1",
            "https://127.0.0.1:8443/v1",
            "http://192.168.1.20:8000/v1",
            "http://10.0.0.5/v1",
            "http://[::1]:8000/v1",
            "http://[fd00::5]:8000/v1",
            "https://gpu.lan/v1",
            "http://nas.local:9000/v1",
            "http://box.home.arpa/v1",
            "http://100.101.102.103:11434/v1",
            "https://gpu.tail1234.ts.net/v1",
        ] {
            assert!(at_home(url), "{url}");
        }
        for url in [
            "https://openrouter.ai/api/v1",
            "http://my-vps.example.com:8000/v1",
            "http://203.0.113.7:8000/v1",
            "http://[2001:db8::1]/v1",
            "http://8.8.8.8/v1",
            "http://100.128.0.1/v1",
            "not an address",
        ] {
            assert!(!at_home(url), "{url}");
        }
    }

    #[test]
    fn a_keys_site() {
        assert_eq!(site("https://openrouter.ai/api/v1"), "https://openrouter.ai");
        assert_eq!(site("https://OpenRouter.ai/v1"), site("https://openrouter.ai/api/v1"));
        assert_ne!(site("https://openrouter.ai.evil.lan/v1"), site("https://openrouter.ai/api/v1"));
        assert_ne!(site("http://openrouter.ai/v1"), site("https://openrouter.ai/v1"), "not over plain http");
        assert_ne!(site("http://127.0.0.1:52521/v1"), site("http://127.0.0.1:9999/v1"));
        assert!(
            is_openrouter("https://openrouter.ai/api/v1/") && !is_openrouter("https://openrouter.ai.evil.lan/api/v1")
        );
    }
}
