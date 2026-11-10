//! The assistant's states on the panel: the animations, their crossfades, the logo and the backlight. The server says
//! only the state and the colours; «speak» follows the voice as the station plays it (play.rs measures its loudness),
//! so the light is in time with the sound without any guessing about network delays.
//!
//! listen — «вдох»: the ring around the centre button fills from the bottom up both sides and breathes; sparks
//!          drift in from the edges once the ring is there;
//! think  — «варится»: two comets on two rings circle in opposite directions, embers glow and fade outside;
//! search — «ищу в интернете»: a radar beam sweeps round the ring with a long trail, waves go out to the corners;
//! speak  — the ring follows the loudness of the voice (brightness only), the outer glow hints at loud syllables;
//! static — the full ring (for picking colours); music — the stars (anim.rs), weighted in by the mix.
//! All of it smooth and slow, nothing popping up, in one colour.
use super::tables::{ANG, D, EMBERS, N};
use std::f64::consts::PI;

/// The listening sparks: (angle, start s, speed) of each, drawn at random once and for all.
const SPARKS: [(f64, f64, f64); 12] = [
    (0.785997998, 7.177710408, 2.142979983),
    (-1.726574146, 2.401330279, 2.211487412),
    (-3.108509769, 6.569827347, 2.1579486),
    (-0.201470633, 2.424259415, 1.794897928),
    (-1.540199805, 3.560610447, 1.953183781),
    (0.336133777, 7.964002267, 2.154863343),
    (0.767674739, 7.911681181, 1.750716089),
    (-2.134950756, 4.900316834, 1.630759406),
    (-2.91740685, 4.119110562, 1.926344218),
    (2.621142423, 5.033810036, 1.959882353),
    (-0.019644785, 1.980119376, 1.608255818),
    (-1.932694329, 5.536256967, 1.740424707),
];

pub const XFADE: f64 = 0.6; // s, state crossfade
const LOGO_RAMP: f64 = 0.7;
const BL_RAMP: f64 = 0.8;
/// the voice is heard this long after the station starts writing a stream to aplay (ALSA's start)
pub const VOICE_LAG: f64 = 0.15;

fn smooth(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn bump(x: f64, w: f64) -> f64 {
    (-(x / w) * (x / w)).exp()
}

/// a − b as an angle in [−π, π).
fn angdiff(a: f64, b: f64) -> f64 {
    (a - b + PI).rem_euclid(2.0 * PI) - PI
}

fn ring(level: f64, width: f64) -> [f64; N] {
    std::array::from_fn(|k| level * bump(D[k] as f64 - 0.75, width))
}

fn cap(mut i: [f64; N]) -> [f64; N] {
    i.iter_mut().for_each(|x| *x = x.min(1.0));
    i
}

pub fn listen(t: f64) -> [f64; N] {
    let reveal = smooth(t / 0.9);
    let breath = 0.82 + 0.13 * (2.0 * PI * (t - 0.9).max(0.0) / 4.0).sin();
    let r = ring(breath, 0.6);
    let mut i: [f64; N] = std::array::from_fn(|k| {
        let from_bottom = angdiff(ANG[k] as f64, PI / 2.0).abs();
        r[k] * ((reveal * PI * 1.15 - from_bottom) / 0.9).clamp(0.0, 1.0)
    });
    let spark_gain = smooth((t - 0.9) / 2.0);
    if spark_gain > 0.0 {
        for (ang, start, speed) in SPARKS {
            let age = (t - start).rem_euclid(8.0);
            let d = 5.6 - speed * age;
            if d < 0.7 {
                continue;
            }
            let fade = smooth(age / 0.8) * smooth((d - 0.7) / 1.4);
            for k in 0..N {
                i[k] += 0.8 * spark_gain * fade * bump(D[k] as f64 - d, 0.8) * bump(angdiff(ANG[k] as f64, ang), 0.5);
            }
        }
    }
    cap(i)
}

fn comet(t: f64, period: f64, radius: f64, width: f64, direction: f64, tail: f64) -> [f64; N] {
    let head = direction * 2.0 * PI * t / period;
    std::array::from_fn(|k| {
        let behind = (direction * angdiff(head, ANG[k] as f64)).rem_euclid(2.0 * PI);
        (-behind / tail).exp() * bump(D[k] as f64 - radius, width)
    })
}

pub fn think(t: f64) -> [f64; N] {
    let mut i = ring(0.35, 0.6);
    let a = comet(t, 2.4, 0.75, 0.6, 1.0, 0.9);
    let b = comet(t, 4.8, 2.6, 0.8, -1.0, 0.7);
    let sb = smooth(t / 1.5);
    for k in 0..N {
        i[k] += 0.95 * a[k] + 0.7 * sb * b[k];
    }
    let g = smooth((t - 0.8) / 1.5);
    for &(idx, start, peak) in EMBERS.iter() {
        let age = (t - start as f64).rem_euclid(4.0);
        if age < 1.6 && D[idx] > 1.5 {
            i[idx] += g * 1.3 * peak as f64 * (PI * age / 1.6).sin().powi(2);
        }
    }
    cap(i)
}

pub fn search(t: f64) -> [f64; N] {
    let mut i = ring(0.3, 0.6);
    let head = 2.0 * PI * t / 3.0;
    for k in 0..N {
        let behind = angdiff(head, ANG[k] as f64).rem_euclid(2.0 * PI);
        i[k] += 0.9 * (-behind / 1.6).exp() * bump(D[k] as f64 - 0.75, 0.7);
    }
    let g = smooth((t - 0.3) / 1.2);
    for n in 0..2 {
        let age = (t - n as f64 * 1.1).rem_euclid(2.2);
        let r = 0.75 + 2.4 * age;
        let fade = smooth(age / 0.4) * (1.0 - smooth((age - 1.2) / 1.0));
        for k in 0..N {
            i[k] += g * 0.45 * fade * bump(D[k] as f64 - r, 0.7);
        }
    }
    cap(i)
}

pub fn speak(level: f64, react: f64) -> [f64; N] {
    let mut i = ring(1.0 - react * 0.6 * (1.0 - level), 0.6 + 0.2 * react * level);
    let glow = 0.35 * react * smooth((level - 0.4) / 0.6);
    for k in 0..N {
        i[k] += glow * bump(D[k] as f64 - 2.0, 0.85);
    }
    cap(i)
}

pub fn statik() -> [f64; N] {
    ring(0.95, 0.6)
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum State {
    Off,
    Listen,
    Think,
    Search,
    Speak,
    Music,
    Static,
}

impl State {
    pub fn parse(s: &str) -> Option<State> {
        Some(match s {
            "off" => State::Off,
            "listen" => State::Listen,
            "think" => State::Think,
            "search" => State::Search,
            "speak" => State::Speak,
            "music" => State::Music,
            "static" => State::Static,
            _ => return None,
        })
    }
}

/// The panel's look, as the server sends it.
#[derive(Clone, Debug)]
pub struct Look {
    pub color: [f64; 3],
    pub logo_color: [f64; 3],
    pub logo: f64,
    pub gain: f64,
    pub gamma: f64,
    pub backlight: f64,
    pub react: f64,
    pub smooth: f64,
    pub speed: f64,
    pub music_react: f64,
    pub music_lag: f64,
    pub night: bool,
}

impl Default for Look {
    fn default() -> Look {
        Look {
            color: [255.0, 48.0, 0.0],
            logo_color: [255.0, 48.0, 0.0],
            logo: 0.85,
            gain: 1.0,
            gamma: 1.5,
            backlight: 70.0,
            react: 0.5,
            smooth: 0.6,
            speed: 1.0,
            music_react: 0.7,
            music_lag: 0.0,
            night: false,
        }
    }
}

/// The loudness of the voice as it plays: RMS per 20 ms in dB → 0..1 (−32..−10 dB: speech is about −26 in the middle,
/// −11 at its loudest), quick to rise, slower to fall; `smooth` 0..1 slows both.
pub struct Envelope {
    hop: usize,
    acc: f64,
    n: usize,
    v: f64,
}

impl Envelope {
    pub fn new(rate: u32) -> Envelope {
        Envelope { hop: (rate / 50) as usize, acc: 0.0, n: 0, v: 0.0 }
    }
    /// samples in; a level out for every full 20 ms
    pub fn push(&mut self, pcm: &[i16], smooth_p: f64, mut out: impl FnMut(f64)) {
        let (attack, release) = (0.06 + 0.34 * (1.0 - smooth_p), 0.02 + 0.13 * (1.0 - smooth_p));
        for &s in pcm {
            let x = s as f64 / 32768.0;
            self.acc += x * x;
            self.n += 1;
            if self.n == self.hop {
                let rms = (self.acc / self.hop as f64).sqrt();
                let db = 20.0 * (rms + 1e-9).log10();
                let x = ((db + 32.0) / 22.0).clamp(0.0, 1.0);
                self.v += if x > self.v { attack } else { release } * (x - self.v);
                out(self.v);
                self.acc = 0.0;
                self.n = 0;
            }
        }
    }
}

/// What the states want shown at a moment, before colour: the backlight, the logo's brightness, how much of the
/// music's stars is mixed in, and each dot's intensity — all in 0..255.
pub struct Draw {
    pub backlight: u8,
    pub logo: u8,
    pub music: u8,
    pub dots: [u8; N],
}

impl Draw {
    /// Nothing lit, under `backlight`.
    pub fn blank(backlight: i32) -> Draw {
        Draw { backlight: backlight.clamp(1, 255) as u8, logo: 0, music: 0, dots: [0; N] }
    }
}

/// The states with their crossfades, the logo and the backlight, for any moment (monotonic s).
pub struct Engine {
    pub look: Look,
    state: State,
    layers: Vec<(State, f64, f64)>, // (state, since, switched at); newest last
    logo: f64,
    bl: f64,
    bl0: f64,
    last: f64,
    idle: bool,
    voice: std::collections::VecDeque<(f64, f64)>, // (heard at, level) of the voice, every 20 ms
}

impl Engine {
    pub fn new(bl0: i32, now: f64) -> Engine {
        let bl0 = if bl0 >= 0 { bl0 as f64 } else { 15.0 };
        Engine {
            look: Look::default(),
            state: State::Off,
            layers: vec![(State::Off, now, now - XFADE)],
            logo: 0.0,
            bl: bl0,
            bl0,
            last: now,
            idle: true,
            voice: std::collections::VecDeque::new(),
        }
    }

    pub fn set(&mut self, s: State, now: f64) {
        if s == self.state {
            return;
        }
        self.state = s;
        self.layers.push((s, now, now));
        if self.layers.len() > 4 {
            self.layers.remove(0); // a fifth switch within 0.6 s: the oldest is long covered
        }
        self.idle = false;
    }

    /// The loudness of the voice, 20 ms apart, the first heard at `t0`.
    pub fn voice(&mut self, t0: f64, levels: &[f64]) {
        for (j, &v) in levels.iter().enumerate() {
            self.voice.push_back((t0 + j as f64 * 0.02, v));
        }
        while self.voice.len() > 3000 {
            self.voice.pop_front();
        }
    }

    fn level(&mut self, now: f64) -> f64 {
        while self.voice.len() >= 2 && self.voice[1].0 <= now {
            self.voice.pop_front();
        }
        match (self.voice.front(), self.voice.get(1)) {
            (Some(&(t0, v0)), Some(&(t1, v1))) if t0 <= now => {
                v0 + (v1 - v0) * ((now - t0) / (t1 - t0)).clamp(0.0, 1.0)
            }
            (Some(&(t0, v0)), None) if now - t0 < 0.05 => v0,
            _ => 0.0,
        }
    }

    fn render(&mut self, s: State, t: f64, now: f64) -> [f64; N] {
        let sp = self.look.speed;
        match s {
            State::Listen => listen(t * sp),
            State::Think => think(t * sp),
            State::Search => search(t * sp),
            State::Speak => {
                let l = self.level(now);
                speak(l, self.look.react)
            }
            State::Static => statik(),
            State::Music | State::Off => [0.0; N],
        }
    }

    /// The frame for `now`; None once everything has faded out and the backlight is back (the panel is left alone
    /// then).
    pub fn frame(&mut self, now: f64) -> Option<Draw> {
        let dt = (now - self.last).clamp(0.0, 0.2);
        self.last = now;
        let k = self.layers.iter().enumerate().filter(|(_, l)| now - l.2 >= XFADE).map(|(j, _)| j).max().unwrap_or(0);
        self.layers.drain(..k);
        let layers = self.layers.clone();
        let (st, since, _) = layers[0];
        let mut img = self.render(st, now - since, now);
        let mut w = if st == State::Music { 1.0 } else { 0.0 };
        let mut a = 1.0;
        for &(st, since, sw) in &layers[1..] {
            a = smooth((now - sw) / XFADE);
            let im = self.render(st, now - since, now);
            for j in 0..N {
                img[j] = (1.0 - a) * img[j] + a * im[j];
            }
            w = (1.0 - a) * w + a * if st == State::Music { 1.0 } else { 0.0 };
        }
        let active = self.state != State::Off;
        let p = &self.look;
        let logo_target = if self.state == State::Listen { p.logo } else { 0.0 };
        self.logo = approach(self.logo, logo_target, dt / LOGO_RAMP * p.logo.max(0.05));
        let bl_target = if active { p.backlight } else { self.bl0 };
        self.bl = approach(self.bl, bl_target, dt / BL_RAMP * (p.backlight - self.bl0).abs().max(1.0));
        let faded = !active && a >= 1.0 && self.logo <= 0.0 && (self.bl - self.bl0).abs() < 0.5;
        if faded && self.idle {
            return None;
        }
        self.idle = faded; // this last frame is blank with the old backlight; then nothing
        Some(Draw {
            backlight: (self.bl.round() as i64).clamp(1, 255) as u8,
            logo: (255.0 * self.logo).round() as u8,
            music: (255.0 * w).round().clamp(0.0, 255.0) as u8,
            dots: img.map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8),
        })
    }
}

fn approach(v: f64, target: f64, step: f64) -> f64 {
    if v < target {
        (v + step).min(target)
    } else {
        (v - step).max(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animations_match_their_reference() {
        let v = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/anims.vec")).unwrap();
        assert_eq!(&v[..4], b"RANV");
        let n = u32::from_le_bytes(v[4..8].try_into().unwrap()) as usize;
        let mut worst = 0f64;
        let mut p = 8;
        for _ in 0..n {
            let k = u32::from_le_bytes(v[p..p + 4].try_into().unwrap());
            let a = f32::from_le_bytes(v[p + 4..p + 8].try_into().unwrap()) as f64;
            let b = f32::from_le_bytes(v[p + 8..p + 12].try_into().unwrap()) as f64;
            p += 12;
            let want: Vec<f64> =
                v[p..p + 4 * N].as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c) as f64).collect();
            p += 4 * N;
            let got = match k {
                1 => listen(a),
                2 => think(a),
                3 => search(a),
                4 => speak(a, b),
                5 => statik(),
                _ => panic!("kind {k}"),
            };
            for j in 0..N {
                let d = (got[j] - want[j]).abs();
                assert!(d < 1e-4, "kind {k} at {a}/{b}, dot {j}: {} vs {}", got[j], want[j]);
                worst = worst.max(d);
            }
        }
        eprintln!("animations: {n} frames, worst dot ±{worst:.1e}");
    }

    #[test]
    fn the_envelope_of_a_rising_sine() {
        // at 24 kHz with smooth 0.6: every 10th value, as the reference the animations were tuned with gives them
        let want = [0.0, 0.0, 0.140975, 0.308869, 0.43247, 0.537007, 0.618159, 0.676724, 0.732081, 0.78781];
        let n = 48000;
        let y: Vec<i16> = (0..n)
            .map(|i| {
                let x = (400.0 * i as f64 / (n - 1) as f64).sin() * (0.3 * i as f64 / (n - 1) as f64);
                ((x as f32) * 32768.0) as i16
            })
            .collect();
        let mut e = Envelope::new(24000);
        let mut got = vec![];
        e.push(&y, 0.6, |v| got.push(v));
        assert_eq!(got.len(), 100);
        for (j, w) in want.iter().enumerate() {
            assert!((got[j * 10] - w).abs() < 2e-3, "envelope {j}: {} vs {w}", got[j * 10]);
        }
    }

    #[test]
    fn crossfades_and_going_quiet() {
        let mut e = Engine::new(15, 0.0);
        assert!(e.frame(0.0).is_none(), "off and faded: nothing to draw");
        e.set(State::Listen, 1.0);
        let f = e.frame(1.3).unwrap();
        assert!(f.backlight > 15, "the backlight glides up");
        e.set(State::Off, 2.0);
        let mut t = 2.0;
        let mut last = None;
        while t < 5.0 {
            t += 0.02;
            match e.frame(t) {
                Some(f) => last = Some(f),
                None => break,
            }
        }
        assert!(t < 5.0, "it goes quiet once faded");
        let l = last.unwrap();
        assert_eq!(l.backlight, 15, "the backlight is back");
        assert!(l.dots.iter().all(|&x| x == 0), "the last frame is blank");
    }
}
