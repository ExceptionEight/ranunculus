//! The link to the server: one WebSocket (wss), opened by the station, kept up forever.
//! TLS: a home server has a self-signed certificate, pinned by its SHA-256 (`server_cert_sha256`); without a pin the
//! usual CA check (a VPS with a real certificate).
//! A session: hello → welcome (or refuse: wrong version/token — then wait long, it will not fix itself) → messages
//! both ways until the socket drops or the server goes silent; then everything playing stops and the panel blanks.
use crate::config::Config;
use crate::input::Input;
use crate::panel::{self, Panel};
use crate::play::{Event, Player};
use crate::volume::Knob;
use crate::{kws, mic, stamp};
use futures_util::{SinkExt, StreamExt};
use ranunculus_proto::{Down, Frame, Up, PROTO, VERSION};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Everything a session works with; lives across reconnects.
pub struct Hw {
    pub player: Player,
    pub play_events: mpsc::UnboundedReceiver<Event>,
    pub mic: mpsc::Receiver<mic::Chunk>,
    pub inputs: mpsc::UnboundedReceiver<Input>,
    pub volume: mpsc::UnboundedReceiver<u8>,
    pub knob: Option<Knob>,
    pub panel: Option<Panel>,
    pub caps: Vec<String>,
    pub spotter: kws::Spotter,
}

/// The wake word's state within a session.
struct Ear {
    threshold: f32,
    barge: f32,
    self_name: bool,
    /// Streaming the command up since then (after a wake, until `listen_end`).
    listening: Option<Instant>,
    mute_until: Instant,
    /// The last WINDOW samples: sent with a wake — the word itself (the server records every wake, to tell its own from
    /// the TV's) and the start of the command that may overlap its end.
    preroll: std::collections::VecDeque<i16>,
}

/// The volume buttons are the client's (the firmware's handling of buttons is switched off, setup.rs): a tap is ± half
/// a level (the knob 0..255 in 20 halves of 12.75, as the firmware moved it), holding repeats it. Slides over the ring
/// come as `swipe` (input.rs) and go to the server (tracks).
const HALF: f64 = 12.75;
const REPEAT: Duration = Duration::from_millis(200);

fn volume_step(knob: &Knob, dir: i32) -> Option<u8> {
    let cur = knob.get().ok()? as f64;
    let halves = ((cur / HALF).round() as i32 + dir).clamp(0, 20);
    let new = (halves as f64 * HALF).round().clamp(0.0, 255.0) as u8;
    knob.set(new).ok()?;
    Some(new)
}

const PREROLL: usize = 32000; // 2 s at 16 kHz: the word and what came before it (the server gives the command its last 0.4 s)
const LISTEN_MAX: Duration = Duration::from_secs(15); // the server ends a command by 10 s; this is the safety net
const VOICE_TAIL: f64 = 0.3; // the residual of the voice's echo lingers this long after it stops

pub enum End {
    /// The socket dropped or the server went silent: reconnect soon.
    Lost(String),
    /// The server refused us: reconnecting will not help until someone fixes the token (or the server's client).
    Refused(String),
    /// The server is of another version and gave us its client: it is in this program's place — time to end.
    Updated(String),
}

#[derive(Debug)]
struct Pinned {
    sha256: Vec<u8>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _i: &[CertificateDer<'_>],
        _n: &ServerName<'_>,
        _o: &[u8],
        _t: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let d = ring::digest::digest(&ring::digest::SHA256, end_entity.as_ref());
        if d.as_ref() == self.sha256.as_slice() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("the server's certificate is not the pinned one".into()))
        }
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn connector(cfg: &Config) -> Option<tokio_tungstenite::Connector> {
    let pin = cfg.pin()?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("ring supports the default TLS versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { sha256: pin, provider }))
        .with_no_client_auth();
    Some(tokio_tungstenite::Connector::Rustls(Arc::new(tls)))
}

const SILENT: Duration = Duration::from_secs(15); // nothing from the server this long: the link is dead
const PING: Duration = Duration::from_secs(5);

pub async fn session(cfg: &Config, hw: &mut Hw) -> End {
    // short: Windows drops a connection to a closed port without a word, so a server that is down costs the timeout
    let r = tokio::time::timeout(
        Duration::from_secs(4),
        tokio_tungstenite::connect_async_tls_with_config(&cfg.server, None, true, connector(cfg)),
    )
    .await;
    let ws = match r {
        Err(_) => return End::Lost("connect: timed out".into()),
        Ok(Err(e)) => return End::Lost(format!("connect: {e}")),
        Ok(Ok((ws, _))) => ws,
    };
    let (mut tx, mut rx) = ws.split();
    let hello = Up::Hello {
        proto: PROTO,
        version: VERSION.into(),
        device: cfg.device.clone(),
        token: cfg.token.clone(),
        caps: hw.caps.clone(),
    };
    if let Err(e) = tx.send(Message::Text(hello.to_json().into())).await {
        return End::Lost(format!("hello: {e}"));
    }
    // the answer to the hello
    let first = tokio::time::timeout(Duration::from_secs(5), rx.next()).await;
    match first {
        Ok(Some(Ok(Message::Text(t)))) => match serde_json::from_str::<Down>(&t) {
            Ok(Down::Welcome { proto, version, session }) => {
                eprintln!("{} link: welcome from the server {version} (proto {proto}), session {session}", stamp());
            }
            Ok(Down::Refuse { reason }) => return End::Refused(reason),
            Ok(Down::Update { version, size, sha256 }) => {
                eprintln!("{} link: the server is {version}, this client {VERSION}: taking the server's", stamp());
                return match crate::update::take(&mut rx, &version, size, &sha256).await {
                    Ok(()) => End::Updated(version),
                    Err(e) => End::Refused(format!("the server's client {version} was not taken: {e}")),
                };
            }
            other => return End::Lost(format!("expected welcome, got {other:?}")),
        },
        Ok(Some(Ok(m))) => return End::Lost(format!("expected welcome, got {m:?}")),
        Ok(Some(Err(e))) => return End::Lost(format!("welcome: {e}")),
        Ok(None) => return End::Lost("closed before welcome".into()),
        Err(_) => return End::Lost("no welcome in 5 s".into()),
    }
    // stale input from before the session is not news
    while hw.mic.try_recv().is_ok() {}
    while hw.play_events.try_recv().is_ok() {}
    let mut seq = 0u32;
    let mut ear = Ear {
        threshold: 0.8,
        barge: 0.85,
        self_name: false,
        listening: None,
        mute_until: Instant::now(),
        preroll: std::collections::VecDeque::with_capacity(PREROLL + 1024),
    };
    hw.spotter.reset();
    let mut last_rx = Instant::now();
    let mut ping = tokio::time::interval(PING);
    let mut n_ping = 0u64;
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    let mut held: Option<(i32, Instant)> = None; // a volume button held: (direction, next repeat)
    let mut before: Option<u8> = None; // the volume before this gesture's presses (a slide puts it back)
    let mut level: Option<u8> = None; // the knob as last seen (its first report is no change)
    let end = loop {
        tokio::select! {
            m = rx.next() => {
                last_rx = Instant::now();
                match m {
                    None => break End::Lost("the server closed the link".into()),
                    Some(Err(e)) => break End::Lost(format!("read: {e}")),
                    Some(Ok(Message::Text(t))) => match serde_json::from_str::<Down>(&t) {
                        Ok(Down::Kws { threshold, barge, self_name }) => {
                            (ear.threshold, ear.barge, ear.self_name) = (threshold, barge, self_name);
                        }
                        Ok(Down::ListenEnd) => {
                            if ear.listening.take().is_some() {
                                hw.spotter.reset(); // the command is still in its memory: without this it fires again
                                ear.mute_until = Instant::now() + Duration::from_millis(300);
                            }
                        }
                        Ok(d) => {
                            if let Some(up) = handle(d, hw) {
                                if let Err(e) = tx.send(Message::Text(up.to_json().into())).await {
                                    break End::Lost(format!("write: {e}"));
                                }
                            }
                        }
                        Err(e) => eprintln!("{} link: a message not understood ({e}): {}", stamp(), t.chars().take(120).collect::<String>()),
                    },
                    Some(Ok(Message::Binary(b))) => match Frame::decode(&b) {
                        Ok(Frame::Pcm { id, pcm }) => hw.player.data(id, pcm),
                        Ok(Frame::Mic { .. }) => {}
                        Err(e) => eprintln!("{} link: {e}", stamp()),
                    },
                    Some(Ok(Message::Close(_))) => break End::Lost("the server closed the link".into()),
                    Some(Ok(_)) => {} // ping/pong: tungstenite answers pings itself
                }
            }
            c = hw.mic.recv() => {
                let Some(c) = c else { break End::Lost("mic: the reader is gone".into()) };
                let pcm16: Vec<i16> = c.pcm.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
                ear.preroll.extend(&pcm16);
                while ear.preroll.len() > PREROLL {
                    ear.preroll.pop_front();
                }
                if let Some(since) = ear.listening {
                    if since.elapsed() > LISTEN_MAX {
                        eprintln!("{} kws: no listen_end from the server in {} s — stopped listening", stamp(), LISTEN_MAX.as_secs());
                        ear.listening = None;
                        hw.spotter.reset();
                    }
                }
                let mut wake = None;
                if ear.listening.is_none() {
                    let mut pmax = 0f32;
                    hw.spotter.push(&pcm16, |p| pmax = pmax.max(p));
                    if Instant::now() >= ear.mute_until {
                        let playing = hw.player.voice_active(VOICE_TAIL);
                        let thr = if playing { ear.barge } else { ear.threshold };
                        if pmax >= thr && !(playing && ear.self_name) {
                            wake = Some((pmax, playing));
                        }
                    }
                }
                let mut out: Vec<Message> = vec![];
                if let Some((p, barge)) = wake {
                    eprintln!("{} kws: the wake word, p={p:.2}{}", stamp(), if barge { " over the answer" } else { "" });
                    out.push(Message::Text(Up::Wake { p, barge, t_us: c.t_us, pre: ear.preroll.len() as u32, button: false }.to_json().into()));
                    let pre: Vec<u8> = ear.preroll.iter().flat_map(|s| s.to_le_bytes()).collect();
                    seq = seq.wrapping_add(1);
                    out.push(Message::Binary(Frame::Mic { seq, t_us: c.t_us, pcm: pre }.encode().into()));
                    ear.listening = Some(Instant::now());
                } else if ear.listening.is_some() {
                    seq = seq.wrapping_add(1);
                    out.push(Message::Binary(Frame::Mic { seq, t_us: c.t_us, pcm: c.pcm }.encode().into()));
                }
                let mut failed = None;
                for m in out {
                    if let Err(e) = tx.send(m).await {
                        failed = Some(e);
                        break;
                    }
                }
                if let Some(e) = failed {
                    break End::Lost(format!("write: {e}"));
                }
            }
            e = hw.play_events.recv() => {
                let Some(Event::Done(id, killed)) = e else { continue };
                hw.player.finished(id);
                if let Err(e) = tx.send(Message::Text(Up::Done { id, killed }.to_json().into())).await {
                    break End::Lost(format!("write: {e}"));
                }
            }
            i = hw.inputs.recv() => {
                // holding the centre button is the wake word without words: listening starts here
                if let Some(Input::Button { name, action: "hold" }) = &i {
                    if name == "activate" && ear.listening.is_none() {
                        let barge = hw.player.voice_active(VOICE_TAIL);
                        eprintln!("{} kws: the centre button held — listening", stamp());
                        let pre: Vec<u8> = ear.preroll.iter().flat_map(|s| s.to_le_bytes()).collect();
                        seq = seq.wrapping_add(1);
                        let n = ear.preroll.len() as u32;
                        let msgs = [Message::Text(Up::Wake { p: 1.0, barge, t_us: crate::now_us(), pre: n, button: true }.to_json().into()),
                                    Message::Binary(Frame::Mic { seq, t_us: crate::now_us(), pcm: pre }.encode().into())];
                        let mut failed = None;
                        for m in msgs {
                            if let Err(e) = tx.send(m).await {
                                failed = Some(e);
                                break;
                            }
                        }
                        if let Some(e) = failed {
                            break End::Lost(format!("write: {e}"));
                        }
                        ear.listening = Some(Instant::now());
                    }
                }
                // the volume buttons: handled here
                if let Some(Input::Button { name, action }) = &i {
                    let dir = match name.as_str() { "volume_up" => 1, "volume_down" => -1, _ => 0 };
                    if dir != 0 {
                        match *action {
                            "press" => {
                                if let Some(k) = hw.knob.as_ref() {
                                    if before.is_none() {
                                        before = k.get().ok();
                                    }
                                    volume_step(k, dir);
                                }
                            }
                            "hold" => held = Some((dir, Instant::now() + REPEAT)),
                            _ => {
                                // tap / release: the gesture is over and the steps stay
                                held = None;
                                before = None;
                            }
                        }
                        continue;
                    }
                    if name == "swipe" {
                        // a slide passed over a volume key: undo what its press did
                        if let (Some(b), Some(k)) = (before.take(), hw.knob.as_ref()) {
                            let _ = k.set(b);
                        }
                        held = None;
                    }
                }
                let up = match i {
                    Some(Input::Button { name, action }) => Up::Button { name, action: action.into() },
                    Some(Input::Mute(on)) => Up::Mute { on },
                    None => continue,
                };
                if let Err(e) = tx.send(Message::Text(up.to_json().into())).await {
                    break End::Lost(format!("write: {e}"));
                }
            }
            v = hw.volume.recv() => {
                let Some(l) = v else { continue };
                // the volume ring on the panel, for every change (buttons, voice, the server); not for the first report
                if level.is_some_and(|old| old != l) {
                    if let Some(p) = &hw.panel {
                        p.send(panel::Msg::Volume(l));
                    }
                }
                level = Some(l);
                let level = l;
                if let Err(e) = tx.send(Message::Text(Up::Volume { level, req: None }.to_json().into())).await {
                    break End::Lost(format!("write: {e}"));
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                if let Some((dir, next)) = held {
                    if now >= next {
                        if let Some(k) = hw.knob.as_ref() {
                            volume_step(k, dir);
                        }
                        held = Some((dir, now + REPEAT));
                    }
                }
            }
            _ = ping.tick() => {
                if last_rx.elapsed() > SILENT {
                    break End::Lost(format!("the server was silent for {} s", SILENT.as_secs()));
                }
                n_ping += 1;
                if let Err(e) = tx.send(Message::Ping(n_ping.to_le_bytes().to_vec().into())).await {
                    break End::Lost(format!("write: {e}"));
                }
            }
        }
    };
    let _ = tx.close().await;
    end
}

/// One message from the server; the answer to send back, if any.
fn handle(d: Down, hw: &mut Hw) -> Option<Up> {
    match d {
        Down::Play { id, rate, channels, music_lead, buffer_ms } => {
            match hw.player.play(id, rate, channels, music_lead, buffer_ms) {
                Ok(()) => None,
                Err(what) => Some(Up::Error { req: None, what }),
            }
        }
        Down::End { id } => {
            hw.player.end(id);
            None
        }
        Down::Kill { id } => {
            hw.player.kill(id);
            None
        }
        Down::VolumeGet { req } => Some(match hw.knob.as_ref().map(|k| k.get()) {
            Some(Ok(level)) => Up::Volume { level, req: Some(req) },
            Some(Err(e)) => Up::Error { req: Some(req), what: format!("volume: {e}") },
            None => Up::Error { req: Some(req), what: "volume: no control device".into() },
        }),
        Down::VolumeSet { level, req } => Some(match hw.knob.as_ref().map(|k| k.set(level).and_then(|_| k.get())) {
            Some(Ok(level)) => Up::Volume { level, req },
            Some(Err(e)) => Up::Error { req, what: format!("volume: {e}") },
            None => Up::Error { req, what: "volume: no control device".into() },
        }),
        Down::Panel { state } if ["next", "previous", "pause", "continue"].contains(&state.as_str()) => {
            if let Some(p) = &hw.panel {
                p.send(panel::Msg::Anim(state));
            }
            None
        }
        Down::Panel { state } => match (panel::states::State::parse(&state), &hw.panel) {
            (Some(s), Some(p)) => {
                p.send(panel::Msg::State(s));
                None
            }
            (None, _) => Some(Up::Error { req: None, what: format!("panel: no state «{state}»") }),
            _ => None,
        },
        Down::PanelLook(l) => {
            let rgb = |s: &str, d: [f64; 3]| ranunculus_proto::hex_rgb(s).map(|c| c.map(|x| x as f64)).unwrap_or(d);
            let look = panel::states::Look {
                color: rgb(&l.color, [255.0, 48.0, 0.0]),
                logo_color: rgb(&l.logo_color, [255.0, 48.0, 0.0]),
                logo: l.logo as f64,
                gain: l.gain as f64,
                gamma: (l.gamma as f64).max(0.2),
                backlight: l.backlight as f64,
                react: l.react as f64,
                smooth: l.smooth as f64,
                speed: l.speed as f64,
                music_react: l.music_react as f64,
                music_lag: l.music_lag as f64,
                night: l.night,
            };
            hw.player.smooth = look.smooth;
            if let Some(p) = &hw.panel {
                p.send(panel::Msg::Look(look));
            }
            None
        }
        Down::Welcome { .. } | Down::Refuse { .. } | Down::Update { .. } | Down::Kws { .. } | Down::ListenEnd => None,
    }
}
