//! Google's Gemini API, for a connection to generativelanguage.googleapis.com: one key does every role, each the way
//! Google gives it.
//!
//! - Answers and the memory's vectors go by its OpenAI-compatible API (…/v1beta/openai), the language the rest of the
//!   server speaks. What that API takes differently is put right here (`request`): a field it does not know is
//!   refused whole, and thinking — which these models cannot be told to leave — counts into an answer's length.
//! - What a model thought before a tool call (its «thought signature», beside the call) goes back with the call in
//!   the same request's next round, as it came (Signatures): Gemini refuses the round without it. Calls of earlier
//!   requests need none, and the server words the history anew for every request — so none is kept longer.
//! - The voice and the recognition are in Google's own API only: the voice as a stream of sound (`speak`), the
//!   recognition as an «interaction» (`transcribe`). A voice has no speed there (the settings refuse one).
use crate::services::{sent, Endpoint};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

const SITE: &str = "https://generativelanguage.googleapis.com";
/// Room for thinking: the server's own limit (a spoken answer's) would leave a model that thinks hard no words.
const MAX_TOKENS: u64 = 16_000;
/// The levels of thinking every Gemini model takes (`reasoning_effort`), from the least. None turns it off.
pub const EFFORTS: [&str; 3] = ["low", "medium", "high"];
/// The language the stations are spoken to in: a hint to the recognition, and the voices offered.
const LANGUAGE: &str = "ru-RU";

/// A connection to Google's API, by its address.
pub fn is_google(url: &str) -> bool {
    crate::providers::site(url) == SITE
}

/// The API's root, «…/v1beta», whatever the address was given with: nothing, or the OpenAI-compatible «…/openai».
pub fn base(url: &str) -> String {
    let u = url.trim_end_matches('/');
    let u = u.strip_suffix("/openai").unwrap_or(u);
    if u.split_once("://").is_some_and(|(_, rest)| rest.contains('/')) {
        u.to_string()
    } else {
        format!("{u}/v1beta")
    }
}

/// What came beside a model's tool calls (`extra_content`: the thought signature), by the call's id: given back in
/// the next round of that request.
#[derive(Default)]
pub struct Signatures(Mutex<VecDeque<(String, Value)>>);

impl Signatures {
    /// An answer's tool calls, as a stream's event or a whole answer has them.
    pub fn keep(&self, calls: &Value) {
        let mut q = self.0.lock().unwrap();
        for c in calls.as_array().into_iter().flatten() {
            if let (Some(id), Some(x)) = (c["id"].as_str(), c.get("extra_content").filter(|x| !x.is_null())) {
                q.push_back((id.to_string(), x.clone()));
            }
        }
        // a few requests at a time from a few stations: the rounds of each go one after another within seconds
        while q.len() > 64 {
            q.pop_front();
        }
    }

    fn get(&self, id: &str) -> Option<Value> {
        self.0.lock().unwrap().iter().rev().find(|(i, _)| i == id).map(|(_, x)| x.clone())
    }
}

/// A chat completion's request (`body`: messages, tools, …) as Google's OpenAI-compatible API takes it. `kept`: what
/// came beside this request's own earlier calls goes back with them.
pub fn request(body: &Value, model: &str, extra: &Map<String, Value>, kept: &Signatures, stream: bool) -> Value {
    let mut b = body.as_object().cloned().unwrap_or_default();
    // a field it does not know is refused with the whole request (what a model at home is told); the sampling is
    // ignored by the models of now and, Google says, will be refused by those to come
    for k in ["chat_template_kwargs", "temperature", "top_p", "top_k"] {
        b.remove(k);
    }
    if let Some(asked) = b.get("max_tokens").and_then(Value::as_u64) {
        b.insert("max_tokens".into(), json!(asked.max(MAX_TOKENS)));
    }
    let messages = b.get_mut("messages").and_then(Value::as_array_mut).into_iter().flatten();
    for c in messages.filter_map(|m| m.get_mut("tool_calls")?.as_array_mut()).flatten() {
        if let Some(x) = c["id"].as_str().and_then(|id| kept.get(id)) {
            c["extra_content"] = x;
        }
    }
    b.insert("model".into(), json!(model));
    if stream {
        b.insert("stream".into(), json!(true));
    }
    for (k, v) in extra {
        b.insert(k.clone(), v.clone());
    }
    Value::Object(b)
}

/// Google's error (`{"error": {…}}`) in a line. A limit run into is named by what it is («requests per minute … on
/// Tier 1», and how many): the message itself opens with advice and links, and the limit comes after two hundred
/// characters of them.
fn why_not(text: &str) -> String {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let e = &v["error"];
    let limit = e["details"].as_array().into_iter().flatten().find_map(|d| {
        let x = d["violations"].get(0)?;
        let what = x["description"].as_str()?.split(". ").next()?.trim_end_matches('.');
        Some(format!("{what} (limit: {})", x["quotaValue"].as_str().unwrap_or("?")))
    });
    let said = limit.or(e["message"].as_str().map(String::from)).unwrap_or_else(|| text.to_string());
    said.chars().take(200).collect()
}

/// A failed answer in a line: the status and why.
async fn refused(r: reqwest::Response) -> (u16, String) {
    let code = r.status().as_u16();
    let why = why_not(&r.text().await.unwrap_or_default());
    (code, if why.is_empty() { format!("HTTP {code}") } else { format!("HTTP {code}: {why}") })
}

/// The model's name as the API's addresses take it: the list gives «models/…», the settings keep it without.
fn name(model: &str) -> &str {
    model.trim_start_matches("models/")
}

/// A sentence in the endpoint's voice → the sound (s16le mono) and its rate. Asked for as a stream: the sound is all
/// there in half the time of the same sound asked for whole. → Err: (the HTTP status, if that is what failed; why).
pub async fn speak(http: &reqwest::Client, e: &Endpoint, text: &str) -> Result<(Vec<u8>, u32), (Option<u16>, String)> {
    let mut config = json!({"responseModalities": ["AUDIO"]});
    if !e.voice.is_empty() {
        config["speechConfig"] = json!({"voiceConfig": {"voice": e.voice}});
    }
    let body = json!({"contents": [{"role": "user", "parts": [{"text": text}]}], "generationConfig": config});
    let url = format!("{}/models/{}:streamGenerateContent?alt=sse", e.url, name(&e.model));
    let ask = || http.post(&url).timeout(Duration::from_secs(60)).header("x-goog-api-key", &e.key).json(&body);
    let r = sent("tts", ask).await.map_err(|e| (None, e))?;
    if !r.status().is_success() {
        let (code, why) = refused(r).await;
        return Err((Some(code), why));
    }
    let (mut pcm, mut rate, mut stopped) = (vec![], 24000, String::new());
    let (mut buf, mut s) = (Vec::<u8>::new(), r.bytes_stream());
    while let Some(chunk) = s.next().await {
        buf.extend_from_slice(&chunk.map_err(|e| (None, crate::why(&e)))?);
        while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line);
            let Some(data) = line.trim().strip_prefix("data:") else { continue };
            let v: Value =
                serde_json::from_str(data.trim()).map_err(|e| (None, format!("bad JSON in the stream: {e}")))?;
            if !v["error"].is_null() {
                return Err((None, format!("an error in the stream: {}", why_not(data))));
            }
            let c = &v["candidates"][0];
            if let Some(why) = c["finishReason"].as_str() {
                stopped = why.to_string();
            }
            // the sound comes in pieces, each named «audio/l16; rate=24000; channels=1»; the last part is empty text
            for d in c["content"]["parts"].as_array().into_iter().flatten().map(|p| &p["inlineData"]) {
                let Some(data) = d["data"].as_str() else { continue };
                pcm.extend(BASE64.decode(data).map_err(|e| (None, format!("bad sound in the stream: {e}")))?);
                let named = |p: &str| p.trim().strip_prefix("rate=")?.parse::<u32>().ok();
                rate = d["mimeType"].as_str().and_then(|m| m.split(';').find_map(named)).unwrap_or(rate);
            }
        }
    }
    if pcm.is_empty() && !matches!(stopped.as_str(), "" | "STOP") {
        return Err((None, format!("the voice answered with no sound ({stopped})")));
    }
    Ok((pcm, rate))
}

/// A WAV file → what is said in it. Not kept at Google: an interaction stays there for weeks unless it is told not to.
pub async fn transcribe(http: &reqwest::Client, e: &Endpoint, wav: &[u8]) -> Result<String, String> {
    let body = json!({"model": name(&e.model), "store": false,
        "input": [{"type": "audio", "data": BASE64.encode(wav), "mime_type": "audio/wav"}],
        "generation_config": {"transcription_config": {"language_codes": [LANGUAGE]}}});
    let url = format!("{}/interactions", e.url);
    let ask = || http.post(&url).timeout(Duration::from_secs(30)).header("x-goog-api-key", &e.key).json(&body);
    let r = sent("asr", ask).await?;
    if !r.status().is_success() {
        return Err(refused(r).await.1);
    }
    let v: Value = r.json().await.map_err(|e| e.to_string())?;
    if let Some(status) = v["status"].as_str().filter(|s| *s != "completed") {
        return Err(format!("the recognition stopped: {status}"));
    }
    // nothing heard: no steps at all
    let steps = v["steps"].as_array().into_iter().flatten().filter(|s| s["type"] == "model_output");
    let said: Vec<&str> =
        steps.flat_map(|s| s["content"].as_array().into_iter().flatten()).filter_map(|c| c["text"].as_str()).collect();
    Ok(said.join(" "))
}

/// A list of Google's API whole, page by page: `what` — «models», «voices».
pub async fn list(http: &reqwest::Client, base: &str, what: &str, key: &str) -> Result<Vec<Value>, String> {
    let (url, mut items, mut page) = (format!("{base}/{what}"), vec![], None::<String>);
    loop {
        let mut req = http.get(&url).header("x-goog-api-key", key).query(&[("pageSize", "1000")]);
        if let Some(p) = &page {
            req = req.query(&[("pageToken", p)]);
        }
        let r = req.send().await.map_err(|e| format!("{url}: {}", crate::why(&e)))?;
        if !r.status().is_success() {
            return Err(format!("{url}: {}", refused(r).await.1));
        }
        let j: Value = r.json().await.map_err(|e| format!("{url}: {e}"))?;
        items.extend(j[what].as_array().cloned().unwrap_or_default());
        // the models' list names the next page one way, the voices' another
        match j["nextPageToken"].as_str().or(j["next_page_token"].as_str()) {
            Some(next) if !next.is_empty() && items.len() < 20_000 => page = Some(next.to_string()),
            _ => return Ok(items),
        }
    }
}

/// Google's list of models as the settings show it for a role, the newest first. The list does not say what a model
/// is for, only how it is asked: the voices and the recognizers are told by their names, the models that answer by
/// their thinking (the ones that draw are among them; the ones that sing and speak are not). A Gemini takes the
/// levels of EFFORTS; a voice model gets `voices`.
pub fn models(list: &[Value], role: &str, voices: &[String]) -> Vec<Value> {
    let item = |m: &Value| {
        let id = name(m["name"].as_str()?);
        let asked = |how: &str| m["supportedGenerationMethods"].as_array().is_some_and(|a| a.iter().any(|x| x == how));
        let fits = match role {
            "embeddings" => asked("embedContent"),
            "tts" => asked("generateContent") && id.contains("tts"),
            "asr" => asked("generateContent") && id.contains("transcribe"),
            _ => asked("generateContent") && m["thinking"] == json!(true),
        };
        if !fits {
            return None;
        }
        let mut item = json!({"id": id, "name": m["displayName"].as_str().unwrap_or(id)});
        match role {
            "llm" => {
                let efforts: &[&str] = if id.starts_with("gemini-") { &EFFORTS } else { &[] };
                item["context"] = m["inputTokenLimit"].clone();
                item["efforts"] = json!(efforts);
                item["off"] = Value::Null;
            }
            "tts" => item["voices"] = json!(voices),
            _ => {}
        }
        Some(item)
    };
    list.iter().rev().filter_map(item).collect()
}

/// The voices to offer of Google's two thousand: the language's own first, then those of no one language. A
/// language's own are named by it («ru-ru-podcaster-1»); the thirty that speak any are named by a word («kore»).
pub fn voices(list: &[Value]) -> Vec<String> {
    let ours = LANGUAGE.to_lowercase();
    let of = |own: bool| {
        let ours = ours.clone();
        list.iter().filter_map(move |v| {
            let (id, language) = (v["id"].as_str()?, v["language_code"].as_str()?.to_lowercase());
            let fits =
                if own { language == ours } else { language != ours && !id.starts_with(&format!("{language}-")) };
            fits.then(|| id.to_string())
        })
    };
    of(true).chain(of(false)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_google_is() {
        assert!(is_google("https://generativelanguage.googleapis.com/v1beta/openai"));
        assert!(is_google("https://GenerativeLanguage.googleapis.com"));
        assert!(
            !is_google("https://openrouter.ai/api/v1")
                && !is_google("https://generativelanguage.googleapis.com.evil.org")
        );
        let root = "https://generativelanguage.googleapis.com/v1beta";
        for given in [
            "https://generativelanguage.googleapis.com",
            "https://generativelanguage.googleapis.com/v1beta/",
            "https://generativelanguage.googleapis.com/v1beta/openai/",
        ] {
            assert_eq!(base(given), root, "{given}");
        }
    }

    #[test]
    fn a_request_as_google_takes_it() {
        let kept = Signatures::default();
        // a stream's event: two calls at once, the signature beside the first only
        kept.keep(&json!([
            {"id": "call_1", "type": "function", "function": {"name": "home_set", "arguments": "{}"}, "extra_content": {"google": {"thought_signature": "sig"}}},
            {"id": "call_2", "type": "function", "function": {"name": "set_volume", "arguments": "{}"}},
        ]));
        let call =
            |id: &str| json!({"id": id, "type": "function", "function": {"name": "home_set", "arguments": "{}"}});
        let body = json!({
            "messages": [
                {"role": "system", "content": "Ты Мразота."},
                {"role": "user", "content": "выключи свет"},
                {"role": "assistant", "content": null, "tool_calls": [call("call_0")]}, // an earlier request's
                {"role": "tool", "tool_call_id": "call_0", "content": "{\"ok\":true}"},
                {"role": "assistant", "content": "Выключила."},
                {"role": "user", "content": "включи свет и сделай потише"},
                {"role": "assistant", "content": null, "tool_calls": [call("call_1"), call("call_2")]},
                {"role": "tool", "tool_call_id": "call_1", "content": "{\"ok\":true}"},
                {"role": "tool", "tool_call_id": "call_2", "content": "{\"ok\":true}"},
            ],
            "tools": [{"type": "function", "function": {"name": "home_set", "parameters": {"type": "object"}}}],
            "tool_choice": "required", "temperature": 0.3, "max_tokens": 1500, "chat_template_kwargs": {"enable_thinking": false},
        });
        let extra = json!({"reasoning_effort": "low"}).as_object().cloned().unwrap();
        let r = request(&body, "gemini-3.8-flash", &extra, &kept, true);
        // nothing Google refuses, and room to think
        assert!(r.get("chat_template_kwargs").is_none() && r.get("temperature").is_none());
        assert_eq!(
            (r["model"].as_str(), r["max_tokens"].as_u64(), r["stream"].as_bool(), r["reasoning_effort"].as_str()),
            (Some("gemini-3.8-flash"), Some(16_000), Some(true), Some("low"))
        );
        assert_eq!((r["tool_choice"].clone(), r["tools"].clone()), (json!("required"), body["tools"].clone()));
        // the signature back beside the call it came with, and nowhere else
        let calls = |i: usize| r["messages"][i]["tool_calls"].clone();
        assert_eq!(calls(6)[0]["extra_content"], json!({"google": {"thought_signature": "sig"}}));
        assert_eq!((calls(6)[1].get("extra_content"), calls(2)[0].get("extra_content")), (None, None));
        assert_eq!(calls(6)[0]["function"], call("call_1")["function"]);
        assert_eq!(r["messages"][4], body["messages"][4], "a message without calls gets no field of them");
        // a whole answer (the notes): nothing added that was not asked
        let whole = request(&json!({"messages": [{"role": "user", "content": "x"}]}), "m", &Map::new(), &kept, false);
        assert_eq!(whole, json!({"messages": [{"role": "user", "content": "x"}], "model": "m"}));
    }

    #[test]
    fn a_refusal_in_a_line() {
        let quota = json!({"error": {"code": 429, "status": "RESOURCE_EXHAUSTED",
            "message": "You exceeded your current quota, please check your plan and billing details. For more information on this error, head to a page.",
            "details": [{"@type": "type.googleapis.com/google.rpc.Help", "links": []},
                        {"@type": "type.googleapis.com/google.rpc.QuotaFailure", "violations": [{
                            "description": "Generate content requests per minute limit exceeded on Tier 1. See a page.", "quotaValue": "10"}]}]}});
        assert_eq!(
            why_not(&quota.to_string()),
            "Generate content requests per minute limit exceeded on Tier 1 (limit: 10)"
        );
        let plain = json!({"error": {"message": "Model 'gemini-nope' not found.", "code": "not_found"}});
        assert_eq!(why_not(&plain.to_string()), "Model 'gemini-nope' not found.");
        assert_eq!((why_not(""), why_not("Bad Gateway")), (String::new(), "Bad Gateway".to_string()));
    }

    #[test]
    fn the_models_of_each_role_and_the_voices() {
        let model = |name: &str, how: &[&str], thinking: bool| {
            let mut m = json!({"name": format!("models/{name}"), "displayName": name.to_uppercase(), "inputTokenLimit": 1000, "supportedGenerationMethods": how});
            if thinking {
                m["thinking"] = json!(true);
            }
            m
        };
        let list = [
            model("gemini-2.5-flash", &["generateContent", "countTokens"], true),
            model("gemma-4-31b-it", &["generateContent"], true),
            model("gemini-3.5-transcribe", &["generateContent"], false),
            model("gemini-4-neon", &["generateContent"], true),
            model("lyria-3.5", &["generateContent"], false),
            model("gemini-3.8-flash-tts", &["generateContent"], false),
            model("gemini-embedding-2", &["embedContent"], false),
            model("gemini-3.5-transcribe-live", &["bidiGenerateContent"], false),
            model("gemini-3.8-live", &["bidiGenerateContent"], true),
        ];
        let ids = |role: &str| {
            models(&list, role, &[]).iter().map(|m| m["id"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        };
        assert_eq!(ids("llm"), ["gemini-4-neon", "gemma-4-31b-it", "gemini-2.5-flash"]);
        assert_eq!(
            (ids("asr"), ids("tts")),
            (vec!["gemini-3.5-transcribe".to_string()], vec!["gemini-3.8-flash-tts".to_string()])
        );
        assert_eq!(ids("embeddings"), ["gemini-embedding-2"]);
        let llm = models(&list, "llm", &[]);
        assert_eq!(
            llm[0],
            json!({"id": "gemini-4-neon", "name": "GEMINI-4-NEON", "context": 1000, "efforts": ["low", "medium", "high"], "off": null})
        );
        assert_eq!(llm[1]["efforts"], json!([]), "not a Gemini: no levels of ours");
        // the voices: the language's own first, then those of no one language
        let voice = |id: &str, language: &str| json!({"id": id, "type": "prebuilt", "language_code": language});
        let all = [
            voice("achernar", "en-US"),
            voice("en-us-anchor-1", "en-US"),
            voice("kore", "en-US"),
            voice("ru-ru-concierge-1", "ru-RU"),
            voice("ru-ru-podcaster-1", "ru-RU"),
            voice("sr-rs-anchor-1", "sr-RS"),
        ];
        let offered = voices(&all);
        assert_eq!(offered, ["ru-ru-concierge-1", "ru-ru-podcaster-1", "achernar", "kore"]);
        assert_eq!(models(&list, "tts", &offered)[0]["voices"], json!(offered));
    }
}
