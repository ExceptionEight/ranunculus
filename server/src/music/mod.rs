//! The music player of one station: the queue the music service found, played on the station as a music stream
//! (48 kHz stereo; the station draws the panel's pulse from it), controls, ducking under the voice (duck.rs), what
//! plays now. The service (services/music) finds the music and gives each track's audio, already decoded and repaired
//! where the source broke; this side paces it (stream.rs), sets its level and follows the controls.
//!
//! - Sent LEAD s ahead of real time: a pause or the next track cuts it within ~0.3 s; ducking glides in ~0.4 s.
//! - Pause remembers where the listener is (what was heard, not what was sent) and resumes there.
//! - The station gone (a network blip): pause — the queue stays, «продолжи» goes on. The music service gone (restarting,
//!   stopped) for longer than DOWN_FOR: the same. A track it has no audio for: the next one.
//! - «Дальше» on the last track, a seek past its end: the queue has played out (stopped). «Назад» on the first: it again.
//! - The panel: «music» while it plays and nothing else is shown; the controls' one-shots (next, previous, pause,
//!   continue).
//! - Plain spoken commands («пауза», «дальше», «выключи музыку») go straight to the player, past the model
//!   (commands.rs): a model may answer «Выключила» without calling the tool, and the music plays on.
//! - Seek: the track again from another second (as after a pause); past its end — the next one. The repeat: the track
//!   on now, again and again, till «дальше», another request or «стоп».
//! - Every listen is kept (listens.rs): how long it sounded, how far it got, how it ended — as the listener heard it.
//!   It begins with the track's first sound: a track that never sounded is no listen.
//!
//! The log names no tracks.
mod commands;
mod duck;
mod stream;
#[cfg(test)]
mod tests;

pub use commands::{say, spoken_command};
pub use duck::{Ducked, Why};

use crate::listens;
use crate::session::{log, Station};
use crate::text::now_s;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

const SR: usize = 48000;
const CH: usize = 2;
const FRAMES: usize = 1024; // per block (~21 ms)
const LEAD: f64 = 0.3; // s of audio sent ahead of real time
const HEARD_LAG: f64 = 0.05; // the station stamps the music for the panel: heard LEAD + this after it is sent
const GAIN: f32 = 0.8; // the music as it plays (the voice must stay on top)
const DUCK: f32 = 0.2; // under the listening, an answer, a ring
/// a track's stream from the service broke this many times: the track is given up (the next one)
const TRIES: usize = 3;
/// the music service unreachable (refused, no answer) this long: the music pauses where it is, the queue stays. A
/// restart of the service takes a few seconds and is ridden out: taken for broken tracks, it would burn the whole
/// queue in seconds
const DOWN_FOR: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 10 });
/// between tries while it is unreachable
const DOWN_EVERY: Duration = Duration::from_millis(if cfg!(test) { 300 } else { 1000 });
/// a station back within this after its link was lost: the music it was playing goes on
const BACK_IN: Duration = Duration::from_secs(60);
/// no audio from the service this long while a track plays: the stream is broken
const READ_FOR: Duration = Duration::from_secs(30);
/// a stream that ends this far before the track's end did not end, it broke (the service only closes the connection
/// when it gives up, which looks like the track's end; the service itself tells a break from the end by the same
/// margin)
const SHORT_BY: f64 = 5.0;

#[derive(Clone, Debug)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    /// s: the service's catalogue's, then the audio's own (X-Duration)
    pub duration: Option<f64>,
    pub album: Option<String>,
    /// the recording's ISRC, where the service gives one (listens.rs: the same song across services)
    pub isrc: Option<String>,
}

/// A seek's «назад» / «вперёд» with no number, s
pub const SEEK_STEP: f64 = 15.0;

/// The track on now, as it is listened to (listens.rs): what of it sounded in the stretches before this one.
struct Listen {
    id: i64,
    track: Track,
    t: f64,
    heard: f64,
    reached: f64,
}

impl Track {
    pub fn name(&self) -> String {
        format!("{} — {}", self.title, self.artist)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum State {
    Stop,
    Play,
    Pause,
}

impl State {
    fn word(self) -> &'static str {
        match self {
            State::Stop => "stop",
            State::Play => "play",
            State::Pause => "pause",
        }
    }
}

struct St {
    queue: Vec<Track>,
    pos: usize,
    state: State,
    offset: f64, // s into the current track to start from (after a pause)
    stream: Option<u32>,
    // the stream on now: where it started, when, how much was sent — what the listener heard so far
    start: f64,
    t0: Option<Instant>,
    sent: f64,
    /// the track on now again when it ends
    repeat: bool,
    listen: Option<Listen>,
}

pub struct Music {
    service: String,
    http: reqwest::Client,
    device: String,
    station: Mutex<Option<Station>>,
    st: Mutex<St>,
    gen: AtomicU64, // bumped on every change: the running track's task sees it and quits
    changed: Notify,
    /// why it is ducked now, a count by `Why`: each owner holds its own (`Ducked`), the music is up when none is left
    ducks: Mutex<[u32; 3]>,
    gain: Mutex<f32>,
    /// when the link's loss paused the music (attach resumes it if the station is back soon)
    lost_at: Mutex<Option<Instant>>,
    /// where the listens go (None: no server.db — not kept)
    listens: Option<listens::Writer>,
}

fn heard(st: &St) -> f64 {
    match st.t0 {
        Some(t0) => st.start + st.sent.min(t0.elapsed().as_secs_f64()).max(0.0),
        None => st.offset,
    }
}

/// A stream's stretch is over (a change, its end, a break): what sounded of it goes to the listen, and where the
/// listener is stays in `offset` (to go on from). Twice is once.
fn close_stretch(st: &mut St) {
    if st.t0.is_none() {
        return;
    }
    let at = heard(st);
    if let Some(l) = st.listen.as_mut() {
        l.heard += (at - st.start).max(0.0);
        l.reached = l.reached.max(at);
    }
    st.offset = at;
    st.start = at;
    st.t0 = None;
}

impl Music {
    pub fn new(service: &str, device: &str, listens: Option<listens::Writer>) -> Arc<Music> {
        // no limit on a whole request: a mix plays for hours; a service that stops sending is given up after READ_FOR
        let http = crate::http()
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(READ_FOR)
            .build()
            .expect("an HTTP client");
        Arc::new(Music {
            service: service.trim_end_matches('/').to_string(),
            http,
            device: device.to_string(),
            station: Mutex::new(None),
            st: Mutex::new(St {
                queue: vec![],
                pos: 0,
                state: State::Stop,
                offset: 0.0,
                stream: None,
                start: 0.0,
                t0: None,
                sent: 0.0,
                repeat: false,
                listen: None,
            }),
            gen: AtomicU64::new(0),
            changed: Notify::new(),
            ducks: Mutex::new([0; 3]),
            gain: Mutex::new(GAIN),
            lost_at: Mutex::new(None),
            listens,
        })
    }

    /// The device's station (re)connected. Music its link took with it goes on, from where the listener was: playing on
    /// the old link (the station came back before the server saw it gone), or paused by the loss less than BACK_IN ago
    /// (a Wi-Fi blip; the old link's clean-up came first). After longer the music stays paused: it must not start by
    /// itself an hour later.
    pub fn attach(self: &Arc<Self>, f: Station) {
        let moved = {
            let mut cur = self.station.lock().unwrap();
            let moved = cur.as_ref().is_some_and(|old| old.sid() != f.sid());
            *cur = Some(f);
            moved
        };
        let lost = self.lost_at.lock().unwrap().take().is_some_and(|t| t.elapsed() < BACK_IN);
        let state = self.st.lock().unwrap().state;
        if (moved && state == State::Play) || (lost && state == State::Pause) {
            log(&self.device, "music: the station is back, it goes on from where it was");
            self.start(|_| {});
        }
    }

    /// The station `sid` is gone: what played is paused (the queue stays). Checked and cleared at once: a new link
    /// attached meanwhile stays. The ducking is its owners' (`Ducked`): the link's end stops its listening and its
    /// request, and their reasons go with them; a ring waits for the station to come back and keeps the music under it.
    pub fn detach(&self, sid: u64) {
        let mine = {
            let mut cur = self.station.lock().unwrap();
            let mine = cur.as_ref().is_some_and(|f| f.sid() == sid);
            if mine {
                *cur = None;
            }
            mine
        };
        if !mine {
            return;
        }
        if self.st.lock().unwrap().state == State::Play {
            self.halt(State::Pause);
            *self.lost_at.lock().unwrap() = Some(Instant::now());
            log(&self.device, "music: the station is gone, paused");
        }
    }

    fn station(&self) -> Option<Station> {
        self.station.lock().unwrap().clone()
    }

    // ------------------------------------------------------------ for the tools and the session
    /// Finds `query` and plays what was found. → the tool's result
    pub async fn play(self: &Arc<Self>, query: &str) -> Value {
        let r = self
            .http
            .post(format!("{}/v1/find", self.service))
            .json(&json!({"query": query}))
            .timeout(Duration::from_secs(40))
            .send()
            .await;
        let v: Value = match r {
            Ok(r) if r.status().is_success() => r.json().await.unwrap_or(Value::Null),
            Ok(r) => {
                let e: Value = r.json().await.unwrap_or(Value::Null);
                return json!({"ok": false, "error": e["error"].as_str().unwrap_or("музыкальный сервис ответил ошибкой")});
            }
            Err(e) => {
                log(&self.device, format!("music: the service: {e}"));
                return json!({"ok": false, "error": "музыкальный сервис не отвечает"});
            }
        };
        let queue: Vec<Track> = v["tracks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| {
                Some(Track {
                    id: t["id"].as_str()?.to_string(),
                    title: t["title"].as_str().unwrap_or("").to_string(),
                    artist: t["artist"].as_str().unwrap_or("").to_string(),
                    duration: t["duration"].as_f64().filter(|d| *d > 0.0),
                    album: t["album"].as_str().filter(|s| !s.is_empty()).map(String::from),
                    isrc: t["isrc"].as_str().filter(|s| !s.is_empty()).map(String::from),
                })
            })
            .collect();
        if queue.is_empty() {
            return json!({"ok": false, "error": format!("на YouTube Music ничего не нашлось по запросу «{query}»")});
        }
        let n = queue.len();
        self.start(move |st| {
            // what played is over: moved on from (or stopped, if it was paused)
            let how = if st.state == State::Play { "other" } else { "stop" };
            self.end_listen(st, how);
            st.queue = queue;
            st.pos = 0;
            st.offset = 0.0;
            st.repeat = false;
        });
        json!({"ok": true, "playing": v["what"], "tracks_in_queue": n})
    }

    /// pause | resume | next | previous | restart | stop | repeat | repeat_off | back | forward (a seek by SEEK_STEP).
    /// → the tool's result
    pub fn control(self: &Arc<Self>, action: &str) -> Value {
        // the model sends actions outside the enum at times: «ok» to one would be «Выключила» with the music on
        if !["pause", "resume", "next", "previous", "restart", "stop", "repeat", "repeat_off", "back", "forward"]
            .contains(&action)
        {
            return json!({"ok": false, "error": format!("нет такого действия: «{action}»")});
        }
        let res = {
            let st = self.st.lock().unwrap();
            if st.queue.is_empty()
                || (st.state == State::Stop && matches!(action, "repeat" | "repeat_off" | "back" | "forward"))
            {
                return json!({"ok": false, "error": "сейчас ничего не играет"});
            }
            st.state
        };
        match action {
            "pause" if res == State::Play => self.halt(State::Pause),
            "resume" if res == State::Pause => self.start(|_| {}),
            "next" => self.step(1, "next"),
            "previous" => self.step(-1, "previous"),
            // the track on now from its start («включи сначала»: with no such action a model takes «previous», and the
            // song before plays); the same listen unless it was nearly over
            "restart" => self.start(|st| {
                let left = st.listen.as_ref().and_then(|l| l.track.duration).map(|d| d - st.offset);
                if left.is_some_and(|l| l <= listens::RESTART_NEW) {
                    self.end_listen(st, "restart");
                }
                st.pos = st.pos.min(st.queue.len() - 1);
                st.offset = 0.0;
            }),
            "stop" => self.halt(State::Stop),
            "repeat" | "repeat_off" => self.st.lock().unwrap().repeat = action == "repeat",
            "back" => return self.seek(Some(-SEEK_STEP), None),
            "forward" => return self.seek(Some(SEEK_STEP), None),
            _ => {}
        }
        let res = self.result();
        // the panel's player one-shots: for the buttons, the voice and the model alike
        let anim = match action {
            "next" => "next",
            "previous" | "restart" => "previous",
            "pause" => "pause",
            "resume" => "continue",
            _ => "",
        };
        if let (Some(f), false) = (self.station(), anim.is_empty()) {
            f.anim(anim);
        }
        res
    }

    /// To another second of the track on now: `by` s from where the listener is (back: < 0), or `to` that second;
    /// past its end — the next track. The same listen. → the tool's result
    pub fn seek(self: &Arc<Self>, by: Option<f64>, to: Option<f64>) -> Value {
        let (state, at, duration) = {
            let st = self.st.lock().unwrap();
            if st.queue.is_empty() || st.state == State::Stop {
                return json!({"ok": false, "error": "сейчас ничего не играет"});
            }
            (st.state, heard(&st), st.queue[st.pos.min(st.queue.len() - 1)].duration)
        };
        let target = match (to, by) {
            (Some(p), _) if p.is_finite() => p,
            (None, Some(d)) if d.is_finite() => at + d,
            _ => return json!({"ok": false, "error": "перемотка: нужно seconds (на сколько) или position (на какую секунду)"}),
        }
        .max(0.0);
        if duration.is_some_and(|d| target >= d - 1.0) {
            self.step(1, "seek");
            return self.result();
        }
        if state == State::Play {
            self.start(move |st| st.offset = target);
        } else {
            self.change(move |st| st.offset = target);
        }
        let mut r = self.result();
        r["position"] = json!(target.round());
        r
    }

    /// The next (1) or previous (-1) track: what played is over, `how`; the repeat is off. Past the last one the queue
    /// has played out: stopped, as at its natural end. Before the first: the first again from its start, a new listen.
    fn step(self: &Arc<Self>, dir: i64, how: &'static str) {
        let mut over = false;
        let gen = self.change(|st| {
            self.end_listen(st, how);
            let to = (st.pos as i64 + dir).max(0) as usize;
            over = to >= st.queue.len();
            st.pos = to.min(st.queue.len());
            st.offset = 0.0;
            st.repeat = false;
            st.state = if over { State::Stop } else { State::Play };
        });
        self.notify();
        if over {
            log(&self.device, "music: the queue has played out");
        } else {
            tokio::spawn(self.clone().run(gen));
        }
    }

    fn result(&self) -> Value {
        let st = self.st.lock().unwrap();
        let t = &st.queue[st.pos.min(st.queue.len() - 1)]; // pos is past the end once the queue has played out
        json!({"ok": true, "state": st.state.word(), "track": t.name(), "repeat": st.repeat})
    }

    /// The player as the web UI shows it (None: nothing to show): the track, where the listener is in it (s; it moves on
    /// while `live`: a stream sounds), its length, the repeat.
    pub fn status(&self) -> Option<Value> {
        let st = self.st.lock().unwrap();
        if st.queue.is_empty() || !matches!(st.state, State::Play | State::Pause) {
            return None;
        }
        let t = &st.queue[st.pos.min(st.queue.len() - 1)];
        Some(
            json!({"state": st.state.word(), "track": t.name(), "title": t.title, "artist": t.artist, "pos": (heard(&st) * 10.0).round() / 10.0,
                    "duration": t.duration, "repeat": st.repeat, "live": st.t0.is_some() && st.state == State::Play}),
        )
    }

    /// (play | pause, «title — artist») of the track on now: the queue moves on by itself, and the history only knows
    /// what play_music reported.
    pub fn now(&self) -> Option<(String, String)> {
        let st = self.st.lock().unwrap();
        if st.queue.is_empty() || !matches!(st.state, State::Play | State::Pause) {
            return None;
        }
        Some((st.state.word().into(), st.queue[st.pos.min(st.queue.len() - 1)].name()))
    }

    pub fn has_queue(&self) -> bool {
        let st = self.st.lock().unwrap();
        !st.queue.is_empty() && matches!(st.state, State::Play | State::Pause)
    }

    pub fn playing(&self) -> bool {
        self.st.lock().unwrap().state == State::Play
    }

    // ------------------------------------------------------------ inside
    /// The resting panel follows the music: «music» while it plays (if nothing else is shown), off when it stops.
    fn notify(&self) {
        let Some(f) = self.station() else { return };
        if self.playing() {
            f.panel_if("music", &["off"]);
        } else {
            f.panel_if("off", &["music"]);
        }
    }

    /// Every change: first the generation (the running track's task stops writing its position), then the state —
    /// else the old task could write its position over the new one, and a new song would start at 30 s.
    fn change(&self, f: impl FnOnce(&mut St)) -> u64 {
        *self.lost_at.lock().unwrap() = None; // the listener did something since: no resuming by itself
        let gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        self.changed.notify_waiters();
        let killed = {
            let mut st = self.st.lock().unwrap();
            close_stretch(&mut st);
            f(&mut st);
            st.t0 = None;
            self.keep(&st, None);
            st.stream.take()
        };
        if let (Some(id), Some(f)) = (killed, self.station()) {
            f.kill(id);
        }
        gen
    }

    /// Pause (where the listener is stays: `change` closed the stretch there) or stop.
    fn halt(&self, state: State) {
        self.change(|st| {
            if state == State::Stop {
                self.end_listen(st, "stop");
                st.offset = 0.0;
                st.repeat = false;
            }
            st.state = state;
        });
        self.notify();
    }

    /// The listen on now as it is (`ended`: over, how) → listens.rs.
    fn keep(&self, st: &St, ended: Option<&str>) {
        let (Some(w), Some(l)) = (&self.listens, st.listen.as_ref()) else { return };
        let (mut heard_s, mut reached) = (l.heard, l.reached);
        if st.t0.is_some() {
            let at = heard(st);
            heard_s += (at - st.start).max(0.0);
            reached = reached.max(at);
        }
        w.save(listens::Row {
            id: l.id,
            device: self.device.clone(),
            track: l.track.id.clone(),
            title: l.track.title.clone(),
            artist: l.track.artist.clone(),
            album: l.track.album.clone(),
            isrc: l.track.isrc.clone(),
            duration: l.track.duration,
            t: l.t,
            heard: heard_s,
            reached,
            ended: ended.map(String::from),
            t_end: ended.map(|_| now_s()),
        });
    }

    /// The listen on now is over, `how`.
    fn end_listen(&self, st: &mut St, how: &str) {
        close_stretch(st);
        self.keep(st, Some(how));
        st.listen = None;
    }

    /// Plays from the queue position `f` sets (and its offset).
    fn start(self: &Arc<Self>, f: impl FnOnce(&mut St)) {
        let gen = self.change(|st| {
            f(st);
            st.state = State::Play;
        });
        self.notify();
        tokio::spawn(self.clone().run(gen));
    }

    fn current(&self, gen: u64) -> bool {
        self.gen.load(Ordering::SeqCst) == gen
    }
}
