//! The server's settings: a TOML file, `server.toml` in the working directory unless another is named on the command
//! line. Every key is optional; without a file these defaults apply:
//! ```toml
//! data = "data"                      # all the server keeps (see below)
//! listen = "0.0.0.0:52700"           # the stations connect here (TLS with the certificate in `data`)
//! station_client = "station/ranunculus-station"   # the client for stations, of this server's version: a station
//!                                    #   of another version is given it and updates itself; "" or no such file — refused
//! [services]                         # the services beside the server (services/); "" — none. The models — who
//!                                    #   hears, answers, speaks — are chosen in the web UI (Настройки → Модели)
//! pages = "http://127.0.0.1:52712"   # a web page as text (the read_page tool)
//! search = "http://127.0.0.1:8888/search"   # SearXNG (the web_search tool)
//! music = "http://127.0.0.1:52713"
//! [memory]
//! notes = true                       # the note-taker
//! prewarm = true                     # the prompt into the cache of a model at home at start (and after new notes)
//! [web]                              # the web UI (web/mod.rs)
//! listen = "0.0.0.0:52702"           # "" — off (what the web UI shows works on)
//! ui = "web/dist"                    # the single-page app, as web/ builds it
//! tls = true                         # HTTPS; false — plain HTTP: behind a proxy with HTTPS, or on this machine only
//!                                    #   (signing in takes HTTPS or localhost)
//! cert = ""                          # "" — the server's own, made at the first start
//! key = ""
//! public_url = ""                    # the address people open, behind a proxy («https://home.example.org»)
//! trusted_proxies = []               # their addresses («10.0.0.5», «10.42.0.0/16»): X-Forwarded-For/-Proto from them only
//! password = true                    # signing in by password
//! [web.oidc]                         # signing in by an OIDC provider (web/oidc.rs); needs public_url (https)
//! issuer = ""                        # «https://auth.example.org»; "" — off
//! name = ""                          # the button's «Войти через …» ("" — the issuer's host)
//! client_id = ""
//! client_secret_file = ""            # the client's secret, alone in a file (600)
//! groups = []                        # who may: in one of these; ["*"] — anyone the provider lets in
//! scopes = ["profile", "groups"]     # besides openid
//! ca_file = ""                       # the provider's CA, if a home one (PEM)
//! ```
//! In `data` (made at the first start, private: 700): server.db (store.rs), memory.db (memory.rs), timers.json, the
//! token the stations present and tls/ with the certificate they pin (identity.rs), sounds/ (the user's own),
//! wakes/ (every wake recorded), fillers/ (ready phrases in the voice in use). A prompt.md there replaces the built-in
//! system prompt.
use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub data: String,
    pub listen: String,
    pub station_client: String,
    pub services: Services,
    pub memory: Memory,
    pub web: Web,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            data: "data".into(),
            listen: "0.0.0.0:52700".into(),
            station_client: "station/ranunculus-station".into(),
            services: Services::default(),
            memory: Memory::default(),
            web: Web::default(),
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Services {
    pub pages: String,
    pub search: String,
    pub music: String,
    /// The services of the first version (recognition, the voice, the memory's vectors), now roles chosen in the web
    /// UI: a server.toml of then still starts the server, and the log says they are not read.
    #[serde(rename = "asr")]
    pub old_asr: Option<serde::de::IgnoredAny>,
    #[serde(rename = "tts")]
    pub old_tts: Option<serde::de::IgnoredAny>,
    #[serde(rename = "embeddings")]
    pub old_embeddings: Option<serde::de::IgnoredAny>,
}

impl Services {
    /// Those of the first version still in the file.
    pub fn old(&self) -> Vec<&'static str> {
        [
            ("asr", self.old_asr.is_some()),
            ("tts", self.old_tts.is_some()),
            ("embeddings", self.old_embeddings.is_some()),
        ]
        .into_iter()
        .filter_map(|(k, there)| there.then_some(k))
        .collect()
    }
}

impl Default for Services {
    fn default() -> Services {
        Services {
            pages: "http://127.0.0.1:52712".into(),
            search: "http://127.0.0.1:8888/search".into(),
            music: "http://127.0.0.1:52713".into(),
            old_asr: None,
            old_tts: None,
            old_embeddings: None,
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Memory {
    pub notes: bool,
    pub prewarm: bool,
}

impl Default for Memory {
    fn default() -> Memory {
        Memory { notes: true, prewarm: true }
    }
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Web {
    pub listen: String,
    pub ui: String,
    /// HTTPS with `cert`/`key`; false: plain HTTP (behind a proxy that has HTTPS, or for this machine only)
    pub tls: bool,
    /// "": the server's own (`Config::parse` fills them in)
    pub cert: String,
    pub key: String,
    /// the address people open it by, behind a proxy: its host is a Host it may be opened by, its origin the page's
    pub public_url: String,
    /// the proxies whose X-Forwarded-For / X-Forwarded-Proto are believed: addresses or networks (a.b.c.d/n)
    pub trusted_proxies: Vec<String>,
    pub password: bool,
    pub oidc: Oidc,
}

impl Default for Web {
    fn default() -> Web {
        Web {
            listen: "0.0.0.0:52702".into(),
            ui: "web/dist".into(),
            tls: true,
            cert: String::new(),
            key: String::new(),
            public_url: String::new(),
            trusted_proxies: vec![],
            password: true,
            oidc: Oidc::default(),
        }
    }
}

/// `[web.oidc]`: signing in by an OIDC provider (web/oidc.rs).
#[derive(Deserialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct Oidc {
    /// "": off
    pub issuer: String,
    pub name: String,
    pub client_id: String,
    pub client_secret_file: String,
    pub groups: Vec<String>,
    pub scopes: Vec<String>,
    pub ca_file: String,
}

impl Default for Oidc {
    fn default() -> Oidc {
        Oidc {
            issuer: String::new(),
            name: String::new(),
            client_id: String::new(),
            client_secret_file: String::new(),
            groups: vec![],
            scopes: vec!["profile".into(), "groups".into()],
            ca_file: String::new(),
        }
    }
}

/// «~/x» → the home folder's x.
fn home(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => p.to_string(),
    }
}

/// The file read when none is named, if it is there.
pub const FILE: &str = "server.toml";

impl Config {
    /// `path`: the file named on the command line; None — `FILE` here if there is one, else the defaults.
    pub fn load(path: Option<&str>) -> Result<Config, String> {
        let text = match path {
            Some(p) => std::fs::read_to_string(home(p)).map_err(|e| format!("{p}: {e}"))?,
            None => match std::fs::read_to_string(FILE) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => return Err(format!("{FILE}: {e}")),
            },
        };
        Config::parse(&text).map_err(|e| format!("{}: {e}", path.unwrap_or(FILE)))
    }

    pub fn parse(text: &str) -> Result<Config, String> {
        let mut c: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        for p in &c.web.trusted_proxies {
            crate::web::Net::parse(p).map_err(|e| format!("[web] trusted_proxies: {e}"))?;
        }
        if !c.web.public_url.is_empty() {
            crate::web::public_origin(&c.web.public_url).map_err(|e| format!("[web] public_url: {e}"))?;
        }
        for p in [
            &mut c.data,
            &mut c.station_client,
            &mut c.web.ui,
            &mut c.web.cert,
            &mut c.web.key,
            &mut c.web.oidc.client_secret_file,
            &mut c.web.oidc.ca_file,
        ] {
            *p = home(p);
        }
        if c.web.cert.is_empty() != c.web.key.is_empty() {
            return Err("[web] cert and key: both or neither".into());
        }
        if c.web.cert.is_empty() {
            (c.web.cert, c.web.key) = (c.at(crate::identity::CERT), c.at(crate::identity::KEY));
        }
        Ok(c)
    }

    /// A path inside `data`.
    pub fn at(&self, name: &str) -> String {
        format!("{}/{name}", self.data.trim_end_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_what_a_file_changes() {
        let c = Config::parse("").unwrap();
        assert_eq!(
            (c.listen.as_str(), c.at("server.db").as_str(), c.web.cert.as_str()),
            ("0.0.0.0:52700", "data/server.db", "data/tls/server.crt")
        );
        assert!(c.memory.notes && c.web.tls);
        let c = Config::parse("data = \"/var/lib/ranunculus/\"\n[services]\nmusic = \"\"\n[web]\nlisten = \"\"\ncert = \"/x/a.crt\"\nkey = \"/x/a.key\"\n").unwrap();
        assert_eq!(
            (c.at("wakes").as_str(), c.services.music.as_str(), c.web.key.as_str()),
            ("/var/lib/ranunculus/wakes", "", "/x/a.key")
        );
        assert_eq!(c.services.pages, Services::default().pages, "what the file leaves out stays");
        // a typo is said, not passed over; and so is a service of an older version (the models are in the web UI now)
        assert!(Config::parse("[service]\nmusic = \"\"\n").unwrap_err().contains("unknown field"));
        // the first version's services: read past, and said
        let old =
            Config::parse("[services]\nasr = \"http://asr:8000\"\nembeddings = \"http://embeddings:8000\"\n").unwrap();
        assert_eq!(old.services.old(), ["asr", "embeddings"]);
        assert!(Config::parse("").unwrap().services.old().is_empty());
        assert!(Config::parse("[web]\ncert = \"/x/a.crt\"\n").is_err());
        assert!(Config::parse("[web]\ntrusted_proxies = [\"10.0.0.300\"]\n").is_err());
    }
}
