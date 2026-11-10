//! Ranunculus on a Yandex Station 2: the microphone, playback, the panel, the volume and the buttons, connected to
//! the server. A static aarch64 binary, /data/ranunculus/ranunculus-station.
//!
//!   ranunculus-station [run] [CONFIG]   the client (config: /data/ranunculus/station.toml)
//!   ranunculus-station install          set this station up: Alice off, autostart (setup.rs); then reboot
//!   ranunculus-station uninstall        put the station back as it was; then reboot
//!   ranunculus-station status           what is in place
//!   ranunculus-station start | stop     the client under its supervisor, in the background
//!   ranunculus-station boot             what the boot hook runs: our files over the firmware's, then start
//!   ranunculus-station version
//! For looking into a station:
//!   ranunculus-station mic-dump [SECONDS]        microphone levels per second (is the audio front end there?)
//!   ranunculus-station input-dump                raw input events, to name the buttons
//!   ranunculus-station volume                    the volume knob as it is, 0..255
mod config;
mod input;
mod kws;
mod link;
mod mic;
mod panel;
mod play;
mod setup;
mod update;
mod volume;

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

pub fn now_us() -> u64 {
    (panel::now_ns() / 1000) as u64
}

pub fn now_s() -> f64 {
    panel::now_ns() as f64 / 1e9
}

/// Wall-clock time for the log, UTC (the station's own logs are UTC too).
pub fn stamp() -> String {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let s = t.as_secs() % 86400;
    format!("{:02}:{:02}:{:02}.{:03}Z", s / 3600, s / 60 % 60, s % 60, t.subsec_millis())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = match args.get(1).map(|s| s.as_str()) {
        Some("version") => {
            println!("ranunculus-station {} (proto {})", ranunculus_proto::VERSION, ranunculus_proto::PROTO);
            0
        }
        Some("mic-dump") => mic_dump(args.get(2).and_then(|s| s.parse().ok()).unwrap_or(5)),
        Some("input-dump") => input_dump(),
        Some("volume") => volume_cmd(),
        Some("run") => run(args.get(2).map(|s| s.as_str()).unwrap_or(config::PATH)),
        None => run(config::PATH),
        Some(cmd @ ("install" | "uninstall" | "boot" | "start")) => {
            let station = setup::Station::real();
            let done = match cmd {
                "install" => station.install(),
                "uninstall" => station.uninstall(),
                "boot" => station.boot(),
                _ => station.start(),
            };
            done.map_or_else(
                |e| {
                    eprintln!("ranunculus-station {cmd}: {e}");
                    1
                },
                |()| 0,
            )
        }
        Some("stop") => {
            setup::Station::real().stop();
            0
        }
        Some("status") => i32::from(!setup::Station::real().status()),
        Some("supervise") => setup::Station::real().supervise(),
        Some(other) => {
            eprintln!("ranunculus-station: unknown command «{other}»; commands: run [CONFIG] | install | uninstall | status | start | stop | version");
            2
        }
    };
    std::process::exit(code);
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a tokio runtime")
}

fn run(path: &str) -> i32 {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cfg = match config::Config::load(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} station: {e}", stamp());
            return 2;
        }
    };
    eprintln!(
        "{} station {} starting: device {}, server {}",
        stamp(),
        ranunculus_proto::VERSION,
        cfg.device,
        cfg.server
    );
    let rt = runtime();
    rt.block_on(async move {
        let mut caps = vec!["mic".to_string(), "play".to_string()];
        let panel = match panel::Panel::start(cfg.idle_backlight) {
            Ok(p) => {
                caps.push("panel".into());
                Some(p)
            }
            Err(e) => {
                eprintln!("{} panel: none ({e})", stamp());
                None
            }
        };
        let (mic_tx, mic_rx) = mpsc::channel(64);
        tokio::spawn(mic::run(mic_tx, |s| eprintln!("{} {s}", stamp())));
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        for dev in input::DEVICES {
            if std::path::Path::new(dev).exists() {
                let tx = in_tx.clone();
                std::thread::Builder::new()
                    .name("input".into())
                    .spawn(move || {
                        input::watch(dev, move |i| {
                            let _ = tx.send(i);
                        })
                    })
                    .ok();
                if !caps.contains(&"buttons".to_string()) {
                    caps.push("buttons".into());
                }
            }
        }
        if let Some(on) = input::mute_now(input::DEVICES[0]) {
            let _ = in_tx.send(input::Input::Mute(on));
        }
        let (v_tx, v_rx) = mpsc::unbounded_channel();
        let knob = volume::Knob::open().ok();
        if knob.is_some() {
            caps.push("volume".into());
            std::thread::Builder::new()
                .name("volume".into())
                .spawn(move || {
                    volume::watch(move |l| {
                        let _ = v_tx.send(l);
                    })
                })
                .ok();
        }
        caps.push("kws".into());
        let spotter =
            kws::Spotter::new(std::sync::Arc::new(kws::Model::parse(WAKE_WORD).expect("the wake word model built in")));
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let mut hw = link::Hw {
            spotter,
            player: play::Player::new(ev_tx, panel.clone()),
            play_events: ev_rx,
            mic: mic_rx,
            inputs: in_rx,
            volume: v_rx,
            knob,
            panel: panel.clone(),
            caps,
        };
        // a stop leaves the panel blank with the idle backlight, not in the middle of a show
        if let Some(p) = panel.clone() {
            tokio::spawn(async move {
                use tokio::signal::unix::{signal, SignalKind};
                let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt()))
                else {
                    return;
                };
                tokio::select! { _ = term.recv() => {}, _ = int.recv() => {} }
                p.send(panel::Msg::Reset);
                tokio::time::sleep(Duration::from_millis(150)).await;
                eprintln!("{} station: stopped", stamp());
                std::process::exit(0);
            });
        }
        let mut backoff = 1u64;
        let mut said: Option<String> = None;
        loop {
            let t = Instant::now();
            let end = link::session(&cfg, &mut hw).await;
            hw.player.kill_all();
            if let Some(p) = &hw.panel {
                p.send(panel::Msg::Reset);
            }
            if t.elapsed() > Duration::from_secs(30) {
                backoff = 1;
            }
            match end {
                link::End::Lost(why) => {
                    if said.as_deref() != Some(why.as_str()) {
                        eprintln!("{} link: {why}; reconnecting", stamp());
                        said = Some(why);
                    }
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(5);
                }
                link::End::Refused(why) => {
                    // not every 1–5 s: it will not fix itself
                    eprintln!("{} link: the server refused us: {why}; trying again in 15 s", stamp());
                    said = None;
                    tokio::time::sleep(Duration::from_secs(15)).await;
                }
                link::End::Updated(version) => {
                    eprintln!("{} station: updated to {version}, ending for the new client to start", stamp());
                    tokio::time::sleep(Duration::from_millis(150)).await; // the panel's reset on its way
                    std::process::exit(0);
                }
            }
        }
    })
}

fn mic_dump(secs: u64) -> i32 {
    runtime().block_on(async move {
        let (tx, mut rx) = mpsc::channel(256);
        tokio::spawn(mic::run(tx, |s| eprintln!("{s}")));
        let end = Instant::now() + Duration::from_secs(secs);
        let (mut n, mut sum2, mut chunks, mut t) = (0u64, 0f64, 0u64, Instant::now());
        while Instant::now() < end {
            let Ok(Some(c)) = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await else {
                eprintln!("no audio from {} in 3 s", mic::ADDR);
                return 1;
            };
            chunks += 1;
            for s in c.pcm.as_chunks::<2>().0 {
                let v = i16::from_le_bytes(*s) as f64 / 32768.0;
                sum2 += v * v;
                n += 1;
            }
            if t.elapsed() >= Duration::from_secs(1) {
                let db = 10.0 * (sum2 / n.max(1) as f64 + 1e-12).log10();
                println!("{chunks} chunks, {n} samples ({:.0}/s), {db:.1} dBFS", n as f64 / t.elapsed().as_secs_f64());
                (n, sum2, chunks, t) = (0, 0.0, 0, Instant::now());
            }
        }
        0
    })
}

fn input_dump() -> i32 {
    for dev in input::DEVICES {
        println!("{dev}: mute switch {:?}", input::mute_now(dev));
        std::thread::spawn(move || {
            use std::io::Read;
            let Ok(mut f) = std::fs::File::open(dev) else { return };
            let mut b = [0u8; 24];
            while f.read_exact(&mut b).is_ok() {
                if let Some((typ, code, value)) = input::parse(&b) {
                    if typ != 0 {
                        println!("{} {dev}: type {typ} code {code} value {value}", stamp());
                    }
                }
            }
        });
    }
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// The wake word: «Мразота».
const WAKE_WORD: &[u8] = include_bytes!("../models/mrazota.rkw");

fn volume_cmd() -> i32 {
    match volume::Knob::open().and_then(|k| k.get()) {
        Ok(l) => {
            println!("{l}");
            0
        }
        Err(e) => {
            eprintln!("{}: {e}", volume::DEVICE);
            1
        }
    }
}
