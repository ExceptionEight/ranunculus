//! The music, 47 times a second, as a few slow numbers. For each hop of mono samples (int16 scale):
//! - the levels of three bands — bass 30–150 Hz, mid 150–2000 Hz, high 2–12 kHz: band energy in dB over a
//!   2048-sample Hann window, scaled by an automatic gain (a peak that falls ~1 dB/s, a 30 dB window under it), quick
//!   to rise, slower to fall;
//! - onsets (every star of the panel is a sound): per band, the rise of the log spectrum over the last hop (positive
//!   spectral flux), a peak above its own recent average × K (adaptive: loud and quiet tracks alike), confirmed one
//!   hop later (a local maximum — the music reaches the station ~0.35 s before it sounds, so the delay costs
//!   nothing), at most one per band per REFRACTORY s. Like AlexGyver's ColorMusic «flash when the band jumps above
//!   its average», on rises instead of levels. Each onset also carries its peak over the band's median flux of the
//!   last ~6 s (`onsets_abs`): in a ballad the relative threshold passes the ripples of a held chord as often as
//!   drums, so the panel gates on this scale when the music is calm (anim.rs).
use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::collections::VecDeque;
use std::sync::Arc;

const BANDS: [(f64, f64); 3] = [(30.0, 150.0), (150.0, 2000.0), (2000.0, 12000.0)];

/// How long before the end of the current hop an onset of the previous hop sounds: the hop before's centre
/// (1.5 hops) and the window's own lag (stars came ~12 ms late on the test tracks).
pub const ONSET_BACK: f64 = 1.5 * 1024.0 / 48000.0 + 0.012;

const K: [f64; 3] = [1.55, 1.45, 1.45]; // a peak counts above its band's recent mean flux × this
const REFRACTORY: [f64; 3] = [0.16, 0.11, 0.07];
const FLUX_HIST: usize = 24; // hops of recent flux (~0.5 s)
/// The track's own scale: the threshold above is relative to the last 0.5 s, so in smooth music the small ripples
/// of a held chord or a voice pass it as often as drums do — a ballad would light as many stars as a dance track
/// (~10/s). Each onset also gets its peak over the band's typical flux of the last LONG_HIST hops
/// (their median): the panel keeps only the clear ones in calm music (anim.rs), and a peak under GATE× is no onset.
const LONG_HIST: usize = 282; // ~6 s
const GATE: f64 = 1.5;

pub struct Analyzer {
    win: usize,
    dt: f64,
    buf: Vec<f64>,
    hann: Vec<f64>,
    fft: Arc<dyn Fft<f64>>,
    scratch: Vec<Complex<f64>>,
    spec: Vec<f64>,
    band_bins: [(usize, usize); 3],
    peak: [f64; 3],
    val: [f64; 3],
    prev_lm: Option<Vec<f64>>,
    bflux: [VecDeque<f64>; 3], // recent per-band flux, the newest last
    lflux: [VecDeque<f64>; 3], // the same over LONG_HIST hops
    base: [f64; 3],            // their median, refreshed every 8 hops
    hops: u64,
    since_on: [f64; 3],
}

/// One hop's result: the slow band levels, and the onsets of the previous hop (strength 0..1 per band, 0: none) —
/// the hop the peak is in, confirmed now.
pub struct Out {
    pub bass: f32,
    pub mid: f32,
    pub high: f32,
    pub onsets_prev: [f32; 3],
    /// each onset's peak over the band's median flux of the last ~6 s (0: none)
    pub onsets_abs: [f32; 3],
}

/// Bins k with lo <= k*sr/win < hi, as a half-open range.
fn bins(sr: f64, win: usize, lo: f64, hi: f64) -> (usize, usize) {
    let f = |k: usize| k as f64 * sr / win as f64;
    let n = win / 2 + 1;
    let a = (0..n).find(|&k| f(k) >= lo).unwrap_or(n);
    let b = (0..n).find(|&k| f(k) >= hi).unwrap_or(n);
    (a, b)
}

impl Analyzer {
    pub fn new(sr: f64, hop: usize, win: usize) -> Self {
        let hann =
            (0..win).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (win - 1) as f64).cos()).collect();
        let fft = FftPlanner::new().plan_fft_forward(win);
        let band_bins = [
            bins(sr, win, BANDS[0].0, BANDS[0].1),
            bins(sr, win, BANDS[1].0, BANDS[1].1),
            bins(sr, win, BANDS[2].0, BANDS[2].1),
        ];
        Analyzer {
            win,
            dt: hop as f64 / sr,
            buf: vec![0.0; win],
            hann,
            fft,
            scratch: vec![Complex::new(0.0, 0.0); win],
            spec: vec![0.0; win / 2 + 1],
            band_bins,
            peak: [-60.0; 3],
            val: [0.0; 3],
            prev_lm: None,
            bflux: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
            lflux: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
            base: [0.0; 3],
            hops: 0,
            since_on: [1e9; 3],
        }
    }

    /// One hop of mono samples, int16 scale.
    pub fn push(&mut self, mono: &[f32]) -> Out {
        let n = mono.len().min(self.win);
        self.buf.rotate_left(n);
        let w = self.win;
        for (j, &s) in mono[mono.len() - n..].iter().enumerate() {
            self.buf[w - n + j] = s as f64 / 32768.0;
        }
        for i in 0..w {
            self.scratch[i] = Complex::new(self.buf[i] * self.hann[i], 0.0);
        }
        self.fft.process(&mut self.scratch);
        for k in 0..self.spec.len() {
            self.spec[k] = self.scratch[k].norm() / w as f64;
        }
        let mut out = [0f64; 3];
        for b in 0..3 {
            let (lo, hi) = self.band_bins[b];
            let e = 10.0 * (self.spec[lo..hi].iter().map(|x| x * x).sum::<f64>() + 1e-12).log10();
            self.peak[b] = e.max(self.peak[b] - 1.0 * self.dt).max(-70.0);
            let v = ((e - (self.peak[b] - 30.0)) / 30.0).clamp(0.0, 1.0).powf(1.5);
            let a = if v > self.val[b] { 0.5 } else { 0.08 };
            self.val[b] += a * (v - self.val[b]);
            out[b] = self.val[b];
        }
        let (onsets_prev, onsets_abs) = self.onsets(&out);
        Out { bass: out[0] as f32, mid: out[1] as f32, high: out[2] as f32, onsets_prev, onsets_abs }
    }

    fn onsets(&mut self, level: &[f64; 3]) -> ([f32; 3], [f32; 3]) {
        let lm: Vec<f64> = self.spec.iter().map(|x| (1.0 + 1000.0 * x).ln()).collect();
        let mut res = [0f32; 3];
        let mut abs = [0f32; 3];
        self.hops += 1;
        for b in 0..3 {
            self.since_on[b] += self.dt;
            let (lo, hi) = self.band_bins[b];
            let f = match &self.prev_lm {
                None => 0.0,
                Some(p) => (lo..hi).map(|k| (lm[k] - p[k]).max(0.0)).sum::<f64>() / (hi - lo).max(1) as f64,
            };
            let l = &mut self.lflux[b];
            l.push_back(f);
            while l.len() > LONG_HIST {
                l.pop_front();
            }
            if self.hops % 8 == 1 || self.base[b] == 0.0 {
                let mut v: Vec<f64> = l.iter().cloned().collect();
                let mid = v.len() / 2;
                v.select_nth_unstable_by(mid, |a, b| a.partial_cmp(b).unwrap());
                self.base[b] = v[mid];
            }
            let h = &mut self.bflux[b];
            h.push_back(f);
            while h.len() > FLUX_HIST {
                h.pop_front();
            }
            if h.len() >= 6 {
                let n = h.len();
                let (p2, p1, now) = (h[n - 3], h[n - 2], h[n - 1]);
                let mean = h.iter().take(n - 1).sum::<f64>() / (n - 1) as f64; // without the newest
                let thr = K[b] * mean + 1e-4;
                // the previous hop is a local peak above the threshold, the band is not silent, not too soon
                let r = p1 / (self.base[b] + 1e-4); // on the track's scale
                if p1 > thr
                    && p1 >= p2
                    && p1 > now
                    && level[b] > 0.12
                    && self.since_on[b] - self.dt > REFRACTORY[b]
                    && r > GATE
                {
                    res[b] = (0.2 + 0.8 * ((p1 / thr - 1.0) / 1.5).clamp(0.0, 1.0)) as f32;
                    abs[b] = r as f32;
                    self.since_on[b] = self.dt; // the onset was one hop ago
                }
            }
        }
        self.prev_lm = Some(lm);
        (res, abs)
    }
}
