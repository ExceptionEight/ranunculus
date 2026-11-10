//! The station's sounds: what timers and alarms ring with (the web UI uploads the user's own into the data folder's
//! sounds/). A sound here is 48 kHz mono, its leading silence cut, at most MAX s long and as loud as the voice (LOUD,
//! BS.1770 integrated loudness): the volume that suits the voice suits the ring too — a file of the user's may be
//! mastered anyhow loud, and a station may stand in a bedroom.
//! A file is anything ffmpeg reads (wav, mp3, ogg, m4a…), found by its bare name in the folder. A sound that cannot be
//! loaded is replaced by the built-in chime, never by silence.
use crate::session::log;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub const RATE: u32 = 48000;
/// the built-in sound: «ди-дон, ди-дон», then a pause; 1.6 s, looped
pub const CHIME: &str = "chime";
/// LUFS: about what the voice is levelled to (loudness.rs)
const LOUD: f64 = -16.0;
const PEAK: f32 = 0.89; // −1 dBFS: louder files are made quieter, never clipped
const MAX: u32 = 600;

#[derive(Clone)]
pub struct Sound {
    pub name: String,
    /// 48 kHz mono
    pub pcm: Arc<Vec<i16>>,
}

/// The built-in chime: two notes of a soft mallet (E6 — C6), twice, then a pause; tails faded before the loop's seam.
pub fn chime() -> Sound {
    static C: OnceLock<Sound> = OnceLock::new();
    C.get_or_init(|| {
        const CYCLE: f64 = 1.6;
        let r = RATE as f64;
        let n = (CYCLE * r) as usize;
        let mut y = vec![0f32; n];
        for (t0, f) in [(0.0, 1318.51), (0.15, 1046.5), (0.45, 1318.51), (0.6, 1046.5)] {
            let start = (t0 * r) as usize;
            for (i, v) in y.iter_mut().enumerate().skip(start) {
                let t = (i - start) as f64 / r;
                let w =
                    |k: f64, a: f64, tau: f64| a * (2.0 * std::f64::consts::PI * k * f * t).sin() * (-t / tau).exp();
                let attack = (t / 0.004).min(1.0); // 4 ms: no click, still a strike
                *v += (attack * (w(1.0, 1.0, 0.3) + w(2.0, 0.25, 0.12) + w(3.0, 0.06, 0.06))) as f32;
            }
        }
        let fade = (0.25 * r) as usize;
        for k in 0..fade {
            y[n - fade + k] *= 0.5 + 0.5 * (std::f32::consts::PI * k as f32 / fade as f32).cos();
        }
        normalize(&mut y);
        Sound { name: CHIME.into(), pcm: Arc::new(to_i16(&y)) }
    })
    .clone()
}

/// `name`: CHIME, or a file in `dir` (a bare name: nothing outside the folder).
pub async fn load(dir: &str, name: &str) -> Result<Sound, String> {
    if name == CHIME {
        return Ok(chime());
    }
    if name.is_empty() || name.contains('/') || name.starts_with('.') || name.contains('\0') {
        return Err(format!("«{name}» — не имя файла"));
    }
    let path = Path::new(dir).join(name);
    if !path.is_file() {
        return Err(format!("{}: нет такого файла", path.display()));
    }
    decode(&path, name).await
}

/// A file (anything ffmpeg reads) → the sound: 48 kHz mono, its leading silence cut, at most MAX s, to LOUD.
pub async fn decode(path: &Path, name: &str) -> Result<Sound, String> {
    let mut cmd = tokio::process::Command::new("ffmpeg");
    cmd.args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args([
            "-vn",
            "-af",
            "silenceremove=start_periods=1:start_threshold=-50dB",
            "-ac",
            "1",
            "-ar",
            &RATE.to_string(),
            "-t",
            &MAX.to_string(),
            "-f",
            "s16le",
            "pipe:1",
        ])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(30), cmd.output())
        .await
        .map_err(|_| format!("{name}: ffmpeg не справился за 30 с"))?
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr).trim().lines().last().unwrap_or("ffmpeg failed")
        ));
    }
    let mut x: Vec<f32> =
        out.stdout.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c) as f32 / 32768.0).collect();
    if x.len() < RATE as usize / 10 {
        return Err(format!("{name}: звука меньше 0,1 с"));
    }
    if !normalize(&mut x) {
        return Err(format!("{name}: одна тишина"));
    }
    Ok(Sound { name: name.into(), pcm: Arc::new(to_i16(&x)) })
}

/// A name the user gives a sound (an upload, a rename): a plain file name in the folder — no path, not hidden, not the
/// built-in's, printable, at most NAME_MAX characters, no spaces at its ends. → why not
pub fn check_name(name: &str) -> Result<(), String> {
    let bad = if name.is_empty() {
        "пустое имя"
    } else if name.chars().count() > NAME_MAX {
        "длиннее 100 знаков"
    } else if name.trim() != name {
        "пробелы по краям"
    } else if name.starts_with('.') {
        "начинается с точки"
    } else if name.contains(['/', '\\']) || name.chars().any(|c| c.is_control()) {
        "косая черта или служебные знаки"
    } else if name.eq_ignore_ascii_case(CHIME) {
        "так называется встроенный звук"
    } else {
        return Ok(());
    };
    Err(format!("«{name}»: {bad}"))
}

pub const NAME_MAX: usize = 100;

/// The user's sound files in `dir`: (name, bytes, modified), by name. Hidden ones (an upload in progress) are not.
pub fn files(dir: &str) -> Vec<(String, u64, std::time::SystemTime)> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let m = e.metadata().ok().filter(|m| m.is_file())?;
            (!name.starts_with('.')).then(|| (name, m.len(), m.modified().unwrap_or(std::time::UNIX_EPOCH)))
        })
        .collect();
    v.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then(a.0.cmp(&b.0)));
    v
}

/// `load`, or the chime when it fails (logged): a ring is never silent.
pub async fn load_or_chime(dir: &str, name: &str, device: &str) -> Sound {
    match load(dir, name).await {
        Ok(s) => s,
        Err(e) => {
            log(device, format!("   (sound: {e}: the built-in «{CHIME}» instead)"));
            chime()
        }
    }
}

/// To LOUD (peaks kept under PEAK). → false: nothing to hear.
fn normalize(x: &mut [f32]) -> bool {
    let Some(l) = loudness(x) else { return false };
    let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
    let g = (10f64.powf((LOUD - l) / 20.0) as f32).min(PEAK / peak.max(1e-9));
    x.iter_mut().for_each(|v| *v *= g);
    true
}

fn to_i16(x: &[f32]) -> Vec<i16> {
    x.iter().map(|v| (v * 32767.0).round().clamp(-32768.0, 32767.0) as i16).collect()
}

/// ITU-R BS.1770-4 integrated loudness of 48 kHz mono, LUFS: K-weighting, 400-ms blocks every 100 ms, the absolute
/// gate at −70 LUFS and the relative one 10 LU under. A sound shorter than a block is one block. None: silence.
pub fn loudness(x: &[f32]) -> Option<f64> {
    // the K filter at 48 kHz (BS.1770 table 1 and 2): a high shelf (+4 dB), then the RLB high-pass
    let biquad = |b: [f64; 3], a: [f64; 2], x: &[f64]| -> Vec<f64> {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        x.iter()
            .map(|&x0| {
                let y0 = b[0] * x0 + b[1] * x1 + b[2] * x2 - a[0] * y1 - a[1] * y2;
                (x2, x1, y2, y1) = (x1, x0, y1, y0);
                y0
            })
            .collect()
    };
    let x: Vec<f64> = x.iter().map(|&v| v as f64).collect();
    let k = biquad([1.53512485958697, -2.69169618940638, 1.19839281085285], [-1.69065929318241, 0.73248077421585], &x);
    let k = biquad([1.0, -2.0, 1.0], [-1.99004745483398, 0.99007225036621], &k);
    let (block, step) = (RATE as usize * 4 / 10, RATE as usize / 10);
    let ms: Vec<f64> = if k.len() <= block {
        vec![k.iter().map(|v| v * v).sum::<f64>() / k.len().max(1) as f64]
    } else {
        (0..=(k.len() - block) / step)
            .map(|j| k[j * step..j * step + block].iter().map(|v| v * v).sum::<f64>() / block as f64)
            .collect()
    };
    let lufs = |z: f64| -0.691 + 10.0 * z.log10();
    let mean = |v: &[f64]| if v.is_empty() { None } else { Some(v.iter().sum::<f64>() / v.len() as f64) };
    let gated: Vec<f64> = ms.iter().copied().filter(|&z| z > 0.0 && lufs(z) > -70.0).collect();
    let rel = lufs(mean(&gated)?) - 10.0;
    let kept: Vec<f64> = gated.into_iter().filter(|&z| lufs(z) > rel).collect();
    Some(lufs(mean(&kept)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f64, a: f32, s: f64) -> Vec<f32> {
        (0..(s * RATE as f64) as usize)
            .map(|i| a * (2.0 * std::f64::consts::PI * f * i as f64 / RATE as f64).sin() as f32)
            .collect()
    }

    #[test]
    fn loudness_as_bs1770() {
        // the standard's own check: a 0 dBFS 997 Hz sine in one channel is −3.01 LUFS
        assert!((loudness(&sine(997.0, 1.0, 3.0)).unwrap() + 3.01).abs() < 0.05);
        // −20 dB is −20 LU less; silence is nothing
        assert!((loudness(&sine(997.0, 0.1, 3.0)).unwrap() + 23.01).abs() < 0.05);
        assert!(loudness(&vec![0.0; 48000]).is_none());
        // the gate: three seconds of tone and ten of silence measure as the tone (the blocks over its edge, partly
        // silent, are kept: −0.2 dB)
        let mut x = sine(997.0, 0.1, 3.0);
        x.extend(vec![0.0; 480000]);
        assert!((loudness(&x).unwrap() + 23.01).abs() < 0.3);
    }

    #[test]
    fn the_chime() {
        let c = chime();
        assert_eq!(c.pcm.len(), (1.6 * RATE as f64) as usize);
        let x: Vec<f32> = c.pcm.iter().map(|&v| v as f32 / 32768.0).collect();
        let three: Vec<f32> = x.iter().chain(&x).chain(&x).copied().collect();
        assert!((loudness(&three).unwrap() - LOUD).abs() < 0.3, "{:?}", loudness(&three));
        assert!(x.iter().all(|v| v.abs() < PEAK + 0.001));
        assert!(x[0].abs() < 0.01 && x.last().unwrap().abs() < 0.001, "the seam of the loop is quiet");
    }

    #[test]
    fn names_of_the_users() {
        for ok in ["bell.mp3", "Колокольчик (утро).ogg", "a", "x".repeat(NAME_MAX).as_str(), "мой звук"]
        {
            assert!(check_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            " bell.mp3",
            "bell.mp3 ",
            ".hidden",
            "../x",
            "a/b",
            "a\\b",
            "a\nb",
            "a\0b",
            "chime",
            "CHIME",
            "x".repeat(NAME_MAX + 1).as_str(),
        ] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn a_file_of_the_users() {
        let d = std::env::temp_dir().join(format!("server-sounds-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        // a loud tone after half a second of silence, at 44.1 kHz stereo: made 48 kHz mono, the silence cut, to LOUD
        let mut pcm = vec![0u8; 44100 * 2 * 2 / 2];
        for i in 0..44100 {
            let v = (0.9 * (2.0 * std::f64::consts::PI * 660.0 * i as f64 / 44100.0).sin() * 32767.0) as i16;
            pcm.extend(v.to_le_bytes());
            pcm.extend(v.to_le_bytes());
        }
        let mut wav = crate::services::wav(&pcm, 44100);
        wav[22] = 2; // channels
        wav[28..32].copy_from_slice(&(44100u32 * 4).to_le_bytes());
        wav[32] = 4;
        std::fs::write(d.join("loud.wav"), &wav).unwrap();
        let dir = d.to_str().unwrap();
        for bad in ["../x.wav", ".hidden", "", "nope.wav"] {
            assert!(load(dir, bad).await.is_err(), "{bad}");
        }
        let s = load(dir, "loud.wav").await.expect("ffmpeg is needed here: the server decodes sounds with it");
        let secs = s.pcm.len() as f64 / RATE as f64;
        assert!((secs - 1.0).abs() < 0.02, "{secs} s");
        let x: Vec<f32> = s.pcm.iter().map(|&v| v as f32 / 32768.0).collect();
        assert!((loudness(&x).unwrap() - LOUD).abs() < 0.3);
        assert!(x[..480].iter().any(|v| v.abs() > 0.01), "starts at once");
        std::fs::write(d.join("junk.mp3"), b"not audio").unwrap();
        assert!(load(dir, "junk.mp3").await.is_err());
        assert_eq!(load_or_chime(dir, "junk.mp3", "test").await.name, CHIME);
        let _ = std::fs::remove_dir_all(&d);
    }
}
