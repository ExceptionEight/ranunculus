//! Clients of what a connection does besides answering: speech recognition (the OpenAI transcription API), the voice
//! (the OpenAI speech API) and the vectors of the memory (the OpenAI embeddings API). OpenRouter speaks all three under
//! one key, and so may a server at home. Which model of which connection does what is the user's choice in the web UI
//! (providers.rs).
use serde_json::{json, Map, Value};
use std::time::Duration;

/// One role's service: the API's base (…/v1), the model, the connection's key (none at home), a voice (synthesis), the
/// request's own fields.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Endpoint {
    pub url: String,
    pub model: String,
    pub key: String,
    pub voice: String,
    pub extra: Map<String, Value>,
}

impl Endpoint {
    fn post(&self, http: &reqwest::Client, path: &str, wait: Duration) -> reqwest::RequestBuilder {
        let req = http.post(format!("{}{path}", self.url)).timeout(wait);
        if self.key.is_empty() {
            req
        } else {
            req.bearer_auth(&self.key)
        }
    }
}

/// Recognition and the voice in use; None: not chosen yet.
#[derive(Clone)]
pub struct Services {
    http: reqwest::Client,
    pub asr: Option<Endpoint>,
    pub tts: Option<Endpoint>,
    /// the voice's own speed; None: as the service speaks
    speed: Option<f32>,
}

#[derive(Clone)]
pub struct Speech {
    pub pcm: Vec<u8>, // s16le mono
    pub rate: u32,
}

fn client() -> reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        crate::http()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .expect("an HTTP client")
    })
    .clone()
}

/// A request sent, and once more if it did not get through: a connection kept from the last request may have been
/// closed by the service meanwhile, and a recording is not sent a second time by the HTTP client itself. Not after
/// a timeout: that wait was long enough.
async fn sent(what: &str, make: impl Fn() -> reqwest::RequestBuilder) -> Result<reqwest::Response, String> {
    match make().send().await {
        Err(e) if !e.is_timeout() => {
            crate::session::log("server", format!("   ({what}: {}: asked once more)", crate::why(&e)));
            make().send().await.map_err(|e| crate::why(&e))
        }
        r => r.map_err(|e| crate::why(&e)),
    }
}

/// A failed answer in a line: the status and the start of what came with it.
async fn refused(r: reqwest::Response) -> (u16, String) {
    let code = r.status().as_u16();
    let t = r.text().await.unwrap_or_default();
    // {"error": {"message": "…"}} is the usual shape: the message is what a person can read
    let said = serde_json::from_str::<Value>(&t)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().or(v["error"].as_str()).map(String::from))
        .unwrap_or(t);
    let said: String = said.chars().take(200).collect();
    (code, if said.is_empty() { format!("HTTP {code}") } else { format!("HTTP {code}: {said}") })
}

/// A station's microphone is quiet (speech around −60 dBFS): brought to the level recognizers are trained on,
/// −23 dBFS RMS with the peaks under 0.9.
fn level(x: &[f32]) -> Vec<f32> {
    let rms = (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt() + 1e-9;
    let peak = x.iter().fold(0f32, |m, v| m.max(v.abs())) as f64 + 1e-9;
    let gain = (10f64.powf(-23.0 / 20.0) / rms).min(0.9 / peak) as f32;
    x.iter().map(|v| v * gain).collect()
}

/// What was said, of a recognizer's text: some write what they hear besides speech («[шум]», «<|speaker:0|>»).
fn said(text: &str) -> String {
    static TAG: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let tag = TAG.get_or_init(|| regex::Regex::new(r"\[[^\]]*\]|<\|[^|]*\|>").unwrap());
    tag.replace_all(text, " ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The rate and the channels a speech answer names: «audio/pcm;rate=24000;channels=1» (OpenRouter), X-Sample-Rate (a
/// server of ours); neither — OpenAI's «pcm», 24 kHz mono.
fn pcm_format(h: &reqwest::header::HeaderMap) -> (u32, usize) {
    let ctype = h.get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    let named = |k: &str| {
        ctype.split(';').skip(1).find_map(|p| p.trim().strip_prefix(k)?.strip_prefix('=')?.trim().parse::<u32>().ok())
    };
    let header = h.get("x-sample-rate").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    (
        named("rate").or(header).filter(|r| (8000..=192_000).contains(r)).unwrap_or(24000),
        named("channels").unwrap_or(1).clamp(1, 8) as usize,
    )
}

/// Is the answer a sound file (mp3, wav, ogg…), not bare samples? «audio/L16» is bare samples: little-endian, as
/// Google sends its voices under that name (the RFC's L16 is big-endian; nobody we know sends that).
fn is_file(h: &reqwest::header::HeaderMap, body: &[u8]) -> bool {
    let ctype = h.get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    body.starts_with(b"RIFF")
        || body.starts_with(b"ID3")
        || body.starts_with(b"OggS")
        || (ctype.starts_with("audio/") && !ctype.starts_with("audio/pcm") && !ctype.starts_with("audio/l16"))
}

/// Interleaved s16le of `channels` → mono.
fn mono(pcm: &[u8], channels: usize) -> Vec<u8> {
    if channels <= 1 {
        return pcm.to_vec();
    }
    pcm.chunks_exact(2 * channels)
        .flat_map(|f| {
            let sum: i32 = f.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c) as i32).sum();
            ((sum / channels as i32) as i16).to_le_bytes()
        })
        .collect()
}

/// A sound file → 24 kHz mono s16le (ffmpeg): for a voice that answers with mp3 or wav whatever it is asked.
async fn decode(file: &[u8]) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncWriteExt;
    let mut child = tokio::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i", "pipe:0", "-f", "s16le", "-ac", "1", "-ar", "24000", "pipe:1"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("ffmpeg: no input")?;
    let file = file.to_vec();
    // written beside the reading: ffmpeg gives sound back before it has taken all of a long file
    let feed = tokio::spawn(async move {
        let _ = stdin.write_all(&file).await;
    });
    // a sound it cannot make sense of is given up on, not waited for (the child is killed with the wait)
    let out = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .map_err(|_| "ffmpeg: no sound in 30 s".to_string())?
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let _ = feed.await;
    if !out.status.success() || out.stdout.is_empty() {
        return Err("the voice answered with a sound ffmpeg does not read".into());
    }
    Ok(out.stdout)
}

/// Texts → their vectors, each of length one (the memory compares them by the dot product).
pub async fn embed(e: &Endpoint, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    let mut body = json!({"input": texts});
    if !e.model.is_empty() {
        body["model"] = json!(e.model);
    }
    for (k, v) in &e.extra {
        body[k] = v.clone();
    }
    let http = client();
    let r = sent("embeddings", || e.post(&http, "/embeddings", Duration::from_secs(30)).json(&body))
        .await
        .map_err(|e| format!("embeddings: {e}"))?;
    if !r.status().is_success() {
        return Err(format!("embeddings: {}", refused(r).await.1));
    }
    let v: Value = r.json().await.map_err(|e| format!("embeddings: {e}"))?;
    let mut rows: Vec<(u64, Vec<f32>)> = v["data"]
        .as_array()
        .ok_or("embeddings: no vectors in the answer")?
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let vec: Vec<f32> =
                d["embedding"].as_array().into_iter().flatten().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect();
            (d["index"].as_u64().unwrap_or(i as u64), vec)
        })
        .collect();
    rows.sort_by_key(|(i, _)| *i);
    if rows.len() != texts.len() || rows.iter().any(|(_, v)| v.is_empty() || v.len() != rows[0].1.len()) {
        return Err("embeddings: not a vector for every text".into());
    }
    Ok(rows
        .into_iter()
        .map(|(_, v)| {
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            v.into_iter().map(|x| x / n).collect()
        })
        .collect())
}

impl Services {
    pub fn new(asr: Option<Endpoint>, tts: Option<Endpoint>, speed: Option<f32>) -> Services {
        Services { http: client(), asr, tts, speed }
    }

    /// The same synthesizer, asked the same way (the ready phrases are in one voice).
    pub fn same_voice(&self, other: &Services) -> bool {
        self.tts == other.tts && self.speed == other.speed
    }

    /// What tells one voice from another, as a number: the ready phrases are kept by it. Not the key: the voice is
    /// the same under a new one.
    pub fn voice_id(&self) -> Option<u64> {
        use std::hash::{Hash, Hasher};
        let e = self.tts.as_ref()?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (&e.url, &e.model, &e.voice, Value::Object(e.extra.clone()).to_string(), self.speed.map(f32::to_bits))
            .hash(&mut h);
        Some(h.finish())
    }

    /// 16 kHz float samples, as a station's microphone gives them → the text.
    pub async fn transcribe(&self, x: &[f32]) -> Result<String, String> {
        let pcm: Vec<u8> =
            level(x).iter().flat_map(|v| ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
        self.transcribe_wav(wav16k(&pcm)).await
    }

    /// A WAV file → the text.
    pub async fn transcribe_wav(&self, wav: Vec<u8>) -> Result<String, String> {
        let e = self.asr.as_ref().ok_or("asr: no recognition is chosen in the web UI")?;
        let form = || {
            let file = reqwest::multipart::Part::bytes(wav.clone()).file_name("command.wav").mime_str("audio/wav");
            let mut form = reqwest::multipart::Form::new().part("file", file.unwrap()).text("language", "ru");
            if !e.model.is_empty() {
                form = form.text("model", e.model.clone());
            }
            for (k, v) in &e.extra {
                form = form.text(k.clone(), v.as_str().map(String::from).unwrap_or_else(|| v.to_string()));
            }
            form
        };
        let r = sent("asr", || e.post(&self.http, "/audio/transcriptions", Duration::from_secs(30)).multipart(form()))
            .await
            .map_err(|e| format!("asr: {e}"))?;
        if !r.status().is_success() {
            return Err(format!("asr: {}", refused(r).await.1));
        }
        let v: Value = r.json().await.map_err(|e| format!("asr: {e}"))?;
        Ok(said(v["text"].as_str().unwrap_or("")))
    }

    /// A sentence → the voice in use, at the station's level.
    pub async fn speak(&self, text: &str) -> Result<Speech, TtsError> {
        let err = |code, msg: String| TtsError { code, msg };
        let e = self.tts.as_ref().ok_or_else(|| err(None, "no voice is chosen in the web UI".into()))?;
        let mut body = json!({"input": text, "response_format": "pcm"});
        if !e.model.is_empty() {
            body["model"] = json!(e.model);
        }
        if !e.voice.is_empty() {
            body["voice"] = json!(e.voice);
        }
        // only a speed of its own: some voices refuse the field altogether
        if let Some(s) = self.speed.filter(|s| (s - 1.0).abs() > 1e-3) {
            body["speed"] = json!(s);
        }
        for (k, v) in &e.extra {
            body[k] = v.clone();
        }
        let r = sent("tts", || e.post(&self.http, "/audio/speech", Duration::from_secs(60)).json(&body))
            .await
            .map_err(|e| err(None, e))?;
        if !r.status().is_success() {
            let (code, msg) = refused(r).await;
            return Err(err(Some(code), msg));
        }
        let head = r.headers().clone();
        let body = r.bytes().await.map_err(|e| err(None, e.to_string()))?;
        let (pcm, rate) = if is_file(&head, &body) {
            (decode(&body).await.map_err(|e| err(None, e))?, 24000)
        } else {
            let (rate, channels) = pcm_format(&head);
            (mono(&body, channels), rate)
        };
        if pcm.len() < 2 {
            return Err(err(None, "the voice answered with no sound".into()));
        }
        Ok(Speech { pcm: crate::loudness::level(&pcm, rate), rate })
    }
}

/// The synthesizer's failure: `code` — its HTTP status (507: the text is too long for its memory cap).
#[derive(Debug)]
pub struct TtsError {
    pub code: Option<u16>,
    pub msg: String,
}

impl std::fmt::Display for TtsError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "tts: {}", self.msg)
    }
}

/// A 16 kHz mono 16-bit wav around PCM bytes.
pub fn wav16k(pcm: &[u8]) -> Vec<u8> {
    wav(pcm, 16000)
}

/// A mono 16-bit wav around PCM bytes.
pub fn wav(pcm: &[u8], rate: u32) -> Vec<u8> {
    let mut w = Vec::with_capacity(44 + pcm.len());
    w.extend(b"RIFF");
    w.extend(((36 + pcm.len()) as u32).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes());
    w.extend(1u16.to_le_bytes());
    w.extend(rate.to_le_bytes());
    w.extend((rate * 2).to_le_bytes());
    w.extend(2u16.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"data");
    w.extend((pcm.len() as u32).to_le_bytes());
    w.extend_from_slice(pcm);
    w
}

/// A mono 16-bit wav → (PCM bytes, rate); None for anything else.
pub fn read_wav(b: &[u8]) -> Option<(Vec<u8>, u32)> {
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let (mut i, mut rate, mut fmt_ok) = (12usize, 0u32, false);
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let n = u32::from_le_bytes(b[i + 4..i + 8].try_into().ok()?) as usize;
        let body = b.get(i + 8..(i + 8 + n).min(b.len()))?;
        if id == b"fmt " && body.len() >= 16 {
            let (format, channels, bits) = (
                u16::from_le_bytes([body[0], body[1]]),
                u16::from_le_bytes([body[2], body[3]]),
                u16::from_le_bytes([body[14], body[15]]),
            );
            rate = u32::from_le_bytes(body[4..8].try_into().ok()?);
            fmt_ok = format == 1 && channels == 1 && bits == 16;
        } else if id == b"data" {
            return fmt_ok.then(|| (body.to_vec(), rate));
        }
        i += 8 + n + (n & 1);
    }
    None
}

/// `beep` so many times.
pub fn beeps(rate: u32, times: usize) -> Vec<u8> {
    (0..times).flat_map(|_| beep(rate)).collect()
}

/// Two short tones (880 and 1320 Hz, 70 ms each, Hann windows), then 120 ms of silence, at 0.25.
pub fn beep(rate: u32) -> Vec<u8> {
    let n = (0.07 * rate as f64) as usize;
    let tone = |f: f64| {
        (0..n).map(move |i| {
            let t = i as f64 / rate as f64;
            let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
            (2.0 * std::f64::consts::PI * f * t).sin() * w
        })
    };
    let mut y: Vec<f64> = tone(880.0).chain(tone(1320.0)).collect();
    y.extend(std::iter::repeat_n(0.0, (0.12 * rate as f64) as usize));
    y.iter().flat_map(|v| ((v * 0.25 * 32767.0) as i16).to_le_bytes()).collect()
}

/// A service on 127.0.0.1 with the three APIs, as OpenRouter answers them; what it was asked is kept.
#[cfg(test)]
pub mod fake {
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    pub const HEARD: &str = "Привет, как дела?";

    #[derive(Clone, Debug)]
    pub struct Asked {
        pub path: String,
        pub key: String,
        /// the JSON body; a multipart one: its text as it came
        pub body: Value,
    }

    pub struct Service {
        /// the API's base: http://127.0.0.1:port/v1
        pub url: String,
        asked: Arc<Mutex<Vec<Asked>>>,
    }

    impl Service {
        pub async fn start() -> Service {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/v1", l.local_addr().unwrap());
            let asked: Arc<Mutex<Vec<Asked>>> = Default::default();
            let a2 = asked.clone();
            tokio::spawn(async move {
                while let Ok((s, _)) = l.accept().await {
                    tokio::spawn(serve(s, a2.clone()));
                }
            });
            Service { url, asked }
        }

        pub fn asked(&self) -> Vec<Asked> {
            self.asked.lock().unwrap().clone()
        }
    }

    /// Half a second of a 220 Hz tone in stereo at 48 kHz: what `mono` and the named rate are for.
    fn tone() -> Vec<u8> {
        (0..24000)
            .flat_map(|i| {
                let v = ((i as f64 * 220.0 * std::f64::consts::TAU / 48000.0).sin() * 8000.0) as i16;
                [v.to_le_bytes(), v.to_le_bytes()].concat()
            })
            .collect()
    }

    async fn serve(mut s: tokio::net::TcpStream, asked: Arc<Mutex<Vec<Asked>>>) {
        let mut buf = vec![];
        let mut chunk = [0u8; 65536];
        let head_end = loop {
            let n = s.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let field = |name: &str| {
            head.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
            })
        };
        let len: usize = field("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        while buf.len() < head_end + len {
            let n = s.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
        let raw = &buf[head_end..head_end + len];
        let body = serde_json::from_slice(raw).unwrap_or_else(|_| json!(String::from_utf8_lossy(raw)));
        let key = field("authorization").unwrap_or_default().trim_start_matches("Bearer ").to_string();
        // a model named «flaky»: the first request for it is dropped unanswered, as over a connection the service
        // closed while it was kept
        let flaky = String::from_utf8_lossy(raw).contains("flaky");
        let first = flaky && !asked.lock().unwrap().iter().any(|a| a.body.to_string().contains("flaky"));
        asked.lock().unwrap().push(Asked { path: path.clone(), key, body: body.clone() });
        if first {
            return;
        }
        let (status, ctype, out): (&str, &str, Vec<u8>) = if path.ends_with("/audio/speech") {
            if body["voice"] == json!("nobody") {
                ("400 Bad Request", "application/json", br#"{"error":{"message":"no such voice","code":400}}"#.to_vec())
            } else {
                ("200 OK", "audio/pcm;rate=48000;channels=2", tone())
            }
        } else if path.ends_with("/audio/transcriptions") {
            (
                "200 OK",
                "application/json",
                json!({"text": format!("<|speaker:0|> [шум] {HEARD}")}).to_string().into_bytes(),
            )
        } else if path.ends_with("/embeddings") {
            // out of order and not of length one: both are the client's to put right
            let n = body["input"].as_array().map(|a| a.len()).unwrap_or(0);
            let data: Vec<Value> = (0..n)
                .rev()
                .map(|i| json!({"index": i, "embedding": [3.0 * (i + 1) as f64, 4.0 * (i + 1) as f64]}))
                .collect();
            ("200 OK", "application/json", json!({"data": data}).to_string().into_bytes())
        } else {
            ("404 Not Found", "application/json", b"{}".to_vec())
        };
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            out.len()
        );
        let _ = s.write_all(head.as_bytes()).await;
        let _ = s.write_all(&out).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(url: &str, model: &str, voice: &str) -> Endpoint {
        Endpoint { url: url.into(), model: model.into(), key: "sk-test".into(), voice: voice.into(), extra: Map::new() }
    }

    #[test]
    fn a_quiet_microphone_is_brought_up() {
        let quiet: Vec<f32> = (0..16000).map(|i| ((i as f32) * 0.2).sin() * 0.001).collect();
        let y = level(&quiet);
        let rms = (y.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / y.len() as f64).sqrt();
        assert!((20.0 * rms.log10() + 23.0).abs() < 0.5, "{}", 20.0 * rms.log10());
        // a click over quiet speech: the peak holds the gain back
        let mut click = quiet.clone();
        click[100] = 0.5;
        assert!(level(&click).iter().all(|v| v.abs() <= 0.9001));
        assert!(level(&[0.0; 100]).iter().all(|v| *v == 0.0) && level(&[]).is_empty());
    }

    #[test]
    fn only_what_was_said() {
        assert_eq!(said("<|speaker:0|> [шум дождя] Включи радио."), "Включи радио.");
        assert_eq!(said(" [шум] "), "");
        assert_eq!(said("Сколько будет 2 + 2?"), "Сколько будет 2 + 2?");
    }

    #[test]
    fn the_sound_an_answer_names() {
        let h = |pairs: &[(&'static str, &'static str)]| {
            let mut m = reqwest::header::HeaderMap::new();
            for (k, v) in pairs {
                m.insert(*k, v.parse().unwrap());
            }
            m
        };
        assert_eq!(pcm_format(&h(&[("content-type", "audio/pcm;rate=44100;channels=1")])), (44100, 1));
        assert_eq!(pcm_format(&h(&[("content-type", "audio/L16; rate=24000; channels=2")])), (24000, 2));
        assert_eq!(pcm_format(&h(&[("content-type", "audio/pcm"), ("x-sample-rate", "48000")])), (48000, 1));
        assert_eq!(pcm_format(&h(&[])), (24000, 1));
        assert!(!is_file(&h(&[("content-type", "audio/pcm;rate=24000")]), &[0; 8]));
        assert!(is_file(&h(&[("content-type", "audio/mpeg")]), &[0; 8]) && is_file(&h(&[]), b"RIFF\0\0\0\0WAVE"));
        assert_eq!(mono(&[0, 0, 100, 0, 10, 0, 30, 0], 2), [50, 0, 20, 0]);
    }

    #[tokio::test]
    async fn the_three_apis_as_openrouter_answers_them() {
        let f = fake::Service::start().await;
        let s = Services::new(Some(at(&f.url, "a/hear", "")), Some(at(&f.url, "a/say", "sarah")), None);
        // heard: the file, the model and the language go; the recognizer's own marks do not come back
        assert_eq!(s.transcribe(&[0.01; 1600]).await.unwrap(), fake::HEARD);
        // said: 48 kHz stereo, as its answer names it, comes out mono at that rate; no speed unless the voice has one
        let sp = s.speak("Раз.").await.unwrap();
        assert_eq!((sp.rate, sp.pcm.len()), (48000, 48000));
        let fast = Services::new(None, s.tts.clone(), Some(1.3));
        fast.speak("Два.").await.unwrap();
        let e = Services::new(None, Some(at(&f.url, "a/say", "nobody")), None).speak("Три.").await.err().unwrap();
        assert_eq!((e.code, e.msg.as_str()), (Some(400), "HTTP 400: no such voice"));
        assert!(Services::new(None, None, None).speak("Четыре.").await.is_err());
        assert!(!s.same_voice(&fast) && s.voice_id() != fast.voice_id() && s.voice_id().is_some());
        // vectors: in the texts' order, each of length one; the role's own fields go with the request
        let mut memory = at(&f.url, "a/embed", "");
        memory.extra.insert("dimensions".into(), json!(2));
        let v = embed(&memory, &["один".into(), "два".into()]).await.unwrap();
        assert_eq!(v.len(), 2);
        assert!(v.iter().all(|x| (x[0] - 0.6).abs() < 1e-6 && (x[1] - 0.8).abs() < 1e-6), "{v:?}");
        let asked = f.asked();
        assert!(asked.iter().all(|a| a.key == "sk-test"));
        let of =
            |path: &str| asked.iter().filter(|a| a.path.ends_with(path)).map(|a| a.body.clone()).collect::<Vec<_>>();
        let heard = of("/audio/transcriptions")[0].as_str().unwrap().to_string();
        assert!(heard.contains("a/hear") && heard.contains("name=\"language\"") && heard.contains("RIFF"));
        let spoken = of("/audio/speech");
        assert_eq!(spoken[0], json!({"input": "Раз.", "response_format": "pcm", "model": "a/say", "voice": "sarah"}));
        assert!((spoken[1]["speed"].as_f64().unwrap() - 1.3).abs() < 1e-6);
        assert_eq!(of("/embeddings")[0], json!({"input": ["один", "два"], "model": "a/embed", "dimensions": 2}));
        // a request that did not get through is sent once more: the command is heard all the same
        let flaky = Services::new(Some(at(&f.url, "a/flaky", "")), None, None);
        assert_eq!(flaky.transcribe(&[0.01; 1600]).await.unwrap(), fake::HEARD);
        assert_eq!(f.asked().iter().filter(|a| a.body.to_string().contains("a/flaky")).count(), 2);
    }
}
