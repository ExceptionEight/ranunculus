//! Silero VAD v6 (16 kHz), in plain Rust from its safetensors weights (silero_vad 6.2.3, MIT): no ONNX runtime, so
//! it runs on the server and could run on the station too. The graph, as the package's JIT model has it:
//! - 512 new samples behind the last 64 (the context), reflect-padded by 64 on the right → 640;
//! - STFT as a conv: basis 258 × 256, stride 128 → 4 frames; magnitude of 129 bins;
//! - 4 convs (kernel 3, padding 1; strides 1, 2, 2, 1; 129→128→64→64→128), each + bias, ReLU → 128 × 1;
//! - an LSTM cell 128 → 128 (state h, c kept between calls);
//! - ReLU, conv 1×1 to 1, sigmoid: p(speech) for these 32 ms.
//!
//! `VadIterator` is the package's VADIterator: start at p ≥ threshold, end after `min_silence` below
//! threshold − 0.15.
use std::collections::HashMap;

pub const CHUNK: usize = 512;
const CONTEXT: usize = 64;
const N_FFT: usize = 256;
const HOP: usize = 128;
const BINS: usize = N_FFT / 2 + 1;

struct Conv {
    out: usize,
    inp: usize,
    stride: usize,
    w: Vec<f32>, // [out][inp][3]
    b: Vec<f32>,
}

impl Conv {
    /// x: [inp][len] → [out][len_out], kernel 3, padding 1
    fn run(&self, x: &[f32], len: usize) -> (Vec<f32>, usize) {
        let lo = (len + 2 - 3) / self.stride + 1;
        let mut y = vec![0f32; self.out * lo];
        for o in 0..self.out {
            for t in 0..lo {
                let mut s = self.b[o];
                for i in 0..self.inp {
                    let w = &self.w[(o * self.inp + i) * 3..(o * self.inp + i) * 3 + 3];
                    let xi = &x[i * len..(i + 1) * len];
                    for k in 0..3 {
                        let p = (t * self.stride + k) as isize - 1;
                        if p >= 0 && (p as usize) < len {
                            s += w[k] * xi[p as usize];
                        }
                    }
                }
                y[o * lo + t] = s.max(0.0); // ReLU
            }
        }
        (y, lo)
    }
}

pub struct Silero {
    basis: Vec<f32>, // [258][256]
    convs: [Conv; 4],
    w_ih: Vec<f32>, // [512][128]
    w_hh: Vec<f32>,
    b: Vec<f32>,     // b_ih + b_hh, [512]
    w_out: Vec<f32>, // [128]
    b_out: f32,
    context: Vec<f32>,
    h: Vec<f32>,
    c: Vec<f32>,
}

/// The tensors of a safetensors file: name → (shape, f32 data).
pub fn safetensors(b: &[u8]) -> Result<HashMap<String, (Vec<usize>, Vec<f32>)>, String> {
    let n = u64::from_le_bytes(b.get(..8).ok_or("too short")?.try_into().unwrap()) as usize;
    let head: serde_json::Value =
        serde_json::from_slice(b.get(8..8 + n).ok_or("truncated header")?).map_err(|e| e.to_string())?;
    let data = &b[8 + n..];
    let mut out = HashMap::new();
    for (k, v) in head.as_object().ok_or("header is not an object")? {
        if k == "__metadata__" {
            continue;
        }
        if v["dtype"] != "F32" {
            return Err(format!("{k}: {} (only F32)", v["dtype"]));
        }
        let shape: Vec<usize> =
            v["shape"].as_array().ok_or("shape")?.iter().map(|x| x.as_u64().unwrap_or(0) as usize).collect();
        let off = v["data_offsets"].as_array().ok_or("offsets")?;
        let (a, z) = (off[0].as_u64().unwrap_or(0) as usize, off[1].as_u64().unwrap_or(0) as usize);
        let raw = data.get(a..z).ok_or_else(|| format!("{k}: data out of range"))?;
        out.insert(k.clone(), (shape, raw.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()));
    }
    Ok(out)
}

impl Silero {
    /// With the weights the crate ships (assets/: Silero VAD 6.2.3, MIT).
    pub fn bundled() -> Silero {
        Silero::parse(include_bytes!("../assets/silero_vad_16k.safetensors")).expect("the bundled weights")
    }

    pub fn parse(b: &[u8]) -> Result<Silero, String> {
        let mut t = safetensors(b)?;
        let mut take = |name: &str, shape: &[usize]| -> Result<Vec<f32>, String> {
            let (s, v) = t.remove(name).ok_or_else(|| format!("no tensor {name}"))?;
            if s != shape {
                return Err(format!("{name}: shape {s:?}, expected {shape:?}"));
            }
            Ok(v)
        };
        let basis = take("stft_conv.weight", &[2 * BINS, 1, N_FFT])?;
        let mut conv = |i: usize, out: usize, inp: usize, stride: usize| -> Result<Conv, String> {
            Ok(Conv {
                out,
                inp,
                stride,
                w: take(&format!("conv{i}.weight"), &[out, inp, 3])?,
                b: take(&format!("conv{i}.bias"), &[out])?,
            })
        };
        let convs = [conv(1, 128, BINS, 1)?, conv(2, 64, 128, 2)?, conv(3, 64, 64, 2)?, conv(4, 128, 64, 1)?];
        let w_ih = take("lstm_cell.weight_ih", &[512, 128])?;
        let w_hh = take("lstm_cell.weight_hh", &[512, 128])?;
        let b_ih = take("lstm_cell.bias_ih", &[512])?;
        let b_hh = take("lstm_cell.bias_hh", &[512])?;
        let w_out = take("final_conv.weight", &[1, 128, 1])?;
        let b_out = take("final_conv.bias", &[1])?[0];
        let b = b_ih.iter().zip(&b_hh).map(|(a, b)| a + b).collect();
        Ok(Silero {
            basis,
            convs,
            w_ih,
            w_hh,
            b,
            w_out,
            b_out,
            context: vec![0.0; CONTEXT],
            h: vec![0.0; 128],
            c: vec![0.0; 128],
        })
    }

    /// Forget the state (a new utterance): zero context and LSTM state, as reset_states().
    pub fn reset(&mut self) {
        self.context.iter_mut().for_each(|x| *x = 0.0);
        self.h.iter_mut().for_each(|x| *x = 0.0);
        self.c.iter_mut().for_each(|x| *x = 0.0);
    }

    /// p(speech) of the next 512 samples (float, −1..1).
    pub fn prob(&mut self, chunk: &[f32]) -> f32 {
        assert_eq!(chunk.len(), CHUNK, "Silero 16 kHz takes 512 samples");
        // context + chunk, reflect-padded on the right by 64 (the edge sample not repeated)
        let mut x = Vec::with_capacity(CONTEXT + CHUNK + 64);
        x.extend_from_slice(&self.context);
        x.extend_from_slice(chunk);
        let n = x.len();
        for j in 0..64 {
            x.push(x[n - 2 - j]);
        }
        self.context.copy_from_slice(&x[n - CONTEXT..n]);
        let frames = (x.len() - N_FFT) / HOP + 1; // 4
        let mut mag = vec![0f32; BINS * frames];
        for f in 0..frames {
            let seg = &x[f * HOP..f * HOP + N_FFT];
            for k in 0..BINS {
                let re: f32 = self.basis[k * N_FFT..(k + 1) * N_FFT].iter().zip(seg).map(|(a, b)| a * b).sum();
                let im: f32 =
                    self.basis[(BINS + k) * N_FFT..(BINS + k + 1) * N_FFT].iter().zip(seg).map(|(a, b)| a * b).sum();
                mag[k * frames + f] = (re * re + im * im).sqrt();
            }
        }
        let (mut y, mut len) = (mag, frames);
        for c in &self.convs {
            let (yy, l) = c.run(&y, len);
            y = yy;
            len = l;
        }
        // 128 × 1 → LSTM cell (gates i, f, g, o)
        let mut g = self.b.clone();
        for r in 0..512 {
            let wi = &self.w_ih[r * 128..(r + 1) * 128];
            let wh = &self.w_hh[r * 128..(r + 1) * 128];
            let mut s = 0f32;
            for j in 0..128 {
                s += wi[j] * y[j * len] + wh[j] * self.h[j];
            }
            g[r] += s;
        }
        let sig = |v: f32| 1.0 / (1.0 + (-v).exp());
        for j in 0..128 {
            let (i, f, gg, o) = (sig(g[j]), sig(g[128 + j]), g[256 + j].tanh(), sig(g[384 + j]));
            self.c[j] = f * self.c[j] + i * gg;
            self.h[j] = o * self.c[j].tanh();
        }
        let mut s = self.b_out;
        for j in 0..128 {
            s += self.w_out[j] * self.h[j].max(0.0);
        }
        sig(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    Start,
    End,
}

/// silero_vad.VADIterator: `threshold`, `min_silence_ms`, `speech_pad_ms` at 16 kHz, 512 samples a step.
pub struct VadIterator {
    pub threshold: f32,
    min_silence: usize,
    triggered: bool,
    temp_end: usize,
    current: usize,
}

impl VadIterator {
    pub fn new(threshold: f32, min_silence_ms: usize, _speech_pad_ms: usize) -> VadIterator {
        VadIterator { threshold, min_silence: 16 * min_silence_ms, triggered: false, temp_end: 0, current: 0 }
    }

    pub fn push(&mut self, p: f32) -> Option<Event> {
        self.current += CHUNK;
        if p >= self.threshold && self.temp_end != 0 {
            self.temp_end = 0;
        }
        if p >= self.threshold && !self.triggered {
            self.triggered = true;
            return Some(Event::Start);
        }
        if p < self.threshold - 0.15 && self.triggered {
            if self.temp_end == 0 {
                self.temp_end = self.current;
            }
            if self.current - self.temp_end < self.min_silence {
                return None;
            }
            self.temp_end = 0;
            self.triggered = false;
            return Some(Event::End);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_silero_package() {
        let mut m = Silero::bundled();
        let v = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/vad.vec")).unwrap();
        assert_eq!(&v[..4], b"RVAV");
        let n = u32::from_le_bytes(v[4..8].try_into().unwrap()) as usize;
        let k = u32::from_le_bytes(v[8..12].try_into().unwrap()) as usize;
        let f = |off: usize, c: usize| -> Vec<f32> {
            v[off..off + 4 * c].as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
        };
        let x = f(12, n);
        let want = f(12 + 4 * n, k);
        let mut worst = 0f32;
        for i in 0..k {
            let p = m.prob(&x[i * CHUNK..(i + 1) * CHUNK]);
            worst = worst.max((p - want[i]).abs());
        }
        eprintln!("vad parity: {k} chunks, worst p ±{worst:.1e}");
        assert!(worst < 1e-3, "differs by {worst}");
    }

    #[test]
    fn iterator_start_and_end() {
        let mut it = VadIterator::new(0.5, 700, 100);
        assert_eq!(it.push(0.1), None);
        assert_eq!(it.push(0.9), Some(Event::Start));
        assert_eq!(it.push(0.9), None);
        // below threshold − 0.15 for 700 ms = 22 chunks of 32 ms
        let mut ended = 0;
        for i in 0..30 {
            if it.push(0.1) == Some(Event::End) {
                ended = i + 1;
                break;
            }
        }
        assert_eq!(ended, 23, "the end comes after 700 ms of silence");
        assert_eq!(it.push(0.42), None, "0.42 is not speech (below threshold), not silence either");
    }
}
