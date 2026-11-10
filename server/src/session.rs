//! One station's session: the dialog on the station's wake word.
//! wake → panel «listen», a beep, the music ducked, listening (listen.rs) → `listen_end` → «think»: recognition → a
//! plain player command («пауза», «дальше») straight to the player, anything else to the dialog (brain.rs: history,
//! tools, guards), streamed → sentences → speech (speech.rs) → the voice on the station, «speak» → the panel rests
//! («music» while music plays, else off), the music back up. A web tool says a ready phrase («Ищу в интернете.») and
//! shows «search». A wake during the thinking or the answer is a barge-in: the running request stops writing and
//! saying, its stream is cut.
//! Buttons: the centre tapped — pause / resume; a slide over the ring — the next track (− → +) or the previous one
//! (+ → −); holding the centre is a wake (the station's own). While a timer rings (ring.rs), the wake or a touch
//! stops the ring and nothing else.
//! For the web UI (events.rs): the device on and off, its panel, volume; what was heard, the sentences as they go to
//! speech, the exchange once written to server.db's chats (store.rs).
//! The log says what happened and how long it took, never what was said.
use crate::brain::{Brain, Done};
use crate::events;
use crate::fillers::{self, Fillers};
use crate::listen::{Listener, Outcome};
use crate::music::{self, Ducked, Music, Why};
use crate::ring::{self, Ring, Stop};
use crate::services::beep;
use crate::settings::Settings;
use crate::speech::{self, Queue, Say};
use crate::store::Store;
use crate::text::{now_s, round_to, strip_wake, tz};
use crate::timers::{self, Timers};
use crate::tools::{Io, TimerHook, Tools};
use futures_util::{SinkExt, StreamExt};
use ranunculus_proto::{Down, Frame, Look, Up};
use ranunculus_vad::Silero;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

/// nothing from the station this long (it pings every 5 s): the link is dead — a station unplugged, Wi-Fi gone: TCP
/// says nothing for minutes, and rings and music would go into the void meanwhile
const SILENT: Duration = Duration::from_secs(15);
/// a write to the station that takes longer: the link is stuck
const SEND_FOR: Duration = Duration::from_secs(5);
/// said when the model fails to answer
const LOST: &str = "Что-то я задумалась и потеряла мысль. Повтори, пожалуйста.";
/// said until the user adds a model in the web UI
const NO_MODEL: &str = "Мне пока нечем отвечать: модель не выбрана. Добавь её в настройках.";
const KWS_THRESHOLD: f32 = 0.8;
const KWS_BARGE: f32 = 0.85; // while the answer plays: its echo's residual is in the microphone (0.95 is too strict for double talk)

pub struct Shared {
    pub brain: Arc<Brain>,
    pub fillers: Arc<Fillers>,
    /// the pages service (read_page); "": none
    pub pages: String,
    /// SearXNG; "": none
    pub search: String,
    /// the music service
    pub music: Option<String>,
    /// the station of each device now connected: timers speak on it
    pub stations: Mutex<HashMap<String, Station>>,
    /// each device's music player (it outlives a reconnect: the queue stays)
    pub players: Mutex<HashMap<String, Arc<Music>>>,
    /// where the players keep the listens
    pub listens: crate::listens::Writer,
    /// requests (and timer phrases) running: the note-taker waits
    pub active: AtomicUsize,
    pub timers: Arc<Timers>,
    /// the user's own sounds (what is chosen and for how long is in `settings`)
    pub sounds_dir: String,
    pub settings: Arc<Settings>,
    /// server.db: the chats, the devices, the alarms, the wakes
    pub store: Arc<Store>,
    /// where every wake is recorded (wakes.rs)
    pub wakes_dir: String,
    /// who answers, hears and speaks
    pub providers: Arc<crate::providers::Providers>,
    /// each device's ring sounding now (ring.rs)
    pub rings: Mutex<HashMap<String, Arc<Ring>>>,
    /// each device's dialog: listening, or a request on (a ring waits for it to end)
    pub dialogs: Mutex<HashMap<String, usize>>,
}

impl Shared {
    pub fn busy(&self) -> bool {
        self.active.load(Ordering::SeqCst) > 0
    }

    pub fn player(&self, device: &str) -> Option<Arc<Music>> {
        let service = self.music.as_ref()?;
        Some(
            self.players
                .lock()
                .unwrap()
                .entry(device.to_string())
                .or_insert_with(|| Music::new(service, device, Some(self.listens.clone())))
                .clone(),
        )
    }

    pub fn in_dialog(&self, device: &str) -> bool {
        self.dialogs.lock().unwrap().get(device).is_some_and(|&n| n > 0)
    }
}

/// A dialog on a device, while it lives: from the wake to the end of the answer — the music ducked all along (for
/// the listening, then for the answer).
pub struct InDialog(Arc<Shared>, String, Option<Ducked>);

impl InDialog {
    pub fn new(shared: &Arc<Shared>, device: &str, player: &Option<Arc<Music>>) -> InDialog {
        *shared.dialogs.lock().unwrap().entry(device.to_string()).or_insert(0) += 1;
        InDialog(shared.clone(), device.to_string(), player.as_ref().map(|p| p.duck(Why::Listen)))
    }

    /// The command heard: the music stays down, now for the answer.
    fn answering(mut self) -> InDialog {
        self.2 = self.2.take().map(|d| d.then(Why::Answer));
        self
    }
}

impl Drop for InDialog {
    fn drop(&mut self) {
        let mut d = self.0.dialogs.lock().unwrap();
        if let Some(n) = d.get_mut(&self.1) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                d.remove(&self.1);
            }
        }
    }
}

/// What the panel shows when nothing else does: the music's pulse while it plays, else nothing.
pub fn rest(player: &Option<Arc<Music>>) -> &'static str {
    if player.as_ref().is_some_and(|p| p.playing()) {
        "music"
    } else {
        "off"
    }
}

/// Lets go of the panel, if it shows one of `from` now: to the ring's «speak» while a ring sounds on the device (it
/// pulses with the ring), else to rest: an answer that ends under a ring must not set the rest over it, nor a stopped
/// ring's tail under the next ring. `dialog`: a dialog on the device keeps the panel as it is (it lets go of it at its
/// end). Each owner gives up its own hold first (the ring leaves `rings`, the request drops its
/// `InDialog`), and the check is under the panel's lock: of two ending at once, the second sees the first gone.
pub fn rest_panel(shared: &Shared, f: &Station, player: &Option<Arc<Music>>, from: &[&str], dialog: bool) {
    f.panel_unless(
        || if shared.rings.lock().unwrap().contains_key(&f.device) { "speak" } else { rest(player) }.to_string(),
        from,
        || dialog && shared.in_dialog(&f.device),
    );
}

/// A request or a ring on: the note-taker waits.
pub struct Active<'a>(&'a Shared);

impl<'a> Active<'a> {
    pub fn new(s: &'a Shared) -> Active<'a> {
        s.active.fetch_add(1, Ordering::SeqCst);
        Active(s)
    }
}

impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn log(device: &str, s: impl AsRef<str>) {
    let t = chrono::Utc::now().with_timezone(&tz());
    println!("{} [{device}] {}", t.format("%H:%M:%S"), s.as_ref());
}

/// A request's ticket: it is cancelled as soon as a newer one (a barge-in) is issued.
#[derive(Clone)]
pub struct Gen {
    cur: Arc<AtomicU64>,
    mine: u64,
}

impl Gen {
    pub fn cancelled(&self) -> bool {
        self.cur.load(Ordering::SeqCst) != self.mine
    }
}

/// What the dialog tasks, the player and the timers use to talk to the station.
#[derive(Clone)]
pub struct Station {
    out: mpsc::UnboundedSender<Message>,
    ids: Arc<AtomicU32>,
    waiters: Arc<Mutex<HashMap<u32, oneshot::Sender<bool>>>>,
    reqs: Arc<Mutex<HashMap<u32, oneshot::Sender<Result<u8, String>>>>>,
    panel_now: Arc<Mutex<String>>,
    self_name: Arc<AtomicBool>,
    /// the volume knob as last reported, 0..255; −1: not yet
    volume: Arc<AtomicI32>,
    pub device: String,
    sid: u64,
}

/// the voice goes this far ahead of real time: all that can still sound after a stop
const LEAD: f64 = 0.4;

impl Station {
    /// A station with no link: what is sent to it comes out of the receiver (the player's tests).
    #[cfg(test)]
    pub fn for_test(device: &str) -> (Station, mpsc::UnboundedReceiver<Message>) {
        let (out, rx) = mpsc::unbounded_channel();
        (
            Station {
                out,
                ids: Arc::new(AtomicU32::new(1)),
                waiters: Default::default(),
                reqs: Default::default(),
                panel_now: Default::default(),
                self_name: Default::default(),
                volume: Arc::new(AtomicI32::new(-1)),
                device: device.into(),
                sid: SESSIONS.fetch_add(1, Ordering::SeqCst),
            },
            rx,
        )
    }

    pub fn sid(&self) -> u64 {
        self.sid
    }

    /// What the panel shows now.
    pub fn panel_state(&self) -> String {
        self.panel_now.lock().unwrap().clone()
    }

    pub fn volume_known(&self) -> Option<u8> {
        u8::try_from(self.volume.load(Ordering::SeqCst)).ok()
    }

    /// The knob is at `level` (the station said so): for the web UI.
    fn note_volume(&self, level: u8) {
        if self.volume.swap(level as i32, Ordering::SeqCst) != level as i32 {
            events::emit("device", json!({"id": self.device, "volume": volume_json(level)}));
        }
    }

    pub fn send(&self, d: Down) {
        let _ = self.out.send(Message::Text(d.to_json().into()));
    }

    /// The state on the panel; sent only when it changes (the station crossfades on every change).
    pub fn panel(&self, state: &str) {
        let mut now = self.panel_now.lock().unwrap();
        self.panel_locked(&mut now, state);
    }

    /// The change and its message under the panel's lock: two changes at once reach the station in the order they
    /// were made.
    fn panel_locked(&self, now: &mut String, state: &str) {
        if *now == state {
            return;
        }
        *now = state.to_string();
        events::emit("device", json!({"id": self.device, "panel": state}));
        self.send(Down::Panel { state: state.into() });
    }

    /// The state, only if the panel shows one of `from` now (a resting state must not override «listen»). Checked and
    /// changed under one lock: read, let go and then set, it would override a «listen» shown in between.
    pub fn panel_if(&self, state: &str, from: &[&str]) {
        self.panel_unless(|| state.to_string(), from, || false);
    }

    /// `panel_if`, unless `held()` — someone else holds the panel; the state is made under the lock too (the rest
    /// follows the music, which may start meanwhile).
    pub fn panel_unless(&self, state: impl FnOnce() -> String, from: &[&str], held: impl FnOnce() -> bool) {
        let mut now = self.panel_now.lock().unwrap();
        if from.contains(&now.as_str()) && !held() {
            let s = state();
            self.panel_locked(&mut now, &s);
        }
    }

    /// A player one-shot over whatever is shown: next | previous | pause | continue.
    pub fn anim(&self, name: &str) {
        self.send(Down::Panel { state: name.into() });
    }

    /// The answer playing says «Мразота» itself: the station takes no wake from the voice meanwhile.
    pub fn set_self_name(&self, on: bool) {
        if self.self_name.swap(on, Ordering::SeqCst) != on {
            self.send(Down::Kws { threshold: KWS_THRESHOLD, barge: KWS_BARGE, self_name: on });
        }
    }

    fn done(&self, id: u32, killed: bool) {
        if let Some(w) = self.waiters.lock().unwrap().remove(&id) {
            let _ = w.send(killed);
        }
    }

    fn answer(&self, req: u32, r: Result<u8, String>) {
        if let Some(w) = self.reqs.lock().unwrap().remove(&req) {
            let _ = w.send(r);
        }
    }

    async fn ask_volume(&self, make: impl FnOnce(u32) -> Down) -> Result<u8, String> {
        let req = self.ids.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.reqs.lock().unwrap().insert(req, tx);
        self.send(make(req));
        let r = tokio::time::timeout(Duration::from_secs(3), rx).await;
        self.reqs.lock().unwrap().remove(&req);
        match r {
            Ok(Ok(Ok(level))) => {
                self.note_volume(level);
                Ok(level)
            }
            Ok(Ok(Err(e))) => Err(e),
            _ => Err("колонка не ответила".into()),
        }
    }

    /// The volume knob, 0..255, read now.
    pub async fn volume_get(&self) -> Result<u8, String> {
        self.ask_volume(|req| Down::VolumeGet { req }).await
    }

    /// → the knob as it is after setting it.
    pub async fn volume_set(&self, level: u8) -> Result<u8, String> {
        self.ask_volume(|req| Down::VolumeSet { level, req: Some(req) }).await
    }

    /// A new playback stream. → (its id, resolved with `killed` when the station reports it done)
    pub fn open(
        &self,
        rate: u32,
        channels: u8,
        music_lead: Option<f64>,
        buffer_ms: Option<u32>,
    ) -> (u32, oneshot::Receiver<bool>) {
        let id = self.ids.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.waiters.lock().unwrap().insert(id, tx);
        self.send(Down::Play { id, rate, channels, music_lead, buffer_ms });
        (id, rx)
    }

    /// PCM for stream `id`. → false: the station is gone.
    pub fn pcm(&self, id: u32, pcm: Vec<u8>) -> bool {
        self.out.send(Message::Binary(Frame::Pcm { id, pcm }.encode().into())).is_ok()
    }

    pub fn end(&self, id: u32) {
        self.send(Down::End { id });
    }

    pub fn kill(&self, id: u32) {
        self.send(Down::Kill { id });
    }

    /// The link to the station is closed.
    pub fn gone(&self) -> bool {
        self.out.is_closed()
    }

    /// Speech (s16le mono) as one stream, in 0.1-s blocks LEAD s ahead of real time, so a stop cuts it at once;
    /// returns when the station has played it (true) or it was cut (false).
    pub async fn play(&self, pcm: &[u8], rate: u32, g: Option<&Gen>) -> bool {
        let (id, mut rx) = self.open(rate, 1, None, None);
        let block = (rate as usize / 10) * 2;
        let t0 = Instant::now();
        let mut sent = 0usize;
        for b in pcm.chunks(block) {
            if g.is_some_and(|g| g.cancelled()) {
                self.kill(id);
                return false;
            }
            let ahead = sent as f64 / (2.0 * rate as f64) - t0.elapsed().as_secs_f64();
            if ahead > LEAD {
                tokio::time::sleep(Duration::from_secs_f64(ahead - LEAD)).await;
            }
            if !self.pcm(id, b.to_vec()) {
                return false;
            }
            sent += b.len();
        }
        self.end(id);
        loop {
            tokio::select! {
                r = &mut rx => return matches!(r, Ok(false)),
                _ = tokio::time::sleep(Duration::from_millis(20)) => {
                    if g.is_some_and(|g| g.cancelled()) {
                        self.kill(id);
                        return false;
                    }
                    if self.out.is_closed() {
                        return false;
                    }
                }
            }
        }
    }
}

/// The knob (0..255) for the web UI: raw, and as a level 0..10 (one level is 25.5 of the knob), to a half step.
pub fn volume_json(raw: u8) -> Value {
    json!({"raw": raw, "level": (raw as f64 / 12.75).round_ties_even() / 2.0})
}

static SESSIONS: AtomicU64 = AtomicU64::new(1);
/// Each request's number, for the web UI to put «heard», «said» and the exchange together.
static TURNS: AtomicU64 = AtomicU64::new(1);

/// One station's link till it ends: closed, broken, silent for SILENT, or `kick`ed — the same station connected again
/// (its old link is dead): every way out goes through the same clean-up below (aborted instead, a request speaking
/// then would wait forever, holding the dialog and «busy»).
pub async fn run<S>(
    ws: tokio_tungstenite::WebSocketStream<S>,
    device: String,
    caps: Vec<String>,
    shared: Arc<Shared>,
    kick: Arc<tokio::sync::Notify>,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut stream) = ws.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if !matches!(tokio::time::timeout(SEND_FOR, sink.send(m)).await, Ok(Ok(()))) {
                break;
            }
        }
        let _ = tokio::time::timeout(SEND_FOR, sink.close()).await;
    });
    let station = Station {
        out: out_tx,
        ids: Arc::new(AtomicU32::new(1)),
        waiters: Default::default(),
        reqs: Default::default(),
        panel_now: Default::default(),
        self_name: Default::default(),
        volume: Arc::new(AtomicI32::new(-1)),
        device: device.clone(),
        sid: SESSIONS.fetch_add(1, Ordering::SeqCst),
    };
    shared.stations.lock().unwrap().insert(device.clone(), station.clone());
    if let Err(e) = shared.store.seen(&device, now_s()) {
        log(&device, format!("   ({e})"));
    }
    {
        // the web UI shows the knob: read it once, the station reports every change after that
        let f = station.clone();
        tokio::spawn(async move { f.volume_get().await });
    }
    let player = shared.player(&device);
    if let Some(p) = &player {
        p.attach(station.clone());
    }
    if !caps.iter().any(|c| c == "kws") {
        log(&device, "the station has no wake word model: it will not hear its name");
    }
    station.send(Down::Kws { threshold: KWS_THRESHOLD, barge: KWS_BARGE, self_name: false });
    tokio::spawn(keep_look(station.clone()));
    // what holds the panel now: a ring that goes on here (ring.rs), the music resumed by `attach` above (its own «music»
    // came before the panel had a state, and was lost), or nothing
    station.panel(if shared.rings.lock().unwrap().contains_key(&device) { "speak" } else { rest(&player) });
    events::emit("device", crate::web::device(&shared, &device));
    let gen = Arc::new(AtomicU64::new(0));
    let mut vad: Option<Silero> = None;
    let mut listener: Option<Listener> = None;
    let mut hearing: Option<InDialog> = None;
    let mut ring_stopped = false; // this wake stopped a ring: «стоп» after it is about the ring
    let mut rec: Option<crate::wakes::Recording> = None; // this wake, recorded (wakes.rs)
    let mut t_wake = now_s();
    loop {
        let m = tokio::select! {
            m = tokio::time::timeout(SILENT, stream.next()) => match m {
                Err(_) => {
                    log(&device, format!("link lost: nothing for {} s", SILENT.as_secs()));
                    break;
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    log(&device, format!("link lost: {e}"));
                    break;
                }
                Ok(Some(Ok(m))) => m,
            },
            _ = kick.notified() => {
                log(&device, "a new link of this station: the old one is over");
                break;
            }
        };
        match m {
            Message::Text(t) => match serde_json::from_str::<Up>(&t) {
                Ok(Up::Wake { p, barge, pre, button, .. }) => {
                    let rang = shared.stop_ring(&device, Stop::Wake);
                    if listener.is_some() {
                        continue;
                    }
                    rec = Some(crate::wakes::Recording::new(p, barge, button, pre));
                    ring_stopped = rang;
                    log(
                        &device,
                        format!(
                            "wake word p={p:.2}{}",
                            if rang {
                                " (stopped the ring)"
                            } else if barge {
                                " (over the answer)"
                            } else if shared.busy() {
                                " (over the thinking)"
                            } else {
                                ""
                            }
                        ),
                    );
                    gen.fetch_add(1, Ordering::SeqCst); // whatever runs stops: a barge-in
                    station.set_self_name(false);
                    t_wake = now_s();
                    station.panel("listen");
                    let f = station.clone();
                    tokio::spawn(async move { f.play(&beep(24000), 24000, None).await });
                    listener = Some(Listener::new(vad.take().unwrap_or_else(Silero::bundled)));
                    hearing = Some(InDialog::new(&shared, &device, &player)); // the music ducked from here on
                }
                Ok(Up::Done { id, killed }) => station.done(id, killed),
                Ok(Up::Volume { level, req: Some(r) }) => station.answer(r, Ok(level)),
                Ok(Up::Error { req: Some(r), what }) => station.answer(r, Err(what)),
                Ok(Up::Button { name, action }) => button(&shared, &device, &player, &name, &action),
                Ok(Up::Mute { on }) => log(&device, format!("microphones {}", if on { "muted" } else { "on" })),
                Ok(Up::Volume { level, req: None }) => {
                    station.note_volume(level);
                    log(&device, format!("volume {level}"));
                }
                Ok(Up::Error { what, .. }) => log(&device, format!("station error: {what}")),
                Ok(_) => {}
                Err(e) => log(&device, format!("a message not understood ({e})")),
            },
            Message::Binary(b) => {
                let Ok(Frame::Mic { pcm, .. }) = Frame::decode(&b) else { continue };
                let Some(l) = listener.as_mut() else { continue };
                let s16: Vec<i16> = pcm.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c)).collect();
                // the first frame after a wake is the 2 s before it: recorded; the command gets its last 0.4 s
                let s16 = match rec.as_mut() {
                    Some(r) => r.push(&s16).to_vec(),
                    None => s16,
                };
                let Some(outcome) = l.push(&s16) else { continue };
                station.send(Down::ListenEnd);
                vad = listener.take().map(|l| l.into_vad());
                // written down on a blocking thread: the request starts now (wakes.rs)
                let wake = rec.take().map(|r| crate::wakes::save(&shared.store, &shared.wakes_dir, &device, r));
                match outcome {
                    Outcome::Silence => {
                        hearing = None; // the music up, unless a ring holds it
                        log(&device, "   (silence)");
                        if let Some(w) = wake {
                            w.heard("", shared.providers.services());
                        }
                        rest_panel(&shared, &station, &player, &["listen"], false);
                    }
                    Outcome::Command(audio) => {
                        station.panel("think");
                        let g = Gen { cur: gen.clone(), mine: gen.load(Ordering::SeqCst) };
                        let hold = hearing.take().map(InDialog::answering);
                        tokio::spawn(request(
                            audio,
                            t_wake,
                            station.clone(),
                            shared.clone(),
                            g,
                            ring_stopped,
                            hold,
                            wake,
                        ));
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    gen.fetch_add(1, Ordering::SeqCst); // the station is gone: everything stops
    {
        let mut fl = shared.stations.lock().unwrap();
        if fl.get(&device).is_some_and(|f| f.sid == station.sid) {
            fl.remove(&device);
        }
    }
    if let Some(p) = &player {
        p.detach(station.sid);
    }
    drop(station);
    writer.abort();
    events::emit("device", crate::web::device(&shared, &device)); // offline — unless a new link took over already
    log(&device, "session over");
}

/// How the panel looks: the colours, how it follows the voice and the music; at night by the home's clock (23–7) the
/// music's stars are slower and dimmer.
fn look() -> Look {
    let hour = chrono::Timelike::hour(&chrono::Utc::now().with_timezone(&tz()));
    Look {
        color: "#FF3000".into(),
        logo_color: "#FF3000".into(),
        logo: 0.85,
        gain: 1.0,
        gamma: 1.5,
        backlight: 70.0,
        react: 0.5,
        smooth: 0.6,
        speed: 1.0,
        music_react: 0.7,
        music_lag: 0.0,
        night: !(7..23).contains(&hour),
    }
}

/// The look sent to a station when it connects, and again when night comes or ends, for as long as its link lives.
async fn keep_look(station: Station) {
    let mut night = None;
    while !station.gone() {
        let l = look();
        if night != Some(l.night) {
            night = Some(l.night);
            station.send(Down::PanelLook(l));
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

/// The buttons — only while there is music to control. While a ring sounds, the centre (any press) or a slide stops
/// it, and that is all it does.
fn button(shared: &Shared, device: &str, player: &Option<Arc<Music>>, name: &str, action: &str) {
    if (name == "swipe" || (name == "activate" && action != "release")) && shared.stop_ring(device, Stop::Touch) {
        log(device, format!("   (button {name} {action}: the ring stopped)"));
        return;
    }
    let Some(p) = player.as_ref().filter(|p| p.has_queue()) else {
        log(device, format!("button {name} {action}"));
        return;
    };
    let act = match (name, action) {
        ("activate", "tap") => {
            if p.playing() {
                "pause"
            } else {
                "resume"
            }
        }
        ("swipe", "right") => "next",
        ("swipe", "left") => "previous",
        _ => {
            log(device, format!("button {name} {action}"));
            return;
        }
    };
    let r = p.control(act);
    log(
        device,
        format!("   (buttons: {act}, {})", if r["ok"] == json!(true) { "done" } else { "nothing to do it to" }),
    );
}

/// `ring_stopped`: the wake before it stopped a ring; `hold`: the dialog, from the wake on, till the answer is said (the
/// music under it); `wake`: the wake, being written down (wakes.rs).
#[allow(clippy::too_many_arguments)]
async fn request(
    audio: Vec<f32>,
    t_wake: f64,
    station: Station,
    shared: Arc<Shared>,
    g: Gen,
    ring_stopped: bool,
    hold: Option<InDialog>,
    wake: Option<crate::wakes::Saving>,
) {
    let _busy = Active::new(&shared);
    let dev = station.device.clone();
    let player = shared.player(&dev);
    let turn = TURNS.fetch_add(1, Ordering::SeqCst);
    let services = shared.providers.services();
    shared.fillers.prepare(&services);
    let t1 = now_s();
    let (raw, _) = match services.transcribe(&audio).await {
        Ok(r) => r,
        Err(e) => {
            log(&dev, format!("   ({e})"));
            (String::new(), 0.0)
        }
    };
    let text = strip_wake(&raw);
    events::emit("heard", json!({"device": dev, "turn": turn, "heard": raw, "text": text}));
    if let Some(w) = wake {
        w.heard(&raw, shared.providers.services()); // and the wake's window recognised only now, after the command
    }
    if g.cancelled() {
        journal(
            &shared,
            turn,
            json!({"device": dev, "heard": raw, "text": text, "calls": [], "answer": "", "cut": true,
                                      "asr_s": round_to(now_s() - t1, 2)}),
        );
        return;
    }
    if ring_stopped && (text.is_empty() || ring::is_stop(&text)) {
        // «Мразота, стоп» over a ring: the wake already stopped it; nothing more — not «Не расслышала», not the music
        log(&dev, "   (said over a ring: the ring is stopped, nothing more)");
        drop(hold); // the music up, unless another ring holds it
        rest_panel(&shared, &station, &player, &["think"], false);
        journal(
            &shared,
            turn,
            json!({"device": dev, "heard": raw, "text": text, "calls": [], "answer": "", "ring": "stopped",
                                      "asr_s": round_to(now_s() - t1, 2)}),
        );
        return;
    }
    let t2 = now_s();
    let searching = Arc::new(AtomicBool::new(false));
    let q = Arc::new(Queue::default());
    let speaker =
        tokio::spawn(speech::speak(q.clone(), station.clone(), services.clone(), g.clone(), t_wake, searching.clone()));
    let mut model = String::new();
    let action = player.as_ref().filter(|p| p.has_queue()).and_then(|_| music::spoken_command(&text));
    let say = |s: &str| events::emit("said", json!({"device": dev, "turn": turn, "text": s}));
    let (answer, calls): (String, Vec<Done>) = if text.is_empty() {
        let a = "Не расслышала.".to_string();
        say(&a);
        q.text(a.clone());
        (a, vec![])
    } else if let (Some(action), Some(p)) = (action, &player) {
        // «пауза», «дальше», «выключи»: straight to the player, no model; done silently
        let res = p.control(action);
        let ok = res["ok"] == json!(true);
        let answer = if ok {
            music::say(action).to_string()
        } else {
            "Сейчас ничего не играет.".to_string()
        };
        if !ok {
            say(&answer);
            q.text(answer.clone());
        }
        let calls = vec![("player_control".to_string(), json!({"action": action}), res)];
        shared.brain.remember(&text, &answer, &calls);
        (answer, calls)
    } else {
        let sh = shared.clone();
        let timers = TimerHook {
            timers: shared.timers.clone(),
            device: dev.clone(),
            start: Arc::new(move |t| timers::schedule(sh.clone(), t)),
        };
        let mut tools = Tools::new(
            Io {
                station: Some(station.clone()),
                timers: Some(timers),
                pages: shared.pages.clone(),
                search: shared.search.clone(),
                music: player.clone(),
                home: shared.settings.place(),
            },
            shared.brain.memory.clone(),
        );
        let gc = g.clone();
        let cancelled = move || gc.cancelled();
        let (qs, gs, fs, ds) = (q.clone(), g.clone(), station.clone(), dev.clone());
        let mut on_sentence = move |s: String| {
            if gs.cancelled() {
                return;
            }
            events::emit("said", json!({"device": ds, "turn": turn, "text": s}));
            static NAME: OnceLock<Regex> = OnceLock::new();
            if NAME.get_or_init(|| Regex::new("(?i)мразот").unwrap()).is_match(&s) {
                fs.set_self_name(true); // the assistant's own name in the answer: its echo must not be a barge-in
            }
            qs.text(s);
        };
        let (qt, gt, fl, ft, se) = (q.clone(), g.clone(), shared.fillers.clone(), station.clone(), searching.clone());
        let mut announced: HashSet<String> = HashSet::new();
        let mut on_tool = move |name: &str, args: &Value| {
            // a web tool starts: the «ищу» animation and a phrase (once per kind; not for «дальше» on a page already
            // read — it is cached, no wait)
            if gt.cancelled() || fillers::kind_of(name).is_none() || announced.contains(name) {
                return;
            }
            if name == "read_page" {
                let part = args
                    .get("part")
                    .filter(|v| !v.is_null() && **v != json!(0) && **v != json!(""))
                    .map(crate::tools::text_of)
                    .unwrap_or("1".into());
                if part != "1" {
                    return;
                }
            }
            announced.insert(name.to_string());
            se.store(true, Ordering::SeqCst);
            ft.panel("search");
            if let Some((phrase, clip)) = fl.pick(name, &services) {
                qt.put(match clip {
                    Some(c) => Say::Clip(c),
                    None => Say::Text { text: phrase, beeps: 0 },
                });
            }
        };
        let (r, sorry) = match shared.providers.llm() {
            Some(llm) => (
                shared.brain.ask(&llm, &text, &mut tools, &mut on_sentence, &cancelled, &mut on_tool, None, &dev).await,
                LOST,
            ),
            None => (Err("no model to answer: none is chosen in the web UI".to_string()), NO_MODEL),
        };
        searching.store(false, Ordering::SeqCst);
        match r {
            Ok(a) => {
                model = a.model;
                (a.text, a.calls)
            }
            Err(e) => {
                log(&dev, format!("   ({e})"));
                let a = sorry.to_string();
                if !g.cancelled() {
                    say(&a);
                    q.text(a.clone());
                }
                (a, vec![])
            }
        }
    };
    q.finish();
    let _ = speaker.await;
    // the answer is over: the wake word is heard again (left on till the next wake, the flag would make a timer after
    // an answer that said the name deaf to «Мразота, стоп»)
    station.set_self_name(false);
    if g.cancelled() {
        log(&dev, "   (interrupted: the answer was cut)");
        journal(
            &shared,
            turn,
            json!({"device": dev, "heard": raw, "text": text,
                                      "calls": calls.iter().map(|(n, a, r)| json!([n, a, r])).collect::<Vec<_>>(), "answer": answer, "cut": true,
                                      "asr_s": round_to(t2 - t1, 2)}),
        );
        return;
    }
    // the dialog over: the music up and the panel at rest — unless a ring sounds (it begins over an answer longer than
    // its WAIT); the dialog let go first, then the panel (rest_panel)
    drop(hold);
    rest_panel(&shared, &station, &player, &["think", "search", "speak"], false);
    let t3 = now_s();
    let tools: Vec<&str> = calls.iter().map(|(name, _, _)| name.as_str()).collect();
    log(
        &dev,
        format!(
            "   answered: recognition {:.2} s, the answer {:.2} s{}",
            t2 - t1,
            t3 - t2,
            if tools.is_empty() { String::new() } else { format!(", tools: {}", tools.join(", ")) }
        ),
    );
    journal(
        &shared,
        turn,
        json!({"device": dev, "heard": raw, "text": text, "model": (!model.is_empty()).then_some(&model),
                            "calls": calls.iter().map(|(n, a, r)| json!([n, a, r])).collect::<Vec<_>>(), "answer": answer,
                            "asr_s": round_to(t2 - t1, 2), "llm_s": round_to(t3 - t2, 2), "from_hit_s": round_to(t3 - t_wake, 2)}),
    );
}

/// One exchange into the chats (server.db), and to the web UI.
fn journal(shared: &Shared, turn: u64, line: Value) {
    let dev = line["device"].as_str().unwrap_or("server").to_string();
    match shared.store.add_exchange(&line, now_s()) {
        Ok((_, row)) => events::emit("exchange", json!({"device": dev, "turn": turn, "exchange": row})),
        Err(e) => log(&dev, format!("   ({e})")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station() -> (Station, mpsc::UnboundedReceiver<Message>) {
        let (out, rx) = mpsc::unbounded_channel();
        (
            Station {
                out,
                ids: Arc::new(AtomicU32::new(1)),
                waiters: Default::default(),
                reqs: Default::default(),
                panel_now: Default::default(),
                self_name: Default::default(),
                volume: Arc::new(AtomicI32::new(-1)),
                device: "d".into(),
                sid: 1,
            },
            rx,
        )
    }

    fn sent(rx: &mut mpsc::UnboundedReceiver<Message>) -> Vec<String> {
        let mut v = vec![];
        while let Ok(Message::Text(t)) = rx.try_recv() {
            if let Ok(Down::Panel { state }) = serde_json::from_str::<Down>(&t) {
                v.push(state);
            }
        }
        v
    }

    #[test]
    fn the_panel_let_go_of() {
        let (f, mut rx) = station();
        f.panel("speak"); // a ring
                          // a ring ends while a dialog is on: the dialog lets go of the panel itself
        f.panel_unless(|| "music".into(), &["speak"], || true);
        assert_eq!(f.panel_state(), "speak");
        // the dialog over, nothing holds it: at rest
        f.panel_unless(|| "music".into(), &["think", "search", "speak"], || false);
        assert_eq!(f.panel_state(), "music");
        // «listen» is not a resting panel's to take
        f.panel("listen");
        f.panel_if("off", &["think", "speak"]);
        assert_eq!(f.panel_state(), "listen");
        f.panel_if("listen", &["listen"]);
        assert_eq!(sent(&mut rx), ["speak", "music", "listen"], "each change once, in order");
    }
}
