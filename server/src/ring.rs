//! The station rings: a sound looped (sounds.rs), not the voice. Timers and alarms ring the same way.
//! - It rings for `seconds` (3 s … 10 min: the settings say how long), then fades out — unless stopped first:
//!   - by the wake word: the command after it, if it is only «стоп», «хватит», «спасибо» or nothing at all, is the
//!     end of it — not for the music («стоп» would stop it), not for the model;
//!   - by a touch: the centre or a slide over the ring — whichever, it is the ring that is meant, not the player
//!     (the volume keys only change the volume);
//!   - from the web UI.
//! - One ring a device: another timer due meanwhile keeps it ringing, the time counted anew.
//! - It waits for a dialog on the device to end (up to WAIT s): a ring over an answer or over the user's command
//!   spoils both («таймер на 3 секунды» would ring over «Поставила таймер»).
//! - The music is ducked under it, as under the voice; the panel shows «speak», which the station draws by the
//!   stream's loudness: it pulses with the ring. Neither comes back while another holds them: an answer ending under
//!   the ring, a stopped ring's tail under the next one (music.rs: `Why`; session.rs: `rest_panel`).
//! - The link lost mid-ring (Wi-Fi, the station reconnecting): it rings on once the station is back, till its time is
//!   up — the same ring: the sound and an alarm's rise go on where they were, the break not counted; its volume set again.
//! - An alarm may rise (`Opts::ramp`: from RAMP_FROM dB to full over that many seconds) and have its own volume
//!   (`Opts::volume`: the knob set for the ring, put back after it unless the user moved it meanwhile).
use crate::events;
use crate::music::Why;
use crate::session::{log, rest_panel, Active, Shared, Station};
use crate::sounds::{Sound, RATE};
use crate::text::now_s;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const MIN: f64 = 3.0;
pub const MAX: f64 = 600.0;
const WAIT: f64 = 10.0;
const FADE: f64 = 0.3;
const FADE_IN: f64 = 0.01;
const LEAD: f64 = 0.4; // ahead of real time, as the voice goes: all that can still sound after a stop
const RAMP_FROM: f64 = -30.0; // dB: where a rising ring starts

/// How an alarm rings, beyond the sound and how long.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Opts {
    /// seconds to rise from RAMP_FROM dB to full; 0 — full at once
    pub ramp: f64,
    /// the knob (0..255) while it rings; None — as it is
    pub volume: Option<u8>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Stop {
    Wake = 1,
    Touch = 2,
    Web = 3,
}

/// What a ring is, for the web UI.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Timer,
    Alarm,
    /// a sound tried from the web UI
    Test,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::Timer => "timer",
            Kind::Alarm => "alarm",
            Kind::Test => "test",
        }
    }
}

pub struct Ring {
    id: u64,
    until: Mutex<f64>,
    stop: AtomicU8,
    kind: Kind,
    /// the timer or the alarm in the user's words: «таймер на 10 мин (чай)»
    what: String,
    sound: String,
}

impl Ring {
    fn stopped(&self) -> Option<Stop> {
        match self.stop.load(Ordering::SeqCst) {
            1 => Some(Stop::Wake),
            2 => Some(Stop::Touch),
            3 => Some(Stop::Web),
            _ => None,
        }
    }

    /// For the web UI: what rings, which sound, till when (unix time; the fade after it).
    pub fn json(&self) -> Value {
        json!({"kind": self.kind.word(), "what": self.what, "sound": self.sound, "until": *self.until.lock().unwrap()})
    }
}

fn ring_event(device: &str, r: Option<&Ring>) {
    events::emit("device", json!({"id": device, "ring": r.map(|r| r.json())}));
}

impl Shared {
    /// Stops the device's ring. → whether one was ringing.
    pub fn stop_ring(&self, device: &str, by: Stop) -> bool {
        match self.rings.lock().unwrap().remove(device) {
            Some(r) => {
                r.stop.store(by as u8, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }
}

/// The looped sound, block by block: a fade-in at its very start (and where it goes on after a break), FADE s out from
/// `end` (a sample count) on.
pub struct Looper {
    pcm: Arc<Vec<i16>>,
    pos: usize,
    made: usize,
    from: usize,         // where the stream on now began (the fade-in)
    fade: Option<usize>, // samples of the fade-out left
    ramp: usize,         // samples to rise over (0: none)
}

impl Looper {
    pub fn new(s: &Sound) -> Looper {
        Looper { pcm: s.pcm.clone(), pos: 0, made: 0, from: 0, fade: None, ramp: 0 }
    }

    /// Rising from RAMP_FROM dB to full over `seconds` (even in dB: it sounds even).
    pub fn rising(mut self, seconds: f64) -> Looper {
        self.ramp = (seconds.max(0.0) * RATE as f64) as usize;
        self
    }

    /// The same ring in a new stream (the link came back): faded in again, the sound and the rise go on where they
    /// were — what sounded counts, the break does not.
    fn resume(&mut self) {
        self.from = self.made;
    }

    /// The next `n` samples as s16le, if any are left; `end`: where the fade-out begins (moved on by a later timer: the
    /// fade is called off).
    pub fn next(&mut self, n: usize, end: usize) -> Option<Vec<u8>> {
        let fade_n = (FADE * RATE as f64) as usize;
        match self.fade {
            Some(0) => return None,
            Some(_) if self.made < end => self.fade = None,
            None if self.made >= end => self.fade = Some(fade_n),
            _ => {}
        }
        let fade_in = (FADE_IN * RATE as f64) as usize;
        let mut out = Vec::with_capacity(n * 2);
        for _ in 0..n {
            let mut v = self.pcm[self.pos] as f32;
            self.pos = (self.pos + 1) % self.pcm.len();
            if self.made - self.from < fade_in {
                v *= (self.made - self.from) as f32 / fade_in as f32;
            }
            if self.made < self.ramp {
                v *= 10f64.powf(RAMP_FROM * (1.0 - self.made as f64 / self.ramp as f64) / 20.0) as f32;
            }
            if let Some(left) = self.fade.as_mut() {
                v *= *left as f32 / fade_n as f32;
                *left = left.saturating_sub(1);
            }
            self.made += 1;
            out.extend((v as i16).to_le_bytes());
        }
        Some(out)
    }
}

/// The ring as it would sound for `seconds` (the fade-out after it), all at once: for listening to it in the web UI.
pub fn render(s: &Sound, seconds: f64) -> Vec<u8> {
    render_rising(s, seconds, 0.0)
}

/// … rising over `ramp` s, as an alarm.
pub fn render_rising(s: &Sound, seconds: f64, ramp: f64) -> Vec<u8> {
    let mut l = Looper::new(s).rising(ramp);
    let end = (seconds * RATE as f64) as usize;
    let mut out = vec![];
    while let Some(b) = l.next(RATE as usize / 10, end) {
        out.extend(b);
    }
    out
}

static IDS: AtomicU64 = AtomicU64::new(1);

/// Rings `sound` on the device's station for `seconds`; `what`: (its kind, its name in the web UI); `deadline`: how
/// long to wait for the station (it may be reconnecting); `started` is called once the ring sounds (or is given up).
pub async fn ring(
    shared: &Arc<Shared>,
    device: &str,
    sound: Sound,
    seconds: f64,
    what: (Kind, &str),
    deadline: f64,
    started: impl FnOnce(),
) {
    ring_with(shared, device, sound, seconds, what, deadline, Opts::default(), started).await
}

/// `ring`, rising and at its own volume (an alarm). A ring already sounding is only made longer.
#[allow(clippy::too_many_arguments)]
pub async fn ring_with(
    shared: &Arc<Shared>,
    device: &str,
    sound: Sound,
    seconds: f64,
    what: (Kind, &str),
    deadline: f64,
    opts: Opts,
    started: impl FnOnce(),
) {
    let mut f = loop {
        if let Some(f) = shared.stations.lock().unwrap().get(device).cloned() {
            break f;
        }
        if now_s() > deadline {
            log(device, "   (a ring missed: the station is not there)");
            started();
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let t = now_s();
    while shared.in_dialog(device) && now_s() - t < WAIT {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    f.set_self_name(false); // the wake word stops a ring, even over an answer that said the name
    let seconds = seconds.clamp(MIN, MAX);
    let r = {
        let mut rings = shared.rings.lock().unwrap();
        if let Some(r) = rings.get(device).cloned() {
            *r.until.lock().unwrap() = now_s() + seconds;
            drop(rings);
            log(device, format!("   (another ring due: it rings on, {seconds:.0} s more)"));
            ring_event(device, Some(&r));
            started();
            return;
        }
        let r = Arc::new(Ring {
            id: IDS.fetch_add(1, Ordering::SeqCst),
            until: Mutex::new(now_s() + seconds),
            stop: AtomicU8::new(0),
            kind: what.0,
            what: what.1.to_string(),
            sound: sound.name.clone(),
        });
        rings.insert(device.to_string(), r.clone());
        r
    };
    ring_event(device, Some(&r));
    started();
    let waited = now_s() - t;
    log(
        device,
        format!(
            "   (ringing: the sound «{}» for {seconds:.0} s{})",
            sound.name,
            if waited > 0.5 { format!(", after the dialog: +{waited:.1} s") } else { String::new() }
        ),
    );
    let _busy = Active::new(shared);
    let player = shared.player(device);
    // the music under it till it is over — also while it waits for the station to come back (the music resumed then
    // plays under it); a dialog or another ring holds the music down by itself (music.rs: Why)
    let duck = player.as_ref().map(|p| p.duck(Why::Ring));
    let mut l = Looper::new(&sound).rising(opts.ramp);
    let mut before = None; // the knob before it (its own volume), to be put back
    let t0 = now_s();
    let how = loop {
        f.panel("speak");
        if let Some(v) = opts.volume {
            if let Ok(b) = f.volume_get().await {
                if b != v && f.volume_set(v).await.is_ok() {
                    before.get_or_insert(b);
                }
            }
        }
        match stream(&f, &mut l, &r).await {
            End::Lost => {}
            how => break how,
        }
        // the link broke mid-ring (Wi-Fi, the station reconnecting): it rings on there once the station is back, till its
        // time is up — the timer or the alarm is counted already, and an alarm so cut short could be slept through
        if now_s() < *r.until.lock().unwrap() {
            log(device, "   (the ring broke off: the station is gone, waited for till the ring is due to end)");
        }
        match back(shared, device, &f, &r).await {
            Ok(nf) => {
                log(
                    device,
                    format!(
                        "   (the station is back: the ring goes on, {:.0} s more)",
                        (*r.until.lock().unwrap() - now_s()).max(0.0)
                    ),
                );
                f = nf;
                l.resume();
            }
            Err(how) => break how,
        }
    };
    if let (Some(b), Some(v)) = (before, opts.volume) {
        if f.volume_known() == Some(v) {
            let _ = f.volume_set(b).await; // the user did not touch it meanwhile
        }
    }
    {
        let mut rings = shared.rings.lock().unwrap();
        if rings.get(device).is_some_and(|x| x.id == r.id) {
            rings.remove(device);
        }
    }
    drop(duck);
    log(
        device,
        format!(
            "   (the ring {} after {:.1} s)",
            match how {
                End::Played => "played out",
                End::Stopped(Stop::Wake) => "was stopped by the wake word",
                End::Stopped(Stop::Touch) => "was stopped by a touch",
                End::Stopped(Stop::Web) => "was stopped from the web UI",
                End::Lost => "broke off: the station is gone",
            },
            now_s() - t0
        ),
    );
    if !shared.rings.lock().unwrap().contains_key(device) {
        ring_event(device, None);
    }
    // the panel back to rest — the next ring's «speak» if one sounds already; a dialog on lets go of it at its end
    rest_panel(shared, &f, &player, &["speak"], true);
}

/// The station back after its link was lost mid-ring: its new link. Else how the ring ended meanwhile: stopped (from
/// the web UI, or by the wake word on the new link before this saw it), or its time up — lost.
async fn back(shared: &Shared, device: &str, old: &Station, r: &Ring) -> Result<Station, End> {
    loop {
        if let Some(s) = r.stopped() {
            return Err(End::Stopped(s));
        }
        if now_s() >= *r.until.lock().unwrap() {
            return Err(End::Lost);
        }
        if let Some(f) = shared.stations.lock().unwrap().get(device).filter(|f| f.sid() != old.sid() && !f.gone()) {
            return Ok(f.clone());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[derive(PartialEq, Debug)]
enum End {
    Played,
    Stopped(Stop),
    Lost,
}

/// The ring as one stream, 0.1-s blocks LEAD s ahead of real time; a stop kills the stream at once. `l` goes on from
/// where it was (a stream after the link came back): its end is `until` from now on.
async fn stream(f: &Station, l: &mut Looper, r: &Ring) -> End {
    let (id, mut rx) = f.open(RATE, 1, None, None);
    let block = RATE as usize / 10;
    let (t0, start, base) = (Instant::now(), now_s(), l.made);
    let mut sent = 0usize;
    loop {
        if let Some(s) = r.stopped() {
            f.kill(id);
            return End::Stopped(s);
        }
        let ahead = sent as f64 / RATE as f64 - t0.elapsed().as_secs_f64();
        if ahead > LEAD {
            tokio::time::sleep(Duration::from_secs_f64((ahead - LEAD).min(0.1))).await;
            continue;
        }
        let end = base + ((*r.until.lock().unwrap() - start).max(0.0) * RATE as f64) as usize;
        let Some(b) = l.next(block, end) else { break };
        if !f.pcm(id, b) {
            return End::Lost;
        }
        sent += block;
    }
    f.end(id);
    loop {
        tokio::select! {
            d = &mut rx => return if d.is_ok() { End::Played } else { End::Lost },
            _ = tokio::time::sleep(Duration::from_millis(50)) => {
                if let Some(s) = r.stopped() {
                    f.kill(id);
                    return End::Stopped(s);
                }
                if f.gone() {
                    return End::Lost;
                }
            }
        }
    }
}

/// The command after a wake that stopped the ring: is it only about the ring (then nothing more is done)?
pub fn is_stop(text: &str) -> bool {
    static RE: OnceLock<(Regex, Regex, Regex)> = OnceLock::new();
    let (word, junk, filler) = RE.get_or_init(|| {
        let w = "стоп|стой|хватит|достаточно|все|ладно|хорошо|ок|окей|спасибо|понял|поняла|понятно|услышал|услышала|слышу|\
                 отбой|отмена|выключи|выключись|выключай|отключи|отключись|замолчи|заткнись|тихо|тишина|встаю|встал|встала|\
                 (выключи|отключи|останови|убери|сбрось|отмени)( этот| эту)? (таймер|будильник|звонок|звук|сигнал)|таймер|будильник|\
                 stop|ok|okay|thanks";
        (Regex::new(&format!("^(?:{w})(?: (?:{w}))*$")).unwrap(),
         // recognition writes some words in Latin: «Стоп.» comes back as «Stop.», and the model, given it, stops the
         // music
         Regex::new("[^а-яёa-z ]").unwrap(),
         Regex::new(r"\b(пожалуйста|ну|давай|а|ой|да|уже|мразота)\b").unwrap())
    });
    let t = junk.replace_all(&text.to_lowercase().replace('ё', "е"), " ").to_string();
    let t = filler.replace_all(&t, " ").split_whitespace().collect::<Vec<_>>().join(" ");
    !t.is_empty() && word.is_match(&t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_alarm_rises() {
        let s = Sound { name: "x".into(), pcm: Arc::new(vec![10000i16; RATE as usize]) };
        let pcm: Vec<i16> = render_rising(&s, 4.0, 2.0).chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        let db = |i: usize| 20.0 * (pcm[i] as f64 / 10000.0).abs().log10();
        let r = RATE as usize;
        assert!((db(r / 50) - RAMP_FROM).abs() < 1.0, "quiet at first: {}", db(r / 50));
        assert!((db(r) - RAMP_FROM / 2.0).abs() < 0.5, "half way in dB after 1 s of 2: {}", db(r));
        assert!(db(2 * r + 10).abs() < 0.01 && db(3 * r).abs() < 0.01, "full after the ramp");
        // no ramp: as before
        assert_eq!(render_rising(&s, 4.0, 0.0), render(&s, 4.0));
    }

    #[test]
    fn a_ring_goes_on_after_a_break() {
        // an alarm rising over 2 s, its link lost after 1 s: the new stream fades in again, and the rise goes on from
        // where it was, not from the start
        let s = Sound { name: "x".into(), pcm: Arc::new(vec![10000i16; RATE as usize]) };
        let r = RATE as usize;
        let mut l = Looper::new(&s).rising(2.0);
        for _ in 0..10 {
            l.next(r / 10, 4 * r).unwrap();
        }
        l.resume();
        let x: Vec<i16> = l.next(r / 10, 4 * r).unwrap().chunks(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(x[0], 0, "faded in again");
        let db = 20.0 * (x[x.len() - 1] as f64 / 10000.0).log10();
        assert!((db - RAMP_FROM * (1.0 - 1.1 / 2.0)).abs() < 0.5, "the rise goes on at 1.1 s of 2: {db}");
    }

    #[test]
    fn stop_words() {
        for s in [
            "Стоп.",
            "Stop.",
            "OK, stop",
            "Хватит!",
            "стоп, стоп",
            "Спасибо.",
            "Всё, всё.",
            "Выключи таймер.",
            "Ну хватит уже",
            "Ок, спасибо",
            "Отключи будильник, пожалуйста",
        ] {
            assert!(is_stop(s), "{s}");
        }
        for s in [
            "",
            "Поставь ещё на пять минут.",
            "Сколько времени?",
            "Выключи музыку.",
            "Тише.",
            "Стоп музыка",
            "Stop the music",
            "Включи свет",
        ] {
            assert!(!is_stop(s), "{s}");
        }
    }

    fn sound(n: usize) -> Sound {
        Sound { name: "t".into(), pcm: Arc::new(vec![10000i16; n]) }
    }

    #[test]
    fn looped_then_faded() {
        let s = sound(1000);
        let pcm = render(&s, 1.0);
        let x: Vec<i16> = pcm.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c)).collect();
        let secs = x.len() as f64 / RATE as f64;
        assert!((secs - (1.0 + FADE)).abs() < 0.11, "{secs}");
        assert_eq!(x[0], 0, "fades in");
        assert_eq!(x[48000 / 2], 10000, "the sound, looped past its own length");
        assert!(x[x.len() - 1].abs() < 100, "fades out");
        assert!(x[48000 + 7200] < 10000 && x[48000 + 7200] > 2000, "halfway through the fade");
    }

    #[test]
    fn a_later_timer_calls_the_fade_off() {
        let mut l = Looper::new(&sound(480));
        let mut n = 0;
        // the fade begins at 1 s (block 11); before block 12 another timer moves the end to 4 s
        while let Some(b) = l.next(4800, if n < 11 { 4800 * 10 } else { 4800 * 40 }) {
            n += 1;
            if n > 100 {
                break;
            }
            let last = i16::from_le_bytes([b[b.len() - 2], b[b.len() - 1]]);
            if n == 25 {
                assert_eq!(last, 10000, "the fade begun at 1 s is off: the ring goes on to 4 s");
            }
        }
        assert_eq!(n, 43, "4 s, then the fade (3 blocks)");
    }
}
