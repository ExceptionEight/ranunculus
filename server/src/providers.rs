//! Connections: the model that answers (llm), speech recognition (asr) and the voice (tts). Each is a service with an
//! OpenAI-style API, one of ours at home (services/) or one in the internet (OpenRouter, OpenAI). Kept in server.db
//! (the keys never leave the server). One of each kind is in use, for every station; a change counts from the next
//! request: nothing is restarted.
//! The first start makes the recognition and the voice from server.toml's `[services]` (the ones that come with the
//! installation); the model is the user's to add in the web UI.
use crate::config;
use crate::events;
use crate::llm::Llm;
use crate::services::{Endpoint, Services};
use crate::session::log;
use crate::store::{ProviderRow, Store};
use crate::text::now_s;
use serde_json::{json, Map, Value};
use std::sync::{Arc, RwLock};

pub const KINDS: [&str; 3] = ["llm", "asr", "tts"];

/// What every request uses now.
struct InUse {
    llm: Option<Arc<Llm>>,
    services: Services,
}

pub struct Providers {
    store: Arc<Store>,
    /// server.toml's recognition and voice: used while the database has none of a kind
    config: Services,
    /// the same as connections: made at the first start, and offered again in the web UI once deleted
    built_in: Vec<ProviderRow>,
    /// read from the database at the start and after every change: a request does not wait for the database
    in_use: RwLock<InUse>,
}

pub fn endpoint(p: &ProviderRow) -> Endpoint {
    Endpoint {
        url: p.url.trim_end_matches('/').to_string(),
        model: p.model.clone(),
        key: p.key.clone(),
        voice: p.voice.clone(),
        extra: p.extra.as_object().cloned().unwrap_or_default(),
    }
}

pub fn llm_of(p: &ProviderRow) -> Llm {
    Llm::new(
        &format!("{}/chat/completions", p.url.trim_end_matches('/')),
        &p.model,
        &p.key,
        p.extra.as_object().cloned().unwrap_or_default(),
    )
}

/// In the internet: what is said there goes out of the house.
pub fn cloud(url: &str) -> bool {
    url.starts_with("https://")
}

/// A provider as the API shows it: the key only as «set, ends with …».
pub fn public(p: &ProviderRow) -> Value {
    let tail: String = p.key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    json!({"id": p.id, "kind": p.kind, "name": p.name, "url": p.url, "model": p.model, "voice": p.voice, "speed": p.speed, "extra": p.extra,
           "key_set": !p.key.is_empty(), "key_tail": if p.key.chars().count() > 8 { tail } else { String::new() }, "cloud": cloud(&p.url)})
}

fn row(kind: &str, name: &str, url: String, model: &str, extra: Map<String, Value>) -> ProviderRow {
    ProviderRow {
        id: 0,
        kind: kind.into(),
        name: name.into(),
        url,
        model: model.into(),
        key: String::new(),
        voice: String::new(),
        speed: None,
        extra: Value::Object(extra),
        updated: now_s(),
    }
}

/// The recognition and the voice that come with the installation; "" in server.toml: no such service, the user
/// connects their own.
fn built_in(cfg: &config::Services) -> Vec<ProviderRow> {
    [("asr", "GigaAM", &cfg.asr, "gigaam"), ("tts", "Silero", &cfg.tts, "")]
        .into_iter()
        .filter(|(_, _, address, _)| !address.is_empty())
        .map(|(kind, name, address, model)| {
            row(kind, name, format!("{}/v1", address.trim_end_matches('/')), model, Map::new())
        })
        .collect()
}

impl Providers {
    /// From server.db; the first time, the recognition and the voice are made from server.toml's `[services]`.
    pub fn load(store: Arc<Store>, cfg: &config::Services) -> Arc<Providers> {
        let config = Services::new(&cfg.asr, &cfg.tts, 1.0);
        let p = Providers {
            store,
            config: config.clone(),
            built_in: built_in(cfg),
            in_use: RwLock::new(InUse { llm: None, services: config }),
        };
        // made only into a table that is there and empty: a read that fails for a moment must not look empty, or
        // they are made twice
        match p.store.providers() {
            Ok(l) if l.is_empty() => {
                for x in &p.built_in {
                    match p.store.save_provider(x) {
                        Ok(id) => {
                            if let Err(e) = p.choose(&x.kind, id) {
                                log("server", format!("   (connections: {e})"));
                            }
                        }
                        Err(e) => log("server", format!("   (connections: {e})")),
                    }
                }
                let made = p.list();
                if !made.is_empty() {
                    log(
                        "server",
                        format!(
                            "connections made from server.toml: {}",
                            made.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", ")
                        ),
                    );
                }
            }
            Ok(_) => {}
            Err(e) => {
                log("server", format!("connections not read ({e}): recognition and the voice as server.toml says"))
            }
        }
        p.refresh();
        Arc::new(p)
    }

    /// All of them; a read that failed — none, said in the log.
    pub fn list(&self) -> Vec<ProviderRow> {
        self.store.providers().unwrap_or_else(|e| {
            log("server", format!("   (connections not read: {e})"));
            vec![]
        })
    }

    pub fn get(&self, id: i64) -> Option<ProviderRow> {
        self.list().into_iter().find(|p| p.id == id)
    }

    /// kind → the id chosen in the web UI.
    fn chosen(&self) -> Map<String, Value> {
        self.store.setting("providers").and_then(|v| v.as_object().cloned()).unwrap_or_default()
    }

    /// Puts a provider in use for its kind (`changed` then tells everyone).
    pub fn choose(&self, kind: &str, id: i64) -> Result<(), String> {
        let mut c = self.chosen();
        c.insert(kind.into(), json!(id));
        self.store.set_setting("providers", &Value::Object(c), now_s())
    }

    /// The provider of each kind in use: the chosen one, else the first of the kind.
    fn rows_in_use(&self) -> Vec<ProviderRow> {
        let (list, chosen) = (self.list(), self.chosen());
        KINDS
            .iter()
            .filter_map(|kind| {
                let of_kind = || list.iter().filter(|p| p.kind == *kind);
                let id = chosen.get(*kind).and_then(|v| v.as_i64());
                of_kind().find(|p| Some(p.id) == id).or_else(|| of_kind().next()).cloned()
            })
            .collect()
    }

    /// Is this provider the one in use for its kind?
    pub fn uses(&self, id: i64) -> bool {
        self.rows_in_use().iter().any(|p| p.id == id)
    }

    fn refresh(&self) {
        let rows = self.rows_in_use();
        let of = |kind: &str| rows.iter().find(|p| p.kind == kind);
        let tts = of("tts");
        let services = Services::with(
            of("asr").map(endpoint).unwrap_or_else(|| self.config.asr.clone()),
            tts.map(endpoint).unwrap_or_else(|| self.config.tts.clone()),
            tts.and_then(|p| p.speed).map(|s| s as f32).unwrap_or(self.config.speed()),
        );
        *self.in_use.write().unwrap() = InUse { llm: of("llm").map(|p| Arc::new(llm_of(p))), services };
    }

    /// The model that answers; None: the user has not added one yet.
    pub fn llm(&self) -> Option<Arc<Llm>> {
        self.in_use.read().unwrap().llm.clone()
    }

    /// The recognition and the voice.
    pub fn services(&self) -> Services {
        self.in_use.read().unwrap().services.clone()
    }

    /// For the web UI: all of them (keys hidden), the one of each kind in use, and what came with the installation
    /// and is not connected now.
    pub fn json(&self) -> Value {
        let (list, rows) = (self.list(), self.rows_in_use());
        let in_use: Map<String, Value> =
            KINDS.iter().map(|k| (k.to_string(), json!(rows.iter().find(|p| p.kind == *k).map(|p| p.id)))).collect();
        let built_in: Vec<Value> = self
            .built_in
            .iter()
            .filter(|b| !list.iter().any(|p| p.kind == b.kind && p.url == b.url))
            .map(|b| json!({"kind": b.kind, "name": b.name, "url": b.url, "model": b.model}))
            .collect();
        json!({"items": list.iter().map(public).collect::<Vec<_>>(), "in_use": in_use, "built_in": built_in})
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

    fn llm_row(name: &str, url: &str, model: &str) -> ProviderRow {
        ProviderRow { key: "sk-or-v1-secret-abcd".into(), ..row("llm", name, url.into(), model, Map::new()) }
    }

    #[test]
    fn made_at_the_first_start_then_chosen() {
        let (store, d) = store("first");
        let cfg = config::Services::default();
        let p = Providers::load(store.clone(), &cfg);
        // the recognition and the voice from server.toml, in use; no model until the user adds one
        assert_eq!(
            p.list().iter().map(|x| (x.kind.as_str(), x.name.as_str())).collect::<Vec<_>>(),
            [("asr", "GigaAM"), ("tts", "Silero")]
        );
        let s = p.services();
        assert_eq!(
            (s.asr.url.clone(), s.asr.model.as_str(), s.tts.url.clone(), s.speed()),
            (format!("{}/v1", cfg.asr), "gigaam", format!("{}/v1", cfg.tts), 1.0)
        );
        assert!(p.llm().is_none() && p.json()["in_use"]["llm"].is_null());
        assert_eq!(p.json()["built_in"], json!([]), "both are connected: nothing to offer");
        // a model added: the only one of its kind is in use at once
        let cloud_id = store
            .save_provider(&llm_row("DeepSeek", "https://openrouter.ai/api/v1", "deepseek/deepseek-v4.1-flash"))
            .unwrap();
        p.changed();
        let llm = p.llm().unwrap();
        assert_eq!(
            (llm.url.as_str(), llm.model.as_str(), llm.cloud()),
            ("https://openrouter.ai/api/v1/chat/completions", "deepseek/deepseek-v4.1-flash", true)
        );
        // the key: never out
        let pj = public(&p.get(cloud_id).unwrap());
        assert!(
            !pj.to_string().contains("secret")
                && pj["key_set"] == json!(true)
                && pj["key_tail"] == json!("abcd")
                && pj["cloud"] == json!(true)
        );
        // a second one: in use once chosen, for everyone
        let home_id =
            store.save_provider(&row("llm", "Дома", "http://127.0.0.1:11434/v1".into(), "qwen", Map::new())).unwrap();
        p.changed();
        assert_eq!(p.llm().unwrap().model, "deepseek/deepseek-v4.1-flash");
        p.choose("llm", home_id).unwrap();
        p.changed();
        assert_eq!(
            (p.llm().unwrap().model.as_str(), p.llm().unwrap().cloud(), p.uses(home_id), p.uses(cloud_id)),
            ("qwen", false, true, false)
        );
        assert_eq!(p.json()["in_use"]["llm"], json!(home_id));
        // changed in the web UI: from the next request
        let mut changed = p.get(home_id).unwrap();
        changed.model = "gemma".into();
        store.save_provider(&changed).unwrap();
        p.changed();
        assert_eq!(p.llm().unwrap().model, "gemma");
        // the voice's own speed comes with its provider
        let mut voice = p.list().into_iter().find(|x| x.kind == "tts").unwrap();
        voice.speed = Some(1.4);
        store.save_provider(&voice).unwrap();
        p.changed();
        assert_eq!(p.services().speed(), 1.4);
        // a second start: nothing is made again, the choice stays
        let p2 = Providers::load(store.clone(), &cfg);
        assert_eq!((p2.list().len(), p2.llm().unwrap().model.as_str()), (4, "gemma"));
        // the voice that came with the installation, deleted: offered again
        store.delete_provider(voice.id).unwrap();
        assert_eq!(
            p2.json()["built_in"],
            json!([{"kind": "tts", "name": "Silero", "url": format!("{}/v1", cfg.tts), "model": ""}])
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_read_at_the_start_makes_nothing_again() {
        let (store, d) = store("fail");
        let cfg = config::Services::default();
        let p = Providers::load(store.clone(), &cfg);
        let (n, in_use) = (p.list().len(), p.json()["in_use"].clone());
        assert_eq!(n, 2);
        // the table out of reach for a moment (another connection takes it away)
        let db = rusqlite::Connection::open(d.join("server.db")).unwrap();
        db.execute_batch("ALTER TABLE providers RENAME TO providers_away").unwrap();
        let p2 = Providers::load(store.clone(), &cfg);
        assert!(p2.list().is_empty() && p2.llm().is_none());
        assert_eq!(p2.services().asr.url, format!("{}/v1", cfg.asr), "nothing read: server.toml's");
        db.execute_batch("ALTER TABLE providers_away RENAME TO providers").unwrap();
        p2.changed();
        assert_eq!((p2.list().len(), p2.json()["in_use"].clone()), (n, in_use), "no doubles, the choice as it was");
        let _ = std::fs::remove_dir_all(&d);
    }
}
