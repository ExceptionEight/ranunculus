//! The server of Ranunculus: the stations connect to it; it runs the dialog with the model and the services, keeps
//! the memory, the alarms and the music, and serves the web UI.
//!
//!   ranunculus-server [run] [CONFIG]            CONFIG: config.rs (default: server.toml here, if there is one)
//!   ranunculus-server password NAME [CONFIG]    a web UI user: made, or given a new password (then signed out everywhere)
//!   ranunculus-server users [CONFIG]            the web UI's users
//!   ranunculus-server remove-user NAME [CONFIG]
//!   ranunculus-server station-config ADDRESS [CONFIG]   a station.toml for a station that reaches this server at
//!                                               ADDRESS: where it is, its token, its certificate's SHA-256
//!   ranunculus-server version
mod alarms;
mod brain;
mod config;
mod events;
mod fillers;
mod identity;
mod listen;
mod listener;
mod listens;
mod llm;
mod loudness;
mod memory;
mod music;
mod notes;
mod providers;
mod ring;
mod services;
mod session;
mod settings;
mod sounds;
mod speech;
mod store;
mod text;
mod timers;
mod tools;
mod wakes;
mod web;

use std::sync::Arc;

/// In the data folder (config.rs).
const DB: &str = "server.db";

/// Every HTTP client of the server starts here: reqwest's TLS is rustls on ring, the crypto the stations' link and
/// the web UI already use (reqwest is built without a provider of its own).
pub fn http() -> reqwest::ClientBuilder {
    static RING: std::sync::Once = std::sync::Once::new();
    RING.call_once(|| {
        let _ = tokio_rustls::rustls::crypto::ring::default_provider().install_default();
    });
    reqwest::Client::builder()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("version") => {
            println!("ranunculus-server {} (proto {})", ranunculus_proto::VERSION, ranunculus_proto::PROTO);
            return;
        }
        Some("password" | "users" | "remove-user") => std::process::exit(users(&args)),
        Some("station-config") => std::process::exit(station_config(&args)),
        Some("run") | None => {}
        Some(other) => {
            eprintln!("ranunculus-server: unknown command «{other}»: run [CONFIG] | password NAME [CONFIG] | users [CONFIG] | remove-user NAME [CONFIG] | station-config ADDRESS [CONFIG] | version");
            std::process::exit(2);
        }
    }
    let cfg = match config::Config::load(args.get(2).map(String::as_str)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ranunculus-server: {e}");
            std::process::exit(2);
        }
    };
    let rt =
        tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().expect("a tokio runtime");
    if let Err(e) = rt.block_on(start(cfg)) {
        eprintln!("ranunculus-server: {e}");
        std::process::exit(1);
    }
}

/// `station-config ADDRESS`: what a station needs to reach this server and know it, as its station.toml has it
/// (station/install.sh adds the station's own name). The token and the certificate are made if this is the first
/// thing ever run here. → the exit code
fn station_config(args: &[String]) -> i32 {
    let Some(address) = args.get(2).filter(|a| !a.is_empty() && !a.contains(['/', ' ', '"'])) else {
        eprintln!("ranunculus-server station-config ADDRESS [CONFIG]   ADDRESS: this machine's name or IP address in the home network");
        return 2;
    };
    let made = config::Config::load(args.get(3).map(String::as_str)).and_then(|cfg| {
        let me = identity::ensure(&cfg)?;
        let port = cfg.listen.rsplit(':').next().unwrap_or("52700").to_string();
        Ok((port, identity::fingerprint(&me.cert)?, me.token))
    });
    match made {
        Ok((port, sha256, token)) => {
            println!(
                "server = \"wss://{address}:{port}{}\"\ntoken = \"{token}\"\nserver_cert_sha256 = \"{sha256}\"",
                ranunculus_proto::PATH
            );
            0
        }
        Err(e) => {
            eprintln!("ranunculus-server: {e}");
            1
        }
    }
}

/// `password NAME`, `users`, `remove-user NAME`: the web UI's users, in server.db (web/auth.rs), for whoever can run
/// this on the server: a forgotten password, a user to remove. → the exit code
fn users(args: &[String]) -> i32 {
    let cmd = args[1].as_str();
    let (name, conf) = if cmd == "users" { (None, args.get(2)) } else { (args.get(2), args.get(3)) };
    if cmd != "users" && name.is_none() {
        eprintln!("ranunculus-server {cmd} NAME [CONFIG]");
        return 2;
    }
    let cfg = match config::Config::load(conf.map(String::as_str)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ranunculus-server: {e}");
            return 2;
        }
    };
    let s = match store::Store::open(&cfg.at(DB)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ranunculus-server: {e}");
            return 1;
        }
    };
    let when = |t: f64| {
        chrono::DateTime::from_timestamp(t as i64, 0)
            .map(|d| d.with_timezone(&text::tz()).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    };
    let done = match cmd {
        "users" => s.users().map(|us| {
            if us.is_empty() {
                println!("No users yet: ranunculus-server password NAME");
            }
            for u in us {
                match &u.via {
                    Some(issuer) => println!(
                        "{}\tsince {}, signs in by {issuer}, sessions open: {}",
                        u.name,
                        when(u.created),
                        u.sessions
                    ),
                    None => println!(
                        "{}\tsince {}, password of {}, sessions open: {}",
                        u.name,
                        when(u.created),
                        when(u.changed),
                        u.sessions
                    ),
                }
            }
        }),
        "remove-user" => s.remove_user(name.unwrap()).map(|was| match was {
            Some(n) => println!("«{n}» is removed and signed out everywhere"),
            None => println!("no user «{}»", name.unwrap()),
        }),
        _ => {
            let name = match web::name_ok(name.unwrap()) {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("ranunculus-server: {e}");
                    return 2;
                }
            };
            let pw = match read_password() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("ranunculus-server: {e}");
                    return 2;
                }
            };
            if let Err(e) = web::password_ok(&pw) {
                eprintln!("ranunculus-server: {e}");
                return 2;
            }
            web::hash_password(&pw).and_then(|h| s.set_password(&name, &h)).map(|made| match made {
                true => println!("«{name}» is made and can sign in to the web UI"),
                false => println!("the password of «{name}» is changed; they are signed out everywhere"),
            })
        }
    };
    match done {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("ranunculus-server: {e}");
            1
        }
    }
}

/// From the terminal without echo, twice; from a pipe (scripts, tests): its first line.
fn read_password() -> Result<String, String> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        let a = rpassword::prompt_password(format!("Password (at least {} characters): ", web::PASSWORD_MIN))
            .map_err(|e| e.to_string())?;
        let b = rpassword::prompt_password("Once more: ").map_err(|e| e.to_string())?;
        if a != b {
            return Err("the two passwords differ".into());
        }
        Ok(a)
    } else {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
        Ok(line.trim_end_matches(['\n', '\r']).to_string())
    }
}

async fn start(cfg: config::Config) -> Result<(), String> {
    // the machine's clocks until the settings say whose the home's are
    text::set_tz(text::system_tz());
    let me = identity::ensure(&cfg)?;
    let r = &cfg.services;
    let mem = memory::Memory::open(&cfg.at("memory.db"), (!r.embeddings.is_empty()).then_some(r.embeddings.as_str()))?;
    let brain = Arc::new(brain::Brain::new(Some(mem.clone()), &cfg.at("prompt.md"))?);
    let store = open_store(&cfg)?;
    // what was said and heard is private: the recordings and the user's sounds as the databases
    let (sounds_dir, wakes_dir) = (cfg.at("sounds"), cfg.at("wakes"));
    identity::private_dir(&sounds_dir)?;
    identity::private_dir(&wakes_dir)?;
    // the settings first: the log's times are the home's from here on
    let settings = settings::Settings::load(store.clone(), &sounds_dir);
    session::log(
        "server",
        format!(
            "memory: {} exchanges, {} of them in the prompt, {} notes",
            mem.count(),
            brain.turns(),
            mem.facts().len()
        ),
    );
    let providers = providers::Providers::load(store.clone(), r);
    let fillers = fillers::Fillers::new(&cfg.at("fillers"));
    fillers.prepare(&providers.services());
    brain.set_profile(settings.profile());
    let shared = Arc::new(session::Shared {
        brain: brain.clone(),
        fillers,
        pages: r.pages.trim_end_matches('/').to_string(),
        search: r.search.clone(),
        music: (!r.music.is_empty()).then(|| r.music.clone()),
        players: Default::default(),
        // the music's listens: kept by their own writer thread (listens.rs)
        listens: listens::Writer::start(store.clone()),
        stations: Default::default(),
        active: Default::default(),
        timers: timers::Timers::load(Some(&cfg.at("timers.json"))),
        sounds_dir,
        settings,
        store: store.clone(),
        providers,
        wakes_dir,
        rings: Default::default(),
        dialogs: Default::default(),
    });
    timers::resume(&shared);
    tokio::spawn(alarms::run(shared.clone()));
    match shared.providers.llm() {
        Some(llm) if cfg.memory.prewarm => {
            let b = brain.clone();
            tokio::spawn(async move {
                if let Some(s) = b.prewarm(&llm).await {
                    session::log("server", format!("   (the prompt warmed up in {s:.1} s)"));
                }
            });
        }
        Some(_) => {}
        None => session::log("server", "no model to answer yet: add one in the web UI, in the settings"),
    }
    if cfg.memory.notes {
        tokio::spawn(notes::run(shared.clone()));
    }
    if cfg.web.listen.is_empty() {
        session::log("server", "the web UI is off ([web] listen is empty)");
    } else {
        tokio::spawn(web::serve(cfg.web.clone(), shared.clone(), store, cfg.memory.prewarm));
    }
    listener::serve(&cfg.listen, me, cfg.station_client.clone(), shared).await
}

/// server.db and what is put right in it at every start.
fn open_store(cfg: &config::Config) -> Result<Arc<store::Store>, String> {
    let s = store::Store::open(&cfg.at(DB))?;
    // the music listened to: what a server that stopped left open; the songs keyed by today's rule
    match s.close_lost_listens() {
        Ok(0) => {}
        Ok(n) => session::log("server", format!("music: {n} listens were cut short by the server's last stop")),
        Err(e) => session::log("server", format!("music: {e}")),
    }
    match s.resong() {
        Ok(Some(n)) if n > 0 => session::log(
            "server",
            format!("music: songs told apart by the new rule ({}): {n} tracks", listens::SONG_RULE),
        ),
        Ok(_) => {}
        Err(e) => session::log("server", format!("music: {e}")),
    }
    Ok(s)
}
