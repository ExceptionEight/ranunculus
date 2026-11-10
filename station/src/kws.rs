//! The wake word, heard on the station: a small network of affine layers and filters over time, run frame by frame
//! over the microphone's stream. The model file (models/mrazota.rkw, trained from scratch on recordings of the word)
//! carries the front end too: the Kaldi fbank the network was trained with — DC removed, pre-emphasis 0.97, povey
//! window, |FFT 512|², 40 mel banks, log (floor = f32 epsilon), 25 ms windows every 10 ms. The network is strictly
//! causal (every filter over time ends at the current frame), so it runs on a stream with a ring of past inputs per
//! filter, and gives the same numbers as the batch computation it was trained with (tests/kws.vec).
//! The posterior p(word) is a moving average over the last `smooth` frames.
use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::sync::Arc;

#[derive(Debug)]
pub enum Layer {
    Affine {
        out: usize,
        inp: usize,
        w: Vec<f32>,
        b: Vec<f32>,
    },
    /// w: [taps][dim]; tap taps-1 is the current frame
    Conv {
        dim: usize,
        taps: usize,
        w: Vec<f32>,
        b: Vec<f32>,
    },
    Scale(Vec<f32>),
    Bias(Vec<f32>),
    Relu,
    Softmax,
}

pub struct Model {
    pub win: usize,
    pub hop: usize,
    pub nfft: usize,
    pub n_mels: usize,
    pub preemph: f32,
    pub window: Vec<f32>,
    pub banks: Vec<f32>, // n_mels × (nfft/2 + 1)
    pub smooth: usize,
    pub layers: Vec<Layer>,
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self.b.get(self.p..self.p + n).ok_or("the model file is truncated")?;
        self.p += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<usize, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()) as usize)
    }
    fn f32s(&mut self, n: usize) -> Result<Vec<f32>, String> {
        Ok(self.take(4 * n)?.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect())
    }
}

impl Model {
    pub fn parse(b: &[u8]) -> Result<Model, String> {
        let mut r = Rd { b, p: 0 };
        if r.take(4)? != b"RKWS" {
            return Err("not an .rkw model".into());
        }
        if r.u32()? != 1 {
            return Err("an .rkw of another version".into());
        }
        let (sr, win, hop, nfft, n_mels) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?, r.u32()?);
        if sr != 16000 || win > nfft || hop == 0 || n_mels == 0 {
            return Err(format!("unexpected front end: {sr} Hz, win {win}, hop {hop}, nfft {nfft}, {n_mels} mels"));
        }
        let preemph = f32::from_le_bytes(r.take(4)?.try_into().unwrap());
        let window = r.f32s(win)?;
        let banks = r.f32s(n_mels * (nfft / 2 + 1))?;
        let smooth = r.u32()?.max(1);
        let n = r.u32()?;
        let mut layers = Vec::with_capacity(n);
        let mut dim = n_mels;
        for i in 0..n {
            let kind = r.take(1)?[0];
            let l = match kind {
                1 => {
                    let (out, inp) = (r.u32()?, r.u32()?);
                    Layer::Affine { out, inp, w: r.f32s(out * inp)?, b: r.f32s(out)? }
                }
                2 => {
                    let (dim, taps) = (r.u32()?, r.u32()?);
                    let w = r.f32s(dim * taps)?; // [dim][taps] in the file; [taps][dim] here (contiguous per tap)
                    let mut wt = vec![0f32; dim * taps];
                    for c in 0..dim {
                        for j in 0..taps {
                            wt[j * dim + c] = w[c * taps + j];
                        }
                    }
                    Layer::Conv { dim, taps, w: wt, b: r.f32s(dim)? }
                }
                3 => {
                    let d = r.u32()?;
                    Layer::Scale(r.f32s(d)?)
                }
                4 => {
                    let d = r.u32()?;
                    Layer::Bias(r.f32s(d)?)
                }
                5 => Layer::Relu,
                6 => Layer::Softmax,
                k => return Err(format!("layer {i}: unknown kind {k}")),
            };
            // the dimensions must chain
            match &l {
                Layer::Affine { out, inp, .. } => {
                    if *inp != dim {
                        return Err(format!("layer {i}: affine takes {inp}, gets {dim}"));
                    }
                    dim = *out;
                }
                Layer::Conv { dim: d, .. } if *d != dim => {
                    return Err(format!("layer {i}: conv of {d}, gets {dim}"));
                }
                Layer::Scale(v) | Layer::Bias(v) if v.len() != dim => {
                    return Err(format!("layer {i}: {} values, gets {dim}", v.len()));
                }
                _ => {}
            }
            layers.push(l);
        }
        if !matches!(layers.last(), Some(Layer::Softmax)) || dim != 2 {
            return Err("the network must end in a softmax over 2 outputs (filler, word)".into());
        }
        if r.p != b.len() {
            return Err("trailing bytes after the model".into());
        }
        Ok(Model { win, hop, nfft, n_mels, preemph, window, banks, smooth, layers })
    }
}

/// The spotter's state on a stream: samples → frames → network → smoothed p(word), one value per 10 ms.
pub struct Spotter {
    m: Arc<Model>,
    fft: Arc<dyn Fft<f32>>,
    ring: Vec<f32>, // the last `win` samples (int16 scale), a ring; `pos` is where the next one goes
    pos: usize,
    buf: Vec<f32>,       // the current frame's window, in order
    have: usize,         // samples heard so far, up to win (a frame needs win)
    since: usize,        // new samples since the last frame
    hist: Vec<Vec<f32>>, // per conv layer: taps × dim, a ring
    head: Vec<usize>,
    a: Vec<f32>, // activations of the frames in a batch, [frame][widest]
    b: Vec<f32>,
    feats: Vec<f32>, // the batch's features, [frame][n_mels]
    spec: Vec<Complex<f32>>,
    ps: Vec<f32>, // the last `smooth` posteriors, a ring
    pi: usize,
    frames: u64,
    widest: usize,
}

/// Frames run through the network together: each weight row is read from memory once for all of them. The network
/// is memory-bound on the station (1.1 MB of weights a frame against a small cache: ~1 ms a frame one by one); a
/// 32-ms mic chunk is 3–4 frames, so batching them costs no latency.
const BATCH: usize = 8;

impl Spotter {
    pub fn new(m: Arc<Model>) -> Spotter {
        let fft = FftPlanner::new().plan_fft_forward(m.nfft);
        let hist = m
            .layers
            .iter()
            .map(|l| if let Layer::Conv { dim, taps, .. } = l { vec![0.0; dim * taps] } else { vec![] })
            .collect();
        let n = m.layers.len();
        let widest = m
            .layers
            .iter()
            .map(|l| match l {
                Layer::Affine { out, .. } => *out,
                Layer::Conv { dim, .. } => *dim,
                _ => 0,
            })
            .max()
            .unwrap_or(0)
            .max(m.n_mels);
        Spotter {
            fft,
            ring: vec![0.0; m.win],
            pos: 0,
            buf: vec![0.0; m.win],
            have: 0,
            since: 0,
            hist,
            head: vec![0; n],
            a: vec![0.0; widest * BATCH],
            b: vec![0.0; widest * BATCH],
            feats: Vec::with_capacity(BATCH * m.n_mels),
            widest,
            spec: vec![Complex::new(0.0, 0.0); m.nfft],
            ps: vec![0.0; m.smooth],
            pi: 0,
            frames: 0,
            m,
        }
    }

    /// Forget everything heard (the network's memory and the smoothing), as after a dialog.
    pub fn reset(&mut self) {
        for h in self.hist.iter_mut() {
            h.iter_mut().for_each(|x| *x = 0.0);
        }
        self.ps.iter_mut().for_each(|x| *x = 0.0);
        self.ring.iter_mut().for_each(|x| *x = 0.0);
        self.pos = 0;
        self.have = 0;
        self.since = 0;
    }

    /// Int16 samples in; calls `out(p)` for every new frame (in order, after the chunk's frames are computed).
    pub fn push(&mut self, pcm: &[i16], mut out: impl FnMut(f32)) {
        let win = self.m.win;
        let nm = self.m.n_mels;
        self.feats.clear();
        for &s in pcm {
            self.ring[self.pos] = s as f32;
            self.pos = (self.pos + 1) % win;
            self.have = (self.have + 1).min(win);
            self.since += 1;
            if self.have == win
                && (self.frames == 0 && self.since >= win || self.frames > 0 && self.since >= self.m.hop)
            {
                self.since = 0;
                // the window in order: the oldest sample is where the next one would go
                let (a, b) = self.ring.split_at(self.pos);
                self.buf[..b.len()].copy_from_slice(b);
                self.buf[b.len()..].copy_from_slice(a);
                self.features();
                let f = self.a[..nm].to_vec();
                self.feats.extend_from_slice(&f);
                if self.feats.len() == BATCH * nm {
                    self.flush(&mut out);
                }
            }
        }
        if !self.feats.is_empty() {
            self.flush(&mut out);
        }
    }

    /// The batch's frames through the network, their posteriors smoothed and handed out in order.
    fn flush(&mut self, out: &mut impl FnMut(f32)) {
        let nm = self.m.n_mels;
        let n = self.feats.len() / nm;
        let w = self.widest;
        for f in 0..n {
            let (dst, src) = (&mut self.a[f * w..f * w + nm], &self.feats[f * nm..(f + 1) * nm]);
            dst.copy_from_slice(src);
        }
        self.feats.clear();
        self.run(n);
        for f in 0..n {
            let (l0, l1) = (self.a[f * w], self.a[f * w + 1]);
            let p = 1.0 / (1.0 + (l0 - l1).exp()); // softmax(·)[1]
            self.ps[self.pi] = p;
            self.pi = (self.pi + 1) % self.ps.len();
            self.frames += 1;
            out(self.ps.iter().sum::<f32>() / self.ps.len() as f32);
        }
    }

    /// The fbank of the current window into self.a[..n_mels].
    fn features(&mut self) {
        let m = self.m.clone();
        let win = m.win;
        let mean = self.buf.iter().sum::<f32>() / win as f32;
        for i in 0..m.nfft {
            self.spec[i] = Complex::new(0.0, 0.0);
        }
        // DC out, pre-emphasis (the first sample against itself), window
        let mut prev = self.buf[0] - mean;
        for i in 0..win {
            let x = self.buf[i] - mean;
            let y = x - m.preemph * prev;
            prev = x;
            self.spec[i] = Complex::new(y * m.window[i], 0.0);
        }
        self.fft.process(&mut self.spec);
        let nb = m.nfft / 2 + 1;
        for j in 0..m.n_mels {
            let row = &m.banks[j * nb..(j + 1) * nb];
            let mut e = 0f32;
            for k in 0..nb {
                e += row[k] * self.spec[k].norm_sqr();
            }
            self.a[j] = e.max(f32::EPSILON).ln();
        }
    }

    /// One frame: self.a[..n_mels] (as features() leaves it) through the layers; returns the 2 logits.
    #[cfg(test)]
    fn network(&mut self) -> [f32; 2] {
        self.run(1);
        [self.a[0], self.a[1]]
    }

    /// `n` frames, frame f at self.a[f*widest..], through the layers (frames in time order: the filters keep state).
    fn run(&mut self, n: usize) {
        let m = self.m.clone();
        let wd = self.widest;
        let mut dim = m.n_mels;
        for (li, l) in m.layers.iter().enumerate() {
            match l {
                Layer::Affine { out, inp, w, b } => {
                    for o in 0..*out {
                        let row = &w[o * inp..(o + 1) * inp];
                        for f in 0..n {
                            self.b[f * wd + o] = dot(row, &self.a[f * wd..f * wd + inp]) + b[o];
                        }
                    }
                    std::mem::swap(&mut self.a, &mut self.b);
                    dim = *out;
                }
                Layer::Conv { dim: d, taps, w, b } => {
                    for f in 0..n {
                        // the ring holds the last `taps` inputs; slot head is the oldest, overwritten by this one
                        let h = &mut self.hist[li];
                        let head = self.head[li];
                        h[head * d..(head + 1) * d].copy_from_slice(&self.a[f * wd..f * wd + d]);
                        self.head[li] = (head + 1) % taps;
                        // tap j (0 = oldest) is ring slot (head+1+j) % taps
                        let y = &mut self.b[f * wd..f * wd + d];
                        y.copy_from_slice(&b[..*d]);
                        for j in 0..*taps {
                            let slot = (head + 1 + j) % taps;
                            let x = &h[slot * d..(slot + 1) * d];
                            let wj = &w[j * d..(j + 1) * d];
                            for ((yc, wc), xc) in y.iter_mut().zip(wj).zip(x) {
                                *yc += wc * xc;
                            }
                        }
                    }
                    std::mem::swap(&mut self.a, &mut self.b);
                }
                Layer::Scale(v) => {
                    (0..n).for_each(|f| self.a[f * wd..f * wd + dim].iter_mut().zip(v).for_each(|(x, s)| *x *= s))
                }
                Layer::Bias(v) => {
                    (0..n).for_each(|f| self.a[f * wd..f * wd + dim].iter_mut().zip(v).for_each(|(x, s)| *x += s))
                }
                Layer::Relu => {
                    (0..n).for_each(|f| self.a[f * wd..f * wd + dim].iter_mut().for_each(|x| *x = x.max(0.0)))
                }
                Layer::Softmax => {}
            }
        }
    }
}

/// Σ a·b. On the station (aarch64): NEON, 4 × 4 lanes of fused multiply-add — several times what the compiler makes
/// of plain Rust on its Cortex-A53. Elsewhere: 8 independent partial sums (no dependency chain).
#[cfg(target_arch = "aarch64")]
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::aarch64::*;
    let n = a.len().min(b.len());
    let mut i = 0;
    // SAFETY: NEON is part of every aarch64; loads stay within i + 16 <= n
    unsafe {
        let (mut s0, mut s1, mut s2, mut s3) = (vdupq_n_f32(0.0), vdupq_n_f32(0.0), vdupq_n_f32(0.0), vdupq_n_f32(0.0));
        let (pa, pb) = (a.as_ptr(), b.as_ptr());
        while i + 16 <= n {
            s0 = vfmaq_f32(s0, vld1q_f32(pa.add(i)), vld1q_f32(pb.add(i)));
            s1 = vfmaq_f32(s1, vld1q_f32(pa.add(i + 4)), vld1q_f32(pb.add(i + 4)));
            s2 = vfmaq_f32(s2, vld1q_f32(pa.add(i + 8)), vld1q_f32(pb.add(i + 8)));
            s3 = vfmaq_f32(s3, vld1q_f32(pa.add(i + 12)), vld1q_f32(pb.add(i + 12)));
            i += 16;
        }
        while i + 4 <= n {
            s0 = vfmaq_f32(s0, vld1q_f32(pa.add(i)), vld1q_f32(pb.add(i)));
            i += 4;
        }
        let mut s = vaddvq_f32(vaddq_f32(vaddq_f32(s0, s1), vaddq_f32(s2, s3)));
        while i < n {
            s += a[i] * b[i];
            i += 1;
        }
        s
    }
}

#[cfg(not(target_arch = "aarch64"))]
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    let ((ca, ra), (cb, rb)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    for (x, y) in ca.iter().zip(cb) {
        for k in 0..8 {
            acc[k] += x[k] * y[k];
        }
    }
    let mut s = (acc[0] + acc[4]) + (acc[1] + acc[5]) + (acc[2] + acc[6]) + (acc[3] + acc[7]);
    for (x, y) in ra.iter().zip(rb) {
        s += x * y;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Arc<Model> {
        Arc::new(Model::parse(include_bytes!("../models/mrazota.rkw")).expect("a valid model"))
    }

    #[test]
    fn matches_the_reference_it_was_trained_with() {
        let m = model();
        let v = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/kws.vec")).unwrap();
        assert_eq!(&v[..4], b"RKWV");
        let n = u32::from_le_bytes(v[4..8].try_into().unwrap()) as usize;
        let t = u32::from_le_bytes(v[8..12].try_into().unwrap()) as usize;
        let pcm: Vec<i16> = v[12..12 + 2 * n].as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c)).collect();
        let f = |off: usize, k: usize| -> Vec<f32> {
            v[off..off + 4 * k].as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
        };
        let o = 12 + 2 * n;
        let feats = f(o, t * 40);
        let logits = f(o + 4 * t * 40, t * 2);
        let ps = f(o + 4 * t * 42, t);
        let mut s = Spotter::new(m.clone());
        // feed in uneven chunks, as the stream comes
        let mut got = vec![];
        let mut i = 0;
        let mut k = 0;
        while i < pcm.len() {
            let len = [512, 160, 37, 1000, 3][k % 5].min(pcm.len() - i);
            s.push(&pcm[i..i + len], |p| got.push(p));
            i += len;
            k += 1;
        }
        assert_eq!(got.len(), t, "one value per frame");
        // features and logits frame by frame, recomputed on a fresh spotter for exactness
        let mut s2 = Spotter::new(m);
        let (mut fe, mut le, mut pe) = (0f32, 0f32, 0f32);
        for j in 0..t {
            s2.buf.copy_from_slice(&pcm[j * 160..j * 160 + 400].iter().map(|&x| x as f32).collect::<Vec<_>>()); // features() reads buf
            s2.features();
            for c in 0..40 {
                fe = fe.max((s2.a[c] - feats[j * 40 + c]).abs());
            }
            let lg = s2.network();
            le = le.max((lg[0] - logits[2 * j]).abs()).max((lg[1] - logits[2 * j + 1]).abs());
            pe = pe.max((got[j] - ps[j]).abs());
        }
        eprintln!("kws parity: features ±{fe:.2e}, logits ±{le:.2e}, smoothed p ±{pe:.2e} over {t} frames");
        assert!(fe < 2e-3, "features differ by {fe}");
        assert!(le < 2e-2, "logits differ by {le}");
        assert!(pe < 1e-3, "posteriors differ by {pe}");
    }
}
