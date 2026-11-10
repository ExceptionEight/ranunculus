//! The LED panel (`/dev/fb0`: 84 dots + 5 logo LEDs, R,G,B,W per pixel), drawn by a thread of its own: the
//! assistant's states as the server names them (states.rs), the music's stars from the stream the station plays
//! (anim.rs; analysed in play.rs), and over them the firmware's volume ring and player one-shots.
//! One frame per 20 ms by CLOCK_MONOTONIC while there is something to show; the music layer is drawn for the moment
//! the frame is shown. The logo goes red while the microphones are muted. Never touches
//! /sys/class/graphics/fb0/device/jtag/ (reading it erases the panel's FPGA).
pub mod analyzer;
pub mod anim;
pub mod states;
#[rustfmt::skip] // measured numbers, one table a line
pub mod tables;

use std::fs::{File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

const NPIX: usize = 89;
const FRAME: usize = NPIX * 4;
const PERIOD_NS: i64 = 20_000_000;

pub fn now_ns() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    #[allow(clippy::unnecessary_cast)] // the two are not i64 on every target
    let ns = ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64;
    ns
}

/// What the panel thread is told.
pub enum Msg {
    /// One analysed hop of the music: when it will be heard (monotonic s), its band levels, and the onsets of the
    /// hop before (when heard, strength and peak over the median flux per band).
    Music { heard: f64, bass: f32, mid: f32, high: f32, onsets_at: f64, onsets: [f32; 3], peaks: [f32; 3] },
    /// The server is gone: blank the panel, give the backlight back, forget the queue.
    Reset,
    /// The assistant's state (states.rs).
    State(states::State),
    /// The colours and feel.
    Look(states::Look),
    /// The loudness of the voice as it will be heard: levels 20 ms apart, the first at `t0` (monotonic s).
    Voice { t0: f64, levels: Vec<f64> },
    /// The volume knob changed (0..255): shown as the firmware shows it.
    Volume(u8),
    /// A one-shot of the player: `next`, `previous`, `pause`, `continue`.
    Anim(String),
}

/// The firmware's player animations (its ledpatterns/panel/player_*.led, read on the station), in the look's colour:
/// the brightness of every dot is kept, the hue is the look's. next/previous — a wave along the ring (1.9 s), pause —
/// two pulsing blocks, continue — a «▶» sliding right (1 s). Shown alone on the panel, then back to what was there.
pub const PLAYER_LEDS: [(&str, &str); 4] = [
    ("next", "/system/vendor/quasar/ledpatterns/panel/player_next_track.led"),
    ("previous", "/system/vendor/quasar/ledpatterns/panel/player_previous_track.led"),
    ("pause", "/system/vendor/quasar/ledpatterns/panel/player_pause.led"),
    ("continue", "/system/vendor/quasar/ledpatterns/panel/player_continue.led"),
];

/// A .led file as intensities 0..1 (each dot's brightest channel over the file's brightest) and durations (ms).
pub fn read_led_intensity(path: &str) -> (Vec<[f32; tables::N]>, Vec<u32>) {
    let Ok(s) = std::fs::read_to_string(path) else { return (vec![], vec![]) };
    let (mut fr, mut du) = (vec![], vec![]);
    for line in s.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        if w.len() != tables::N + 1 {
            continue;
        }
        let mut f = [0f32; tables::N];
        for (j, x) in w[..tables::N].iter().enumerate() {
            let v = u32::from_str_radix(x, 16).unwrap_or(0).to_be_bytes();
            f[j] = *v.iter().max().unwrap() as f32;
        }
        fr.push(f);
        du.push(w[tables::N].parse().unwrap_or(20));
    }
    let top = fr.iter().flat_map(|f| f.iter()).cloned().fold(1.0f32, f32::max);
    for f in fr.iter_mut() {
        f.iter_mut().for_each(|x| *x /= top);
    }
    (fr, du)
}

/// The firmware's volume indicator (its ledpatterns/panel/volume.led, read on the station): a scale — frame k of
/// 300 is the ring around the centre button filled to k/299 (6 dots at the bottom of the scale, 14 at the top), green
/// turning yellow, orange and red near the top. Shown for the knob's level, gliding to it, held HOLD s after the last
/// change, then faded out, over whatever the panel shows.
pub const VOLUME_LED: &str = "/system/vendor/quasar/ledpatterns/panel/volume.led";
const VOL_GLIDE: f64 = 1200.0; // frames a second (the whole scale in 0.25 s)
const VOL_IN: f64 = 0.12;
const VOL_HOLD: f64 = 1.5;
const VOL_OUT: f64 = 0.4;
const VOL_BL: i32 = 70; // the backlight while it shows (the idle backlight is too dim for it)

/// .led frames: 84 words RRGGBBWW + a duration per line ("no_size_optimization" and blank lines skipped).
pub fn read_led(path: &str) -> Vec<[u8; FRAME - 20]> {
    let Ok(s) = std::fs::read_to_string(path) else { return vec![] };
    let mut out = vec![];
    for line in s.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        if w.len() != tables::N + 1 {
            continue;
        }
        let mut f = [0u8; FRAME - 20];
        let mut ok = true;
        for (j, x) in w[..tables::N].iter().enumerate() {
            match u32::from_str_radix(x, 16) {
                Ok(v) => f[j * 4..j * 4 + 4].copy_from_slice(&v.to_be_bytes()),
                Err(_) => ok = false,
            }
        }
        if ok {
            out.push(f);
        }
    }
    out
}

struct VolShow {
    target: f64,
    shown: f64,
    since: f64,
    changed: f64,
}

#[derive(Clone)]
pub struct Panel {
    tx: Sender<Msg>,
}

impl Panel {
    /// Opens the panel and starts its thread; `bl0`: the idle backlight (set now). Err: no panel (tests, the sandbox).
    pub fn start(bl0: i32) -> std::io::Result<Panel> {
        let dev = Dev::open()?;
        if dev.read_bl() != bl0 {
            dev.set_bl(bl0);
        }
        let (tx, rx) = channel();
        std::thread::Builder::new().name("panel".into()).spawn(move || run(dev, rx, bl0))?;
        Ok(Panel { tx })
    }

    pub fn send(&self, m: Msg) {
        let _ = self.tx.send(m);
    }
}

/// One dot: intensity 0..1 → R, G, B, W (one hue, brightness along the gamma curve).
fn colour(i: f64, rgb: &[f64; 3], gain: f64, gamma: f64, out: &mut [u8]) {
    let v = i.clamp(0.0, 1.0).powf(gamma);
    for c in 0..3 {
        out[c] = (v * rgb[c] * gain).round().clamp(0.0, 255.0) as u8;
    }
    out[3] = 0;
}

struct Dev {
    fb: File,
    pan: File,
    bl: File,
}

impl Dev {
    fn open() -> std::io::Result<Dev> {
        // STATION_FB_DIR: a directory with files fb0, pan, brightness standing in for the panel (off the station)
        let (fb, pan, bl) = match std::env::var("STATION_FB_DIR") {
            Ok(d) => (format!("{d}/fb0"), format!("{d}/pan"), format!("{d}/brightness")),
            Err(_) => {
                ("/dev/fb0".into(), "/sys/class/graphics/fb0/pan".into(), "/sys/class/backlight/fb0/brightness".into())
            }
        };
        Ok(Dev {
            fb: OpenOptions::new().write(true).open(fb)?,
            pan: OpenOptions::new().write(true).open(pan)?,
            bl: OpenOptions::new().read(true).write(true).open(bl)?,
        })
    }
    fn show(&self, px: &[u8; FRAME]) {
        if self.fb.write_at(px, 0).is_err() {
            eprintln!("panel: fb0 write failed");
        }
        let _ = self.pan.write_at(b"0,0", 0);
    }
    fn read_bl(&self) -> i32 {
        let mut b = [0u8; 16];
        match self.bl.read_at(&mut b, 0) {
            Ok(n) if n > 0 => String::from_utf8_lossy(&b[..n]).trim().parse().unwrap_or(-1),
            _ => -1,
        }
    }
    fn set_bl(&self, v: i32) {
        let _ = self.bl.write_at(format!("{v}\n").as_bytes(), 0);
    }
}

fn run(dev: Dev, rx: Receiver<Msg>, bl0: i32) {
    let mut bl_cur = bl0;
    let mut look = states::Look::default();
    let (mut muted, mut muted_checked) = (false, 0i64);
    let mut music = anim::Music::new(now_ns() as u64);
    let mut music_since: Option<f64> = None;
    let (mut t0, mut k) = (0i64, 0i64);
    let mut last = [0u8; FRAME];
    let mut lit = false; // something non-blank was shown since the last reset
    let mut engine: Option<states::Engine> = None; // the states, once the server names one
    let vol_frames = read_led(VOLUME_LED);
    let player: Vec<(&str, Vec<[f32; tables::N]>, Vec<u32>)> = PLAYER_LEDS
        .iter()
        .map(|(n, p)| {
            let (f, d) = read_led_intensity(p);
            (*n, f, d)
        })
        .collect();
    let mut anim: Option<(usize, f64)> = None; // a player animation: which, since
    let mut vol: Option<VolShow> = None;
    let mut vol_last = 0f64;
    loop {
        let due = if t0 != 0 { t0 + k * PERIOD_NS } else { 0 };
        let wait =
            if t0 != 0 { Duration::from_nanos((due - now_ns()).max(0) as u64) } else { Duration::from_millis(500) };
        // everything that has come in, waiting at most until the next frame is due
        let mut first = true;
        loop {
            let m = if first { rx.recv_timeout(wait) } else { rx.try_recv().map_err(|_| RecvTimeoutError::Timeout) };
            first = false;
            match m {
                Ok(Msg::Music { heard, bass, mid, high, onsets_at, onsets, peaks }) => {
                    music.push(heard, bass, mid, high);
                    music.push_onsets(onsets_at, onsets, peaks);
                }
                Ok(Msg::State(st)) => {
                    let now = now_ns() as f64 / 1e9;
                    engine.get_or_insert_with(|| states::Engine::new(bl0, now)).set(st, now);
                    if t0 == 0 {
                        t0 = now_ns();
                        k = 0;
                    }
                }
                Ok(Msg::Look(l)) => {
                    look = l.clone();
                    engine.get_or_insert_with(|| states::Engine::new(bl0, now_ns() as f64 / 1e9)).look = l;
                }
                Ok(Msg::Voice { t0: vt, levels }) => {
                    if let Some(e) = engine.as_mut() {
                        e.voice(vt, &levels);
                    }
                }
                Ok(Msg::Anim(name)) => {
                    if let Some(i) = player.iter().position(|p| p.0 == name && !p.1.is_empty()) {
                        anim = Some((i, now_ns() as f64 / 1e9));
                        if t0 == 0 {
                            t0 = now_ns();
                            k = 0;
                        }
                    }
                }
                Ok(Msg::Volume(level)) => {
                    if !vol_frames.is_empty() {
                        let now = now_ns() as f64 / 1e9;
                        let target = level as f64 / 255.0 * (vol_frames.len() - 1) as f64;
                        match vol.as_mut() {
                            Some(v) => (v.target, v.changed) = (target, now),
                            None => {
                                // it glides from where the knob was (or from the bottom the first time)
                                vol = Some(VolShow {
                                    target,
                                    shown: if vol_last > 0.0 { vol_last } else { target },
                                    since: now,
                                    changed: now,
                                });
                            }
                        }
                        if t0 == 0 {
                            t0 = now_ns();
                            k = 0;
                        }
                    }
                }
                Ok(Msg::Reset) => {
                    t0 = 0;
                    music_since = None;
                    vol = None;
                    anim = None;
                    if let Some(e) = engine.as_mut() {
                        e.set(states::State::Off, now_ns() as f64 / 1e9);
                    }
                    engine = None;
                    if lit {
                        last = [0u8; FRAME];
                        dev.show(&last);
                        lit = false;
                    }
                    if bl0 >= 0 && bl_cur != bl0 {
                        dev.set_bl(bl0);
                        bl_cur = bl0;
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        if t0 == 0 {
            continue;
        }
        let t = now_ns();
        if t < due {
            if due - t < 1_500_000 {
                std::thread::sleep(Duration::from_nanos((due - t) as u64));
            } else {
                continue;
            }
        }
        // what the states want now; the volume ring and the player's one-shots need frames even when nothing else
        // draws: a blank one under them
        let draw = engine
            .as_mut()
            .and_then(|e| e.frame(due as f64 / 1e9))
            .or_else(|| (vol.is_some() || anim.is_some()).then(|| states::Draw::blank(bl0)));
        if let Some(f) = draw {
            // the backlight, decided once a frame: what the frame wants, at least VOL_BL under the volume ring
            // (decided twice — the frame's, then the ring's — it would flip 15 ↔ 70 every frame, a flicker)
            let frame_bl = f.backlight as i32;
            let want = if vol.is_some() { frame_bl.max(VOL_BL) } else { frame_bl };
            if want != bl_cur {
                dev.set_bl(want);
                bl_cur = want;
            }
            let logo = f.logo as f64 / 255.0;
            let w = f.music as f64 / 255.0;
            let due_s = due as f64 / 1e9;
            let mut m = [[0f32; 3]; tables::N];
            if w > 0.0 {
                let since = match music_since {
                    Some(s) => s,
                    None => {
                        music.start(due_s);
                        music_since = Some(due_s);
                        due_s
                    }
                };
                m = music.render(due_s + look.music_lag, since, look.music_react, look.gain, look.gamma, look.night);
            } else {
                music_since = None;
            }
            for j in 0..tables::N {
                let px = &mut last[j * 4..j * 4 + 4];
                colour(f.dots[j] as f64 / 255.0, &look.color, look.gain, look.gamma, px); // the states, one hue
                for c in 0..3 {
                    px[c] = (px[c] as f64 + w * m[j][c] as f64).round().clamp(0.0, 255.0) as u8;
                    // + the music
                }
            }
            if due - muted_checked > 1_000_000_000 {
                muted = std::fs::read_to_string("/data/quasar/data/is_speaker_muted").is_ok_and(|s| s.trim() == "1");
                muted_checked = due;
            }
            for j in tables::N..NPIX {
                if muted {
                    colour(look.logo, &[255.0, 0.0, 0.0], look.gain, look.gamma, &mut last[j * 4..j * 4 + 4]);
                } else {
                    colour(logo, &look.logo_color, look.gain, look.gamma, &mut last[j * 4..j * 4 + 4]);
                }
            }
            // a player one-shot instead of the panel, in the look's colour (the stars and states wait under it)
            if let Some((i, since)) = anim {
                let el_ms = ((due as f64 / 1e9 - since) * 1000.0).max(0.0) as u32;
                let (_, frames, durs) = &player[i];
                let mut acc = 0u32;
                let mut idx = None;
                for (j, d) in durs.iter().enumerate() {
                    acc += d;
                    if el_ms < acc {
                        idx = Some(j);
                        break;
                    }
                }
                match idx {
                    Some(j) => {
                        for d in 0..tables::N {
                            colour(frames[j][d] as f64, &look.color, look.gain, 1.0, &mut last[d * 4..d * 4 + 4]);
                        }
                    }
                    None => anim = None,
                }
            }
            // the volume ring: alone on the panel (what was there fades out under it and comes back)
            if let Some(v) = vol.as_mut() {
                let now = due as f64 / 1e9;
                let dt = 0.02;
                let d = v.target - v.shown;
                v.shown += d.signum() * d.abs().min(VOL_GLIDE * dt);
                let a = ((now - v.since) / VOL_IN).clamp(0.0, 1.0)
                    * (1.0 - ((now - v.changed - VOL_HOLD) / VOL_OUT).clamp(0.0, 1.0));
                let f = &vol_frames[(v.shown.round() as usize).min(vol_frames.len() - 1)];
                let mut shown = last;
                for j in 0..tables::N {
                    for c in 0..4 {
                        let base = last[j * 4 + c] as f64 * (1.0 - a);
                        shown[j * 4 + c] = (base + f[j * 4 + c] as f64 * a).round().clamp(0.0, 255.0) as u8;
                    }
                }
                let over = now - v.changed > VOL_HOLD + VOL_OUT;
                if over {
                    vol_last = v.shown;
                    vol = None;
                    if frame_bl != bl_cur {
                        dev.set_bl(frame_bl); // back to what the frame under it wants
                        bl_cur = frame_bl;
                    }
                    dev.show(&last);
                } else {
                    dev.show(&shown);
                }
            } else {
                dev.show(&last);
            }
            lit = true;
            k += 1;
        } else if now_ns() - due > 200_000_000 {
            t0 = 0; // nothing to show for 200 ms: the clock stops till there is
        } else {
            k += 1;
        }
    }
}
