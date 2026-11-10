//! Clients of speech recognition (OpenAI transcription API) and synthesis (OpenAI speech API): our services
//! (services/) or any other that speaks the same API — OpenAI's own, a home server. Which ones are in use is the
//! user's choice in the web UI (providers.rs); server.toml's `[services] asr / tts` are the first ones.
use serde_json::{Map, Value};
use std::time::Duration;

/// One service: the API's base (…/v1), the model, the key (none for ours), a voice (synthesis), the provider's own
/// request fields.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Endpoint {
    pub url: String,
    pub model: String,
    pub key: String,
    pub voice: String,
    pub extra: Map<String, Value>,
}

impl Endpoint {
    /// server.toml's service address (no /v1) as an endpoint of ours.
    pub fn ours(service: &str, model: &str, extra: Map<String, Value>) -> Endpoint {
        Endpoint {
            url: format!("{}/v1", service.trim_end_matches('/')),
            model: model.into(),
            extra,
            ..Default::default()
        }
    }
}

#[derive(Clone)]
pub struct Services {
    http: reqwest::Client,
    pub asr: Endpoint,
    pub tts: Endpoint,
    speed: f32,
}

#[derive(Clone)]
pub struct Speech {
    pub pcm: Vec<u8>, // s16le mono
    pub rate: u32,
}

fn client() -> reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| crate::http().timeout(Duration::from_secs(120)).build().expect("an HTTP client")).clone()
}

impl Services {
    /// server.toml's services, as ours are asked.
    pub fn new(asr: &str, tts: &str, speed: f32) -> Services {
        Services::with(Endpoint::ours(asr, "gigaam", Map::new()), Endpoint::ours(tts, "", Map::new()), speed)
    }

    pub fn with(asr: Endpoint, tts: Endpoint, speed: f32) -> Services {
        Services { http: client(), asr, tts, speed }
    }

    /// The same synthesizer, asked the same way (the ready phrases are in one voice).
    pub fn same_voice(&self, other: &Services) -> bool {
        self.tts == other.tts && self.speed == other.speed
    }

    /// 16 kHz float samples → the text.
    pub async fn transcribe(&self, x: &[f32]) -> Result<(String, f64), String> {
        let pcm: Vec<u8> = x.iter().flat_map(|v| ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
        self.transcribe_wav(wav16k(&pcm)).await
    }

    /// A WAV file → the text (and the seconds it took, as our service reports).
    pub async fn transcribe_wav(&self, wav: Vec<u8>) -> Result<(String, f64), String> {
        let e = &self.asr;
        let form = reqwest::multipart::Form::new()
            .part("file", reqwest::multipart::Part::bytes(wav).file_name("command.wav").mime_str("audio/wav").unwrap())
            .text("model", if e.model.is_empty() { "gigaam".to_string() } else { e.model.clone() })
            .text("language", "ru");
        let mut req = self.http.post(format!("{}/audio/transcriptions", e.url)).multipart(form);
        if !e.key.is_empty() {
            req = req.bearer_auth(&e.key);
        }
        let r = req.send().await.map_err(|e| format!("asr: {e}"))?;
        if !r.status().is_success() {
            let code = r.status();
            let t = r.text().await.unwrap_or_default();
            return Err(format!(
                "asr: HTTP {code}{}",
                if t.is_empty() { String::new() } else { format!(": {}", t.chars().take(160).collect::<String>()) }
            ));
        }
        let v: serde_json::Value = r.json().await.map_err(|e| format!("asr: {e}"))?;
        Ok((v["text"].as_str().unwrap_or("").to_string(), v["seconds"].as_f64().unwrap_or(0.0)))
    }

    /// The synthesizer's own health (engine, settings): what changes the sound, for the fillers' cache key.
    pub async fn tts_health(&self) -> Result<String, String> {
        let service = self.tts.url.trim_end_matches("/v1");
        let r = self
            .http
            .get(format!("{service}/health"))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("tts: {e}"))?;
        r.text().await.map_err(|e| format!("tts: {e}"))
    }

    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// A sentence → the voice in use, at the station's level.
    pub async fn speak(&self, text: &str) -> Result<Speech, TtsError> {
        let err = |code, msg: String| TtsError { code, msg };
        let e = &self.tts;
        let mut body = serde_json::json!({"input": text, "speed": self.speed, "response_format": "pcm"});
        if !e.model.is_empty() {
            body["model"] = Value::String(e.model.clone());
        }
        if !e.voice.is_empty() {
            body["voice"] = Value::String(e.voice.clone());
        }
        for (k, v) in &e.extra {
            body[k] = v.clone();
        }
        let mut req = self.http.post(format!("{}/audio/speech", e.url)).json(&body);
        if !e.key.is_empty() {
            req = req.bearer_auth(&e.key);
        }
        let r = req.send().await.map_err(|e| err(None, e.to_string()))?;
        if !r.status().is_success() {
            let code = r.status().as_u16();
            let t = r.text().await.unwrap_or_default();
            return Err(err(
                Some(code),
                format!(
                    "HTTP {code}{}",
                    if t.is_empty() { String::new() } else { format!(": {}", t.chars().take(160).collect::<String>()) }
                ),
            ));
        }
        // ours says its rate; OpenAI's «pcm» is 24 kHz s16le mono
        let rate = r
            .headers()
            .get("x-sample-rate")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(24000);
        let pcm = r.bytes().await.map_err(|e| err(None, e.to_string()))?;
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
