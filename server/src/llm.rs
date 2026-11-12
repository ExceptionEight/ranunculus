//! An OpenAI-compatible chat completion: streamed (the text as it comes, for sentence-by-sentence speech, and the
//! tool calls at the end) or whole (the note-taker, the prompt warm-up). OpenRouter, Ollama, llama.cpp — anything
//! with /v1/chat/completions.
use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use std::time::Duration;

#[derive(Clone)]
pub struct Llm {
    http: reqwest::Client,
    pub url: String,
    pub model: String,
    key: String,
    extra: Map<String, Value>, // the provider's own request fields (OpenRouter: reasoning, provider)
    cloud: bool,               // away from the house: providers::cloud (the tests make a model on 127.0.0.1 one)
}

#[derive(Debug, Clone, Default)]
pub struct Call {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

impl Call {
    pub fn to_json(&self) -> Value {
        json!({"id": self.id, "type": "function", "function": {"name": self.name, "arguments": self.arguments}})
    }
}

#[derive(Debug)]
pub enum LlmError {
    /// The server answered with an error status (a local one: 500 on a tool call it cannot parse; a proxy: 502)
    Http(u16, String),
    /// No answer, a broken stream, a timeout
    Other(String),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            LlmError::Http(code, t) => write!(f, "llm: HTTP {code}: {t}"),
            LlmError::Other(t) => write!(f, "llm: {t}"),
        }
    }
}

/// One event of the stream (`data: {…}`) into the content and the calls so far. An error in it is an error: OpenRouter
/// sends `{"error": {…}}` (and `finish_reason: "error"`) when the provider behind it fails midway — skipped, as an
/// event without `choices`, it would leave an empty answer and a silent station.
fn event(
    v: &Value,
    content: &mut String,
    calls: &mut Vec<Call>,
    on_text: &mut impl FnMut(&str),
) -> Result<(), LlmError> {
    let choice = v["choices"].get(0);
    if !v["error"].is_null() || choice.is_some_and(|c| c["finish_reason"] == "error") {
        let e = &v["error"];
        let why = match e["message"].as_str() {
            Some(m) if e["code"].is_null() => m.to_string(),
            Some(m) => {
                format!("{}: {m}", e["code"].as_str().map(String::from).unwrap_or_else(|| e["code"].to_string()))
            }
            None if e.is_null() => "finish_reason: error".to_string(),
            None => e.to_string(),
        };
        return Err(LlmError::Other(format!("an error in the stream: {}", why.chars().take(200).collect::<String>())));
    }
    let Some(d) = choice.map(|c| &c["delta"]) else { return Ok(()) };
    if let Some(t) = d["content"].as_str().filter(|t| !t.is_empty()) {
        content.push_str(t);
        on_text(t);
    }
    for tc in d["tool_calls"].as_array().into_iter().flatten() {
        let i = tc["index"].as_u64().unwrap_or(0) as usize;
        while calls.len() <= i {
            calls.push(Call::default());
        }
        let c = &mut calls[i];
        if let Some(id) = tc["id"].as_str().filter(|s| !s.is_empty()) {
            c.id = id.into();
        }
        if let Some(n) = tc["function"]["name"].as_str() {
            c.name.push_str(n);
        }
        if let Some(a) = tc["function"]["arguments"].as_str() {
            c.arguments.push_str(a);
        }
    }
    Ok(())
}

/// Resolves once `cancelled()` says so (checked every 50 ms).
async fn until(cancelled: &impl Fn() -> bool) {
    loop {
        if cancelled() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

impl Llm {
    pub fn new(url: &str, model: &str, key: &str, extra: Map<String, Value>) -> Llm {
        let http = crate::http().connect_timeout(Duration::from_secs(10)).build().expect("an HTTP client");
        Llm { http, url: url.into(), model: model.into(), key: key.into(), extra, cloud: crate::providers::cloud(url) }
    }

    /// A provider in the internet (https), not one at home.
    pub fn cloud(&self) -> bool {
        self.cloud
    }

    /// A model on 127.0.0.1 that plays one in the internet (the tests of who waits for whom).
    #[cfg(test)]
    pub fn into_cloud(mut self) -> Llm {
        self.cloud = true;
        self
    }

    fn request(&self, body: &Value, stream: bool) -> reqwest::RequestBuilder {
        let mut b = body.as_object().cloned().unwrap_or_default();
        b.insert("model".into(), json!(self.model));
        if stream {
            b.insert("stream".into(), json!(true));
        }
        for (k, v) in &self.extra {
            b.insert(k.clone(), v.clone());
        }
        let mut req = self.http.post(&self.url).json(&Value::Object(b));
        if !self.key.is_empty() {
            req = req.bearer_auth(&self.key);
        }
        req
    }

    async fn check(r: reqwest::Response) -> Result<reqwest::Response, LlmError> {
        if r.status().is_success() {
            return Ok(r);
        }
        let code = r.status().as_u16();
        let t = r.text().await.unwrap_or_default();
        Err(LlmError::Http(code, t.chars().take(200).collect()))
    }

    /// One streamed completion of `body` (messages, tools, …; the model and `stream` are added here). `on_text` gets
    /// every content delta. `wait`: the longest silence from the server — before the first byte and between any two.
    /// Stops reading (and so stops the model: the connection closes) once `cancelled()` says so. → (content, calls)
    pub async fn stream(
        &self,
        body: &Value,
        wait: Duration,
        mut on_text: impl FnMut(&str),
        cancelled: impl Fn() -> bool,
    ) -> Result<(String, Vec<Call>), LlmError> {
        let send = tokio::time::timeout(wait, self.request(body, true).send());
        let r = tokio::select! {
            r = send => r.map_err(|_| LlmError::Other("timed out".into()))?.map_err(|e| LlmError::Other(e.to_string()))?,
            _ = until(&cancelled) => return Ok((String::new(), vec![])),
        };
        let r = Self::check(r).await?;
        let mut content = String::new();
        let mut calls: Vec<Call> = vec![];
        let mut buf: Vec<u8> = vec![];
        let mut s = r.bytes_stream();
        'read: loop {
            let chunk = tokio::select! {
                c = tokio::time::timeout(wait, s.next()) => match c {
                    Err(_) => return Err(LlmError::Other("timed out".into())),
                    Ok(None) => break,
                    Ok(Some(c)) => c.map_err(|e| LlmError::Other(e.to_string()))?,
                },
                _ = until(&cancelled) => break,
            };
            buf.extend_from_slice(&chunk);
            while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buf.drain(..=nl).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else { continue };
                let data = data.trim();
                if data == "[DONE]" {
                    break 'read;
                }
                let v: Value =
                    serde_json::from_str(data).map_err(|e| LlmError::Other(format!("bad JSON in the stream: {e}")))?;
                event(&v, &mut content, &mut calls, &mut on_text)?;
            }
        }
        Ok((content, calls))
    }

    /// One whole completion → the server's JSON.
    pub async fn complete(&self, body: &Value, timeout: Duration) -> Result<Value, LlmError> {
        let r = self.request(body, false).timeout(timeout).send().await.map_err(|e| LlmError::Other(e.to_string()))?;
        let r = Self::check(r).await?;
        r.json().await.map_err(|e| LlmError::Other(e.to_string()))
    }
}

/// A model on 127.0.0.1 for the tests: OpenAI chat/completions, streamed and whole, no network. The last user line
/// decides how it answers: «slow» — after a second; «fail» — the model named "cloud" starts a word and breaks off with
/// OpenRouter's error event; «silent» — no words at all; else «Ответ <model>.». A whole completion (the warm-up, the
/// notes) takes `whole` ms. Every request is noted as it arrives.
#[cfg(test)]
pub mod fake {
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Clone, Debug)]
    pub struct Seen {
        pub model: String,
        pub text: String,
        pub stream: bool,
        pub at: Instant,
    }

    pub struct Model {
        pub url: String,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Model {
        pub async fn start(whole: u64) -> Model {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/v1/chat/completions", l.local_addr().unwrap());
            let seen: Arc<Mutex<Vec<Seen>>> = Default::default();
            let s2 = seen.clone();
            tokio::spawn(async move {
                while let Ok((s, _)) = l.accept().await {
                    tokio::spawn(serve(s, whole, s2.clone()));
                }
            });
            Model { url, seen }
        }

        pub fn seen(&self) -> Vec<Seen> {
            self.seen.lock().unwrap().clone()
        }
    }

    async fn serve(mut s: tokio::net::TcpStream, whole: u64, seen: Arc<Mutex<Vec<Seen>>>) {
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
        let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
        let len: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
            .unwrap_or(0);
        while buf.len() < head_end + len {
            let n = s.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                return;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let body: Value = serde_json::from_slice(&buf[head_end..head_end + len]).unwrap_or(json!({}));
        let model = body["model"].as_str().unwrap_or("").to_string();
        let text = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
            .find(|m| m["role"] == "user")
            .and_then(|m| m["content"].as_str())
            .unwrap_or("")
            .to_string();
        let stream = body["stream"] == json!(true);
        seen.lock().unwrap().push(Seen { model: model.clone(), text: text.clone(), stream, at: Instant::now() });
        if !stream {
            tokio::time::sleep(Duration::from_millis(whole)).await;
            let out = json!({"choices": [{"message": {"role": "assistant", "content": ""}}]}).to_string();
            let r = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len());
            let _ = s.write_all(r.as_bytes()).await;
            return;
        }
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await;
        if text.contains("slow") {
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }
        let delta = |t: &str| json!({"choices": [{"index": 0, "delta": {"content": t}}]});
        let events = if text.contains("silent") {
            vec![]
        } else if text.contains("fail") && model == "cloud" {
            vec![
                delta("Сейчас"),
                json!({"error": {"code": 502, "message": "Provider returned error"},
                                         "choices": [{"index": 0, "delta": {"content": ""}, "finish_reason": "error"}]}),
            ]
        } else {
            vec![delta(&format!("Ответ {model}."))]
        };
        for e in events {
            if s.write_all(format!("data: {e}\n\n").as_bytes()).await.is_err() {
                return;
            }
        }
        let _ = s.write_all(b"data: [DONE]\n\n").await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(lines: &[Value]) -> Result<(String, Vec<Call>, String), LlmError> {
        let (mut content, mut calls, mut heard) = (String::new(), vec![], String::new());
        for v in lines {
            event(v, &mut content, &mut calls, &mut |t: &str| heard.push_str(t))?;
        }
        Ok((content, calls, heard))
    }

    #[test]
    fn an_error_in_the_stream_is_an_error() {
        let d = |t: &str| json!({"choices": [{"index": 0, "delta": {"content": t}}]});
        // OpenRouter: the provider behind it fails midway
        let r = events(&[
            d("Сей"),
            json!({"error": {"code": 502, "message": "Provider returned error"},
                                         "choices": [{"index": 0, "delta": {"content": ""}, "finish_reason": "error"}]}),
        ]);
        assert!(
            matches!(&r, Err(LlmError::Other(e)) if e == "an error in the stream: 502: Provider returned error"),
            "{:?}",
            r.err()
        );
        assert!(
            matches!(events(&[json!({"error": {"message": "overloaded"}})]), Err(LlmError::Other(e)) if e.ends_with(": overloaded"))
        );
        assert!(matches!(events(&[json!({"error": "boom"})]), Err(LlmError::Other(e)) if e.ends_with("\"boom\"")));
        assert!(
            matches!(events(&[json!({"choices": [{"delta": {}, "finish_reason": "error"}]})]), Err(LlmError::Other(e)) if e.ends_with("finish_reason: error"))
        );
        // what is not an error: an event without choices (usage), "error": null, the usual ends
        let (content, calls, heard) = events(&[d("При"), json!({"usage": {"total_tokens": 3}}), json!({"error": null, "choices": [{"delta": {"content": "вет."}}]}),
                                                json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "c1", "function": {"name": "calc", "arguments": "{}"}}]},
                                                                    "finish_reason": "tool_calls"}]}),
                                                json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})]).unwrap();
        assert_eq!(
            (content.as_str(), heard.as_str(), calls.len(), calls[0].name.as_str()),
            ("Привет.", "Привет.", 1, "calc")
        );
    }

    #[tokio::test]
    async fn a_stream_broken_off_fails() {
        let m = fake::Model::start(0).await;
        let llm = Llm::new(&m.url, "cloud", "", Default::default());
        let mut heard = String::new();
        let r = llm
            .stream(
                &json!({"messages": [{"role": "user", "content": "fail"}]}),
                Duration::from_secs(5),
                |t| heard.push_str(t),
                || false,
            )
            .await;
        assert!(matches!(&r, Err(LlmError::Other(e)) if e.contains("502: Provider returned error")), "{:?}", r.err());
        assert_eq!(heard, "Сейчас", "what came before the error was heard");
        let (content, calls) = llm
            .stream(
                &json!({"messages": [{"role": "user", "content": "привет"}]}),
                Duration::from_secs(5),
                |_| {},
                || false,
            )
            .await
            .unwrap();
        assert_eq!((content.as_str(), calls.len()), ("Ответ cloud.", 0));
    }

    #[test]
    fn in_the_internet_by_its_address() {
        let a = Llm::new("https://openrouter.ai/api/v1/chat/completions", "m", "k", Map::new());
        assert!(a.cloud() && !Llm::new("http://127.0.0.1:52521/v1/chat/completions", "m", "", Map::new()).cloud());
        assert!(!Llm::new("https://gpu.lan/v1/chat/completions", "m", "", Map::new()).cloud());
        assert!(Llm::new("http://my-vps.example.com:8000/v1/chat/completions", "m", "", Map::new()).cloud());
    }
}
