//! The music on the panel, «созвездие»: every star is a sound (dots lit at a rate set by the music look like lights at
//! random moments):
//! - the analyzer finds onsets per band (analyzer.rs) — kick-like rises in the lows, snares/chords/syllables in the
//!   mids, hats in the highs — and each one lights stars at the very moment it is heard: the music reaches the
//!   station ~0.35 s before it sounds, so an onset is known before its time and the star is born exactly on it;
//! - each band has its zone, as in AlexGyver's colour music: the lows bloom on the ring around the centre button (with a
//!   wide glow), the mids in the middle belt, the highs twinkle at the edges — so the eye can tell what lit what;
//! - two accent colours that go together (PAIRS): the lows the first, the highs the second, the mids either (a
//!   50/50 mix of two saturated colours comes out pale and blurs the pair);
//!   the pair is new for each music session and drifts to another every PAIR_S s;
//! - a star is at its peak at once and fades (lows slower, highs fast), brightness by the onset's strength, capped.
//!   Silence and steady sound: dark.
//! - calm music is drawn calm: what suits dance tracks flickers too much on a ballad before sleep. `calm` 0..1 comes
//!   from how much the band levels move (dance music jumps with every kick and hat, a ballad swells and ebbs; the
//!   count of onsets is the same ~10/s for both and tells nothing). The calmer, the more:
//!   - only clear onsets light stars (their peak over the band's flux of the last ~6 s: GATE_DYN..GATE_CALM),
//!     softer, slower to rise, up to 3× slower to fade;
//!   - «дыхание»: a soft glow that follows each band's loudness in its zone, swirling slowly in the two colours —
//!     the swells of a ballad; dynamic music has none of it.
//!
//! At night (the look's flag) all of it is another 1.5× slower and a quarter dimmer.
use super::tables::{ANG, D, N, NEIGH};
use std::collections::VecDeque;

/// Accent pairs in the LEDs' own drive values (green is strong on these LEDs, so it is kept low). Each pair is
/// either neighbours on the colour wheel or a split complement — chosen to sit well together, no mud.
pub const PAIRS: [([f32; 3], [f32; 3], &str); 10] = [
    ([170.0, 0.0, 255.0], [255.0, 30.0, 60.0], "фиолетовый + коралл"),
    ([0.0, 150.0, 140.0], [70.0, 0.0, 255.0], "бирюза + индиго"),
    ([255.0, 80.0, 0.0], [255.0, 0.0, 90.0], "янтарь + роза"),
    ([0.0, 130.0, 255.0], [150.0, 0.0, 255.0], "небесный + фиалка"),
    ([255.0, 0.0, 110.0], [255.0, 70.0, 0.0], "малина + оранж"),
    ([150.0, 60.0, 255.0], [255.0, 90.0, 50.0], "лаванда + персик"),
    ([0.0, 170.0, 60.0], [255.0, 130.0, 0.0], "изумруд + золото"),
    ([30.0, 50.0, 255.0], [255.0, 15.0, 140.0], "электрик + неон-розовый"),
    ([255.0, 35.0, 0.0], [170.0, 0.0, 255.0], "закат: алый + пурпур"),
    ([0.0, 170.0, 200.0], [255.0, 40.0, 120.0], "лагуна + фуксия"),
];
pub const PAIR_S: f64 = 40.0; // s with one pair, then a 5 s drift to the next
const DRIFT_S: f64 = 5.0;
pub const CAP: f32 = 0.6; // brightest a dot gets (before the gamma curve)
const MAX_SPARKS: usize = 30;
/// per band (low, mid, high): zone (D from..to), peak base + strength part, fade s (± a quarter), glow share on the
/// neighbours
const ZONE: [(f32, f32); 3] = [(0.0, 1.3), (1.3, 2.6), (2.6, 9.0)];
const PEAK: [(f64, f64); 3] = [(0.34, 0.26), (0.28, 0.22), (0.2, 0.2)];
const FADE: [f64; 3] = [0.45, 0.3, 0.17];
const GLOW: [f32; 3] = [0.38, 0.22, 0.1];
/// calm, from how the bass, mid and high levels moved over the last MOTION_S: their Σ|Δ| per s (M) and how many
/// times a second one of them jumped by RISE within 3 hops (R) — dance music pumps with every kick and chug, a
/// ballad swells, with small ripples between (the ripples alone, M, put a ballad's busier part next to metal).
/// score = (M / M_REF + R / R_REF) / 2: SCORE_CALM or less is calm 1, SCORE_DYN or more 0. On 8-s windows of 7 slow
/// and 7 fast tracks 97 % fall on the right side of 1.0.
const M_REF: f64 = 2.4;
const R_REF: f64 = 4.0;
const RISE: f32 = 0.1;
const SCORE_CALM: f64 = 0.95;
const SCORE_DYN: f64 = 1.2;
const MOTION_S: f64 = 8.0;
/// a star needs an onset this many times over its band's median flux: dynamic music keeps nearly all, calm only
/// the clear ones (a strum, a piano note, a kick — not the ripple of a held chord)
const GATE_DYN: f64 = 1.8;
const GATE_CALM: f64 = 4.0;
/// «дыхание»: how bright the glow gets (as a star's lum: under the stars, which come on top), how fast it follows
/// the band levels up and down (s), and the slow swirl (s per turn)
const BREATH: f64 = 0.45;
const BREATH_UP: f64 = 0.12;
const BREATH_DOWN: f64 = 0.7;
const SWIRL_S: f64 = 16.0;

/// xorshift64*: enough randomness for where a star lands, no crates.
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }
    pub fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545F4914F6CDD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }
}

pub struct Spark {
    pub dot: usize,
    pub band: usize,
    colour: usize, // 0: the pair's first colour, 1: the second
    pub born: f64,
    attack: f64,
    pub fade: f64,
    peak: f64,
}

impl Spark {
    fn level(&self, now: f64) -> f64 {
        let t = now - self.born;
        if t < 0.0 {
            0.0
        } else if t < self.attack {
            let x = t / self.attack; // dynamic music: one frame up, the peak on the onset; calm: a soft rise
            self.peak * x * x * (3.0 - 2.0 * x)
        } else {
            let x = ((t - self.attack) / self.fade).min(1.0);
            self.peak * (1.0 - x) * (1.0 - x) // then down, softly at the end
        }
    }
    fn done(&self, now: f64) -> bool {
        now - self.born > self.attack + self.fade
    }
}

/// The music as the station heard it (frames and onsets from the tap) and the stars drawn from it.
pub struct Music {
    frames: VecDeque<(f64, f32, f32, f32)>, // band levels per hop (t heard), waiting for their moment
    onsets: VecDeque<(f64, usize, f32, f32)>, // (t heard, band, strength, peak over the median flux), the same
    pub sparks: Vec<Spark>,
    rng: Rng,
    zones: [Vec<usize>; 3],
    pair: usize,
    next_pair: usize,
    pair_since: f64,
    last_lvl: Option<(f64, [f32; 3])>, // the levels heard last
    lvl_hist: VecDeque<[f32; 3]>,      // the last 4 of them: a jump within 3 hops
    rising: [bool; 3],
    motion: VecDeque<(f64, f64, u32)>, // (t, Σ|Δ| of the levels, jumps) of the last MOTION_S: how the music moves
    pub score: f64,
    pub calm: f64,
    env: [f64; 3],   // the glow's band envelopes
    floor: [f64; 3], // and their slow minimum: what the song holds all along
    glow: f64,       // its weight × mean envelope, for eval
    swirl: f64,
    last_look: f64,
}

impl Music {
    pub fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let pair = (rng.next() * PAIRS.len() as f64) as usize % PAIRS.len();
        let zones = [0, 1, 2].map(|b| (0..N).filter(|&k| D[k] >= ZONE[b].0 && D[k] < ZONE[b].1).collect::<Vec<_>>());
        Music {
            frames: VecDeque::new(),
            onsets: VecDeque::new(),
            sparks: vec![],
            rng,
            zones,
            pair,
            next_pair: pair,
            pair_since: 0.0,
            last_lvl: None,
            lvl_hist: VecDeque::new(),
            rising: [false; 3],
            motion: VecDeque::new(),
            score: 0.0,
            calm: 0.0,
            env: [0.0; 3],
            floor: [0.0; 3],
            glow: 0.0,
            swirl: 0.0,
            last_look: 0.0,
        }
    }

    pub fn push(&mut self, t: f64, bass: f32, mid: f32, high: f32) {
        self.frames.push_back((t, bass, mid, high));
        while self.frames.len() > 400 {
            self.frames.pop_front();
        }
    }

    /// Onsets of one hop (strength per band, 0: none; peak over the band's median flux, f32::MAX: not known — an
    /// older tap), heard at t.
    pub fn push_onsets(&mut self, t: f64, s: [f32; 3], r: [f32; 3]) {
        for b in 0..3 {
            if s[b] > 0.0 {
                self.onsets.push_back((t, b, s[b], r[b]));
            }
        }
        while self.onsets.len() > 256 {
            self.onsets.pop_front();
        }
    }

    /// The music state has (re)started: a fresh pair, no leftovers.
    pub fn start(&mut self, now: f64) {
        self.pair = (self.rng.next() * PAIRS.len() as f64) as usize % PAIRS.len();
        self.next_pair = self.pair;
        self.pair_since = now;
        self.sparks.clear();
        self.onsets.retain(|o| o.0 > now - 0.05);
        self.frames.retain(|f| f.0 > now - 0.05);
        self.motion.clear();
        self.last_lvl = None;
        self.lvl_hist.clear();
        self.env = [0.0; 3];
        self.floor = [0.0; 3];
        self.last_look = now;
    }

    fn light(&mut self, born: f64, band: usize, s: f64, react: f64, night: bool) {
        let n = if s > 0.65 && band < 2 { 2 } else { 1 };
        for _ in 0..n {
            if self.sparks.len() >= MAX_SPARKS {
                self.sparks.remove(0); // the oldest makes room
            }
            let zone = &self.zones[band];
            let mut dot = zone[0];
            for _ in 0..8 {
                // a dot of the band's zone that is not lit right now
                dot = zone[(self.rng.next() * zone.len() as f64) as usize % zone.len()];
                if !self.sparks.iter().any(|x| x.dot == dot && x.level(born) > 0.05) {
                    break;
                }
            }
            let slow = (1.0 + 2.0 * self.calm) * if night { 1.5 } else { 1.0 };
            let fade = FADE[band] * self.rng.range(0.75, 1.25) * slow;
            let attack = 0.02 + 0.13 * self.calm + if night { 0.05 } else { 0.0 };
            let peak = (PEAK[band].0 + PEAK[band].1 * s)
                * (0.7 + 0.3 * react)
                * (1.0 - 0.15 * self.calm)
                * if night { 0.75 } else { 1.0 };
            let colour = match band {
                0 => 0,
                2 => 1,
                _ => (self.rng.next() < 0.5) as usize,
            };
            self.sparks.push(Spark { dot, band, colour, born, attack, fade, peak });
        }
    }

    /// Advance to `look` (the moment this frame shows) and draw: RGB 0..255 per dot.
    /// react 0..1: how strong the stars are; gain/gamma: the look's brightness curve.
    pub fn render(&mut self, look: f64, since: f64, react: f64, gain: f64, gamma: f64, night: bool) -> [[f32; 3]; N] {
        let r = react.clamp(0.0, 1.0);
        let dt = (look - self.last_look).clamp(0.0, 0.2);
        self.last_look = look;
        // the levels heard by now: how much they move sets calm
        while let Some(&(t, b, m, h)) = self.frames.front() {
            if t > look {
                break;
            }
            self.frames.pop_front();
            let l = [b, m, h];
            self.lvl_hist.push_back(l);
            if self.lvl_hist.len() > 4 {
                self.lvl_hist.pop_front();
            }
            let mut jumps = 0;
            if self.lvl_hist.len() == 4 {
                for j in 0..3 {
                    let up = l[j] - self.lvl_hist[0][j] > RISE;
                    jumps += (up && !self.rising[j]) as u32; // the start of a jump
                    self.rising[j] = up;
                }
            }
            if let Some((_, p)) = self.last_lvl {
                self.motion.push_back((t, (0..3).map(|j| (l[j] - p[j]).abs() as f64).sum(), jumps));
            }
            self.last_lvl = Some((t, l));
        }
        while self.motion.front().is_some_and(|x| look - x.0 > MOTION_S) {
            self.motion.pop_front();
        }
        let covered = (look - since).min(MOTION_S);
        if covered > 1.5 {
            let mv = self.motion.iter().map(|x| x.1).sum::<f64>() / covered;
            let jumps = self.motion.iter().map(|x| x.2).sum::<u32>() as f64 / covered;
            self.score = 0.5 * (mv / M_REF + jumps / R_REF);
            let target = ((SCORE_DYN - self.score) / (SCORE_DYN - SCORE_CALM)).clamp(0.0, 1.0);
            // drifts over a few seconds, no jumps; faster while the window fills (a new session: the old calm is
            // another song's — a ballad flickered its first ~8 s)
            let tau = if covered < MOTION_S { 1.2 } else { 3.0 };
            self.calm += (target - self.calm) * (dt / tau).min(1.0);
        }
        let fade_in = ((look - since) / 0.8).clamp(0.0, 1.0); // the first 0.8 s fade in
        let gate = GATE_DYN + (GATE_CALM - GATE_DYN) * self.calm;
        while let Some(&(t, band, s, peak)) = self.onsets.front() {
            if t > look {
                break; // not heard yet: wait for its moment
            }
            self.onsets.pop_front();
            if look - t < 0.15 && t >= since && peak as f64 >= gate {
                // calm music: the strength on the track's own scale (a soft strum dim, a kick bright), not the last 0.5 s's
                let sabs = if peak == f32::MAX {
                    s as f64
                } else {
                    0.2 + 0.8 * ((peak as f64 / gate).ln() / 4f64.ln()).clamp(0.0, 1.0)
                };
                let st = s as f64 + (sabs - s as f64) * self.calm;
                self.light(t, band, st * fade_in, r, night); // born on the onset itself, even if this frame is a bit late
            }
        }
        // «дыхание»: the band envelopes, eased (no frames for a moment — the music stopped: down to dark)
        let stale = self.last_lvl.is_none_or(|(t, _)| look - t > 0.25);
        for j in 0..3 {
            let x = if stale { 0.0 } else { self.last_lvl.unwrap().1[j] as f64 };
            let tau = if x > self.env[j] { BREATH_UP } else { BREATH_DOWN } * if night { 1.5 } else { 1.0 };
            self.env[j] += (x - self.env[j]) * (1.0 - (-dt / tau).exp());
            // down at once, up over ~4 s: a piano that never stops kept the whole panel lit near full (Einaudi,
            // levels 0.75–1 all along) — the glow is the swell over what the song holds
            self.floor[j] = if self.env[j] < self.floor[j] {
                self.env[j]
            } else {
                self.floor[j] + (self.env[j] - self.floor[j]) * (dt / 4.0).min(1.0)
            };
        }
        let c = self.calm;
        let weight = c * c * (3.0 - 2.0 * c) * fade_in * (0.7 + 0.3 * r) * if night { 0.75 } else { 1.0 };
        self.swirl = (self.swirl + dt * std::f64::consts::TAU / SWIRL_S * if night { 0.67 } else { 1.0 })
            % std::f64::consts::TAU;
        self.glow = weight * (self.env[0] + self.env[1] + self.env[2]) / 3.0;
        if self.pair == self.next_pair && look - self.pair_since > PAIR_S {
            let mut nxt = (self.rng.next() * PAIRS.len() as f64) as usize % PAIRS.len();
            if nxt == self.pair {
                nxt = (nxt + 1) % PAIRS.len();
            }
            self.next_pair = nxt;
            self.pair_since = look;
        }
        let drift =
            if self.pair != self.next_pair { ((look - self.pair_since) / DRIFT_S).clamp(0.0, 1.0) } else { 0.0 };
        if self.pair != self.next_pair && drift >= 1.0 {
            self.pair = self.next_pair;
            self.pair_since = look;
        }
        let mix =
            |a: [f32; 3], z: [f32; 3], x: f64| [0, 1, 2].map(|j| (a[j] as f64 * (1.0 - x) + z[j] as f64 * x) as f32);
        let smooth = |x: f64| {
            let x = x.clamp(0.0, 1.0);
            x * x * (3.0 - 2.0 * x)
        };
        let (p, q) = (PAIRS[self.pair], PAIRS[self.next_pair]);
        let first = mix(p.0, q.0, drift);
        let second = mix(p.1, q.1, drift);
        let tones = [first, second];
        let mut lum = [0f32; N];
        let mut rgb = [[0f32; 3]; N];
        if weight > 0.003 {
            // each band glows in its zone (soft edges), its envelope over a floor and a gentle curve (the LEDs'
            // gamma already presses the low end: a mid level gave 1–4/255 with a steeper one); a slow swirl
            // around the centre button moves the light and the colours
            let g = [0, 1, 2].map(|j| {
                let f = (0.6 * self.floor[j]).max(0.15);
                ((self.env[j] - f) / (1.0 - f)).clamp(0.0, 1.0).powf(1.2)
            });
            for k in 0..N {
                let d = D[k] as f64;
                let mut x = 0.0;
                let mut w2 = 0.0; // the second colour's share
                for b in 0..3 {
                    let (lo, hi) = (ZONE[b].0 as f64, ZONE[b].1.min(5.0) as f64);
                    let m = smooth((d - lo + 0.5) / 1.0) * smooth((hi + 0.5 - d) / 1.0);
                    x += m * g[b];
                    w2 += m * g[b] * [0.0, 0.5, 1.0][b];
                }
                if x <= 0.0 {
                    continue;
                }
                let a = ANG[k] as f64;
                let tex = 0.45 + 0.55 * (0.5 + 0.5 * (a - self.swirl + 0.7 * d).sin());
                let second = (w2 / x + 0.35 * (2.0 * a + self.swirl).sin()).clamp(0.0, 1.0) as f32;
                let v = (BREATH * weight * x.min(1.0) * tex) as f32;
                lum[k] += v;
                for j in 0..3 {
                    rgb[k][j] += v * (tones[0][j] * (1.0 - second) + tones[1][j] * second);
                }
            }
        }
        for s in &self.sparks {
            let v = s.level(look) as f32;
            if v <= 0.0 {
                continue;
            }
            let c = tones[s.colour];
            let mut add = |dot: usize, x: f32| {
                lum[dot] += x;
                for j in 0..3 {
                    rgb[dot][j] += x * c[j];
                }
            };
            add(s.dot, v);
            for &nb in NEIGH[s.dot].iter().filter(|&&nb| nb != usize::MAX) {
                add(nb, GLOW[s.band] * v);
            }
        }
        self.sparks.retain(|s| !s.done(look));
        let mut out = [[0f32; 3]; N];
        for k in 0..N {
            if lum[k] > 0.0 {
                let level = (lum[k].min(CAP) as f64).powf(gamma) * gain;
                for j in 0..3 {
                    out[k][j] = (rgb[k][j] / lum[k]) * level as f32;
                }
            }
        }
        out
    }
}
