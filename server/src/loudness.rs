//! Speech at one level, whatever synthesizer said it: each voice comes out at a loudness of its own, and a short «Да.»
//! well under a long phrase. The loud part of a phrase is brought to TARGET (the level the station's volume steps
//! and the built-in sounds are matched to) and its peaks are pressed under CEIL, not clipped.

/// dBFS of a phrase's loud part
const TARGET: f64 = -14.5;
/// the limiter's ceiling: -1 dBFS
const CEIL: f64 = 0.891;
/// the most a quiet phrase is raised: 20 dB (more would bring up a synthesizer's noise floor)
const MAX_GAIN: f64 = 10.0;
/// s of a frame the level is measured by
const FRAME: f64 = 0.032;
/// s the limiter takes to let go
const RELEASE: f64 = 0.05;

/// dBFS: the mean power of the frames within 30 dB of the loudest (a median would drown a short «Да.» in the pauses
/// around it). None: silence, or less than a frame.
fn speech_level(y: &[f64], rate: u32) -> Option<f64> {
    let k = (FRAME * rate as f64) as usize;
    if k == 0 || y.len() < k {
        return None;
    }
    let power: Vec<f64> = y.chunks_exact(k).map(|f| f.iter().map(|x| x * x).sum::<f64>() / k as f64 + 1e-12).collect();
    let loudest = power.iter().cloned().fold(0.0, f64::max);
    if 10.0 * loudest.log10() < -70.0 {
        return None;
    }
    let loud: Vec<f64> = power.into_iter().filter(|p| 10.0 * p.log10() > 10.0 * loudest.log10() - 30.0).collect();
    Some(10.0 * (loud.iter().sum::<f64>() / loud.len() as f64).log10())
}

/// A gain under CEIL / |y| by blocks of 1 ms, looking a few blocks ahead and letting go slowly: the peaks pressed.
fn limit(y: &[f64], rate: u32) -> Vec<f64> {
    let b = ((0.001 * rate as f64) as usize).max(1);
    let mut need: Vec<f64> =
        y.chunks(b).map(|c| c.iter().fold(1.0f64, |g, x| g.min(CEIL / x.abs().max(1e-9)))).collect();
    for ahead in [1, 2] {
        for i in 0..need.len() {
            need[i] = need[i].min(need.get(i + ahead).copied().unwrap_or(1.0));
        }
    }
    let a = (-1.0 / (RELEASE * 1000.0)).exp();
    let mut g = 1.0;
    let mut out = Vec::with_capacity(y.len());
    for (block, x) in y.chunks(b).zip(need) {
        g = if x < g { x } else { x + (g - x) * a };
        out.extend(block.iter().map(|v| v * g));
    }
    out
}

/// s16le mono → the same phrase at the station's level.
pub fn level(pcm: &[u8], rate: u32) -> Vec<u8> {
    let y: Vec<f64> = pcm.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c) as f64 / 32768.0).collect();
    let out = match speech_level(&y, rate) {
        None => limit(&y, rate),
        Some(at) => {
            let raised = |g: f64| limit(&y.iter().map(|v| v * g).collect::<Vec<_>>(), rate);
            let mut g = 10f64.powf((TARGET - at) / 20.0).min(MAX_GAIN);
            let mut out = raised(g);
            // the limiter takes some of the level back: a bit more gain, pressed again
            for _ in 0..2 {
                let short = TARGET - speech_level(&out, rate).unwrap_or(TARGET);
                if short < 0.3 || g >= MAX_GAIN {
                    break;
                }
                g = (g * 10f64.powf(short / 20.0)).min(MAX_GAIN);
                out = raised(g);
            }
            out
        }
    };
    out.iter().flat_map(|v| ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 24000;

    /// `seconds` of a 220 Hz tone at `amp`, as PCM.
    fn tone(amp: f64, seconds: f64) -> Vec<u8> {
        (0..(seconds * RATE as f64) as usize)
            .flat_map(|i| {
                ((amp * (2.0 * std::f64::consts::PI * 220.0 * i as f64 / RATE as f64).sin() * 32767.0) as i16)
                    .to_le_bytes()
            })
            .collect()
    }

    fn floats(pcm: &[u8]) -> Vec<f64> {
        pcm.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c) as f64 / 32768.0).collect()
    }

    fn measured(pcm: &[u8]) -> f64 {
        speech_level(&floats(pcm), RATE).unwrap()
    }

    #[test]
    fn quiet_and_loud_voices_come_out_alike() {
        let (quiet, loud) = (level(&tone(0.05, 1.0), RATE), level(&tone(0.9, 1.0), RATE));
        assert!(
            (measured(&quiet) - TARGET).abs() < 0.5 && (measured(&loud) - TARGET).abs() < 0.5,
            "{} {}",
            measured(&quiet),
            measured(&loud)
        );
        assert_eq!(quiet.len(), tone(0.05, 1.0).len());
        // a whisper is raised by 20 dB at most
        let whisper = tone(0.002, 1.0);
        assert!((measured(&level(&whisper, RATE)) - (measured(&whisper) + 20.0)).abs() < 0.5);
    }

    #[test]
    fn a_short_word_between_pauses_is_measured_by_its_loud_part() {
        let mut said = vec![0u8; RATE as usize * 2];
        said.extend(tone(0.05, 0.2));
        said.extend(vec![0u8; RATE as usize * 2]);
        let out = level(&said, RATE);
        let word = &out[RATE as usize * 2..RATE as usize * 2 + RATE as usize / 5 * 2];
        assert!((measured(word) - TARGET).abs() < 1.0, "{}", measured(word));
    }

    #[test]
    fn peaks_are_pressed_under_the_ceiling() {
        // a phrase with a click far above it: after the gain the click would clip
        let mut y = tone(0.1, 1.0);
        let at = RATE as usize;
        y[at..at + 2].copy_from_slice(&30000i16.to_le_bytes());
        let out = floats(&level(&y, RATE));
        let peak = out.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        assert!(peak <= CEIL + 1e-3, "{peak}");
    }

    #[test]
    fn silence_stays_silence() {
        assert_eq!(level(&vec![0u8; 48000], RATE), vec![0u8; 48000]);
        assert_eq!(level(&[], RATE), Vec::<u8>::new());
        assert_eq!(level(&tone(0.5, 0.01), RATE).len(), tone(0.5, 0.01).len());
    }
}
