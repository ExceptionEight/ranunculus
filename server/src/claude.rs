//! Claude through Anthropic's own API (the Messages API), for a connection to api.anthropic.com. The rest of the
//! server speaks OpenAI's chat completions; here is the translation both ways: a request into Claude's, its stream
//! and its whole answer back, its list of models into the settings' one.
//!
//! - What only Claude has — how much it thinks (effort), thinking off — is in the role's extra fields, as Claude takes
//!   them (`{"output_config": {"effort": "high"}}`, `{"thinking": {"type": "disabled"}}`). The sampling sent to other
//!   models (temperature) is not sent: Claude refuses it.
//! - The prompt is cached (it is the same from one round of a request to the next, and the tools never change).
//! - What Claude thought before a tool call goes back with the call in the same request's next round, as it came
//!   (Replay): it is only good in the conversation it was made in, and the server words the history anew for every
//!   request — so it is never given to another request. Refused all the same (a signature that does not fit), the
//!   round is asked again without it.
use crate::llm::Call;
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::sync::Mutex;

/// The API's version the requests are written for.
pub const VERSION: &str = "2026-10-21";
const SITE: &str = "https://api.anthropic.com";
/// Room for thinking: the server's own limit (a spoken answer's) would leave a model that thinks hard no words.
const MAX_TOKENS: u64 = 16_000;
/// Said when Claude declines to answer with nothing said yet: the session would else say «I lost the thought».
pub const DECLINED: &str = "С этим я не помогу.";
/// The levels of effort, from the least.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// A connection to Anthropic's API, by its address.
pub fn is_anthropic(url: &str) -> bool {
    crate::providers::site(url) == SITE
}

/// The API's root, «…/v1», whether the address was given with it or without.
pub fn base(url: &str) -> String {
    let u = url.trim_end_matches('/');
    if u.ends_with("/v1") {
        u.to_string()
    } else {
        format!("{u}/v1")
    }
}

/// What Claude wrote before its tool calls, by the first call's id: given back in the next round of that request.
#[derive(Default)]
pub struct Replay(Mutex<VecDeque<(String, Vec<Value>)>>);

impl Replay {
    /// An answer's blocks with calls in it (an answer in words ends the request: nothing to keep).
    pub fn keep(&self, blocks: Vec<Value>) {
        let Some(id) = blocks.iter().find(|b| b["type"] == "tool_use").and_then(|b| b["id"].as_str()) else { return };
        let mut q = self.0.lock().unwrap();
        q.push_back((id.to_string(), blocks));
        // a few requests at a time from a few stations: the rounds of each go one after another within seconds
        while q.len() > 64 {
            q.pop_front();
        }
    }

    fn get(&self, id: &str) -> Option<Vec<Value>> {
        self.0.lock().unwrap().iter().rev().find(|(i, _)| i == id).map(|(_, b)| b.clone())
    }
}

/// A message's text: a string, or OpenAI's parts.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

/// A JSON schema as Claude takes it for an answer's form: every object closed, no numeric or length limits.
fn strict(s: &Value) -> Value {
    match s {
        Value::Object(o) => {
            let mut out = Map::new();
            for (k, v) in o {
                if matches!(
                    k.as_str(),
                    "minimum"
                        | "maximum"
                        | "exclusiveMinimum"
                        | "exclusiveMaximum"
                        | "multipleOf"
                        | "minLength"
                        | "maxLength"
                        | "minItems"
                        | "maxItems"
                        | "uniqueItems"
                ) {
                    continue;
                }
                out.insert(k.clone(), strict(v));
            }
            if out.get("type").and_then(Value::as_str) == Some("object") {
                out.insert("additionalProperties".into(), json!(false));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(strict).collect()),
        v => v.clone(),
    }
}

/// A chat completion's request (`body`: messages, tools, …) as Claude's. `replay`: what was thought before this
/// request's own earlier calls goes back with them.
pub fn request(body: &Value, model: &str, extra: &Map<String, Value>, replay: Option<&Replay>, stream: bool) -> Value {
    let all = body["messages"].as_array().cloned().unwrap_or_default();
    let system: Vec<String> = all
        .iter()
        .filter(|m| m["role"] == "system")
        .map(|m| text_of(&m["content"]))
        .filter(|t| !t.is_empty())
        .collect();
    let mut messages: Vec<Value> = vec![];
    for m in all.iter().filter(|m| m["role"] != "system") {
        let calls = m["tool_calls"].as_array().cloned().unwrap_or_default();
        let (role, blocks) = match m["role"].as_str() {
            Some("assistant") => {
                let kept = calls.first().and_then(|c| c["id"].as_str()).and_then(|id| replay?.get(id));
                let blocks = kept.unwrap_or_else(|| {
                    let text = text_of(&m["content"]);
                    let mut b: Vec<Value> = vec![];
                    if !text.trim().is_empty() {
                        b.push(json!({"type": "text", "text": text.trim()}));
                    }
                    for c in &calls {
                        let args = c["function"]["arguments"].as_str().unwrap_or("{}");
                        let input =
                            serde_json::from_str::<Value>(args).ok().filter(Value::is_object).unwrap_or(json!({}));
                        b.push(
                            json!({"type": "tool_use", "id": c["id"], "name": c["function"]["name"], "input": input}),
                        );
                    }
                    b
                });
                ("assistant", blocks)
            }
            Some("tool") => (
                "user",
                vec![
                    json!({"type": "tool_result", "tool_use_id": m["tool_call_id"], "content": text_of(&m["content"])}),
                ],
            ),
            _ => {
                let text = text_of(&m["content"]);
                ("user", if text.trim().is_empty() { vec![] } else { vec![json!({"type": "text", "text": text})] })
            }
        };
        if blocks.is_empty() {
            continue; // an empty turn says nothing (and Claude takes none)
        }
        match messages.last_mut() {
            // two of one side in a row (a tool's results and the server's word after them) are one turn to Claude
            Some(last) if last["role"] == role => last["content"].as_array_mut().unwrap().extend(blocks),
            _ => messages.push(json!({"role": role, "content": blocks})),
        }
    }
    // a conversation begins with the user (a history cut where the assistant spoke first)
    if messages.first().is_some_and(|m| m["role"] == "assistant") {
        messages.insert(0, json!({"role": "user", "content": [{"type": "text", "text": "…"}]}));
    }
    let asked = body["max_tokens"].as_u64().unwrap_or(MAX_TOKENS);
    let mut b = Map::new();
    b.insert("model".into(), json!(model));
    b.insert("max_tokens".into(), json!(if asked == 0 { 0 } else { asked.max(MAX_TOKENS) }));
    if !system.is_empty() {
        b.insert("system".into(), json!(system.join("\n\n")));
    }
    b.insert("messages".into(), json!(messages));
    let tools: Vec<Value> = body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            let f = &t["function"];
            json!({"name": f["name"], "description": f["description"], "input_schema": f["parameters"]})
        })
        .collect();
    if !tools.is_empty() {
        b.insert("tools".into(), json!(tools));
        // «none» holds; a forced call («required») is left to the model: most of these models refuse being forced,
        // and none takes it while thinking
        if body["tool_choice"] == "none" {
            b.insert("tool_choice".into(), json!({"type": "none"}));
        }
    }
    let mut output = Map::new();
    if body["response_format"]["type"] == "json_schema" {
        let schema = &body["response_format"]["json_schema"]["schema"];
        output.insert("format".into(), json!({"type": "json_schema", "schema": strict(schema)}));
    }
    b.insert("cache_control".into(), json!({"type": "ephemeral"}));
    if stream {
        b.insert("stream".into(), json!(true));
    }
    for (k, v) in extra {
        match k.as_str() {
            // what other models are asked (a connection made for them before): Claude refuses sampling
            "temperature" | "top_p" | "top_k" => {}
            "output_config" => output.extend(v.as_object().cloned().unwrap_or_default()),
            _ => {
                b.insert(k.clone(), v.clone());
            }
        }
    }
    if !output.is_empty() {
        b.insert("output_config".into(), Value::Object(output));
    }
    Value::Object(b)
}

/// Claude's stream as it is read: the blocks as they come (text, thinking, tool calls), why it stopped.
#[derive(Default)]
pub struct Reading {
    blocks: Vec<Value>,
    /// a tool call's input so far, by its block
    inputs: Vec<String>,
    pub stop: Option<String>,
}

impl Reading {
    /// One event (`data: {…}`): the text goes to `content` and `on_text` as it comes. → Err: the stream's error.
    pub fn event(&mut self, v: &Value, content: &mut String, on_text: &mut impl FnMut(&str)) -> Result<(), String> {
        let i = v["index"].as_u64().unwrap_or(0) as usize;
        match v["type"].as_str().unwrap_or("") {
            "content_block_start" => {
                while self.blocks.len() <= i {
                    self.blocks.push(Value::Null);
                    self.inputs.push(String::new());
                }
                self.blocks[i] = v["content_block"].clone();
            }
            "content_block_delta" => {
                let (d, Some(b)) = (&v["delta"], self.blocks.get_mut(i)) else { return Ok(()) };
                let add = |b: &mut Value, k: &str, t: &str| {
                    let now = b[k].as_str().unwrap_or("").to_string();
                    b[k] = json!(now + t);
                };
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or("");
                        add(b, "text", t);
                        if !t.is_empty() {
                            content.push_str(t);
                            on_text(t);
                        }
                    }
                    "thinking_delta" => add(b, "thinking", d["thinking"].as_str().unwrap_or("")),
                    "signature_delta" => add(b, "signature", d["signature"].as_str().unwrap_or("")),
                    "input_json_delta" => self.inputs[i].push_str(d["partial_json"].as_str().unwrap_or("")),
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(s) = v["delta"]["stop_reason"].as_str() {
                    self.stop = Some(s.to_string());
                }
            }
            "error" => {
                let e = &v["error"];
                return Err(format!(
                    "{}: {}",
                    e["type"].as_str().unwrap_or("error"),
                    e["message"].as_str().unwrap_or("").chars().take(200).collect::<String>()
                ));
            }
            _ => {}
        }
        Ok(())
    }

    /// The answer read → its calls, and its blocks as Claude wrote them (for the next round).
    pub fn finish(mut self) -> (Vec<Call>, Vec<Value>) {
        let mut calls = vec![];
        for (b, input) in self.blocks.iter_mut().zip(&self.inputs) {
            if b["type"] == "tool_use" {
                let parsed = serde_json::from_str::<Value>(if input.is_empty() { "{}" } else { input })
                    .ok()
                    .filter(Value::is_object);
                b["input"] = parsed.unwrap_or(json!({}));
                calls.push(Call {
                    id: b["id"].as_str().unwrap_or("").to_string(),
                    name: b["name"].as_str().unwrap_or("").to_string(),
                    arguments: input.clone(),
                });
            }
        }
        let blocks = self.blocks.into_iter().filter(|b| !b.is_null()).collect();
        (calls, blocks)
    }
}

/// Claude's whole answer as a chat completion's (the warm-up, the notes, the settings' check read it so).
pub fn whole(v: &Value) -> Value {
    let blocks = v["content"].as_array().cloned().unwrap_or_default();
    let text: String = blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect();
    let calls: Vec<Value> = blocks
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| json!({"id": b["id"], "type": "function", "function": {"name": b["name"], "arguments": b["input"].to_string()}}))
        .collect();
    let finish = match v["stop_reason"].as_str() {
        Some("tool_use") => "tool_calls",
        Some("max_tokens") => "length",
        Some("refusal") => "content_filter",
        _ => "stop",
    };
    let mut message = json!({"role": "assistant", "content": text});
    if !calls.is_empty() {
        message["tool_calls"] = json!(calls);
    }
    json!({"model": v["model"], "choices": [{"index": 0, "message": message, "finish_reason": finish}], "usage": v["usage"]})
}

/// Anthropic's list of models as the settings show it: the newest first, as it comes, each with the levels of effort
/// it takes and how its thinking is turned off (`off`: null — it cannot be).
pub fn models(list: &Value) -> Vec<Value> {
    list["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str()?;
            let caps = &m["capabilities"];
            let efforts: Vec<&str> = if caps["effort"]["supported"] == json!(true) {
                EFFORTS.into_iter().filter(|e| caps["effort"][*e]["supported"] == json!(true)).collect()
            } else {
                vec![]
            };
            let types = &caps["thinking"]["types"];
            let off = ["disabled", "between_tools"].into_iter().find(|t| types[*t]["supported"] == json!(true));
            Some(json!({"id": id, "name": m["display_name"].as_str().unwrap_or(id), "context": m["max_input_tokens"],
                        "tools": true, "efforts": efforts, "off": off}))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extra(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn where_claude_is() {
        assert!(is_anthropic("https://api.anthropic.com/v1") && is_anthropic("https://API.anthropic.com"));
        assert!(!is_anthropic("https://openrouter.ai/api/v1") && !is_anthropic("http://api.anthropic.com.evil.org/v1"));
        assert_eq!(base("https://api.anthropic.com"), "https://api.anthropic.com/v1");
        assert_eq!(base("https://api.anthropic.com/v1/"), "https://api.anthropic.com/v1");
    }

    #[test]
    fn a_request_as_claude_takes_it() {
        let body = json!({
            "messages": [
                {"role": "system", "content": "Ты Мразота."},
                {"role": "assistant", "content": "Привет."}, // a history cut where the assistant spoke first
                {"role": "user", "content": "[пт] включи свет"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "home_set", "arguments": "{\"control\":\"light.l#power\",\"value\":\"true\"}"}},
                    {"id": "call_2", "type": "function", "function": {"name": "set_volume", "arguments": "not json"}}]},
                {"role": "tool", "tool_call_id": "call_1", "content": "{\"ok\":true}"},
                {"role": "tool", "tool_call_id": "call_2", "content": "{\"ok\":true}"},
                {"role": "assistant", "content": "Включила."},
                {"role": "user", "content": "Так включила или нет?"},
                {"role": "assistant", "content": ""},
            ],
            "tools": [{"type": "function", "function": {"name": "home_set", "description": "Умный дом", "parameters": {"type": "object"}}}],
            "tool_choice": "required", "temperature": 0.3, "max_tokens": 1500, "chat_template_kwargs": {"enable_thinking": false},
        });
        let r = request(
            &body,
            "claude-haiku-5-5",
            &extra(json!({"temperature": 1, "output_config": {"effort": "high"}})),
            None,
            true,
        );
        assert_eq!(r["system"], "Ты Мразота.");
        assert_eq!(
            (r["model"].as_str(), r["max_tokens"].as_u64(), r["stream"].as_bool()),
            (Some("claude-haiku-5-5"), Some(16_000), Some(true))
        );
        assert_eq!(r["output_config"], json!({"effort": "high"}));
        assert_eq!(r["cache_control"], json!({"type": "ephemeral"}));
        // nothing Claude refuses or does not know
        for k in ["temperature", "chat_template_kwargs", "tool_choice", "response_format"] {
            assert!(r.get(k).is_none(), "{k}");
        }
        assert_eq!(
            r["tools"],
            json!([{"name": "home_set", "description": "Умный дом", "input_schema": {"type": "object"}}])
        );
        let m = r["messages"].as_array().unwrap();
        let roles: Vec<&str> = m.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant", "user", "assistant", "user", "assistant", "user"]);
        assert_eq!(m[0]["content"][0]["text"], "…");
        assert_eq!(
            m[3]["content"],
            json!([{"type": "tool_use", "id": "call_1", "name": "home_set", "input": {"control": "light.l#power", "value": "true"}},
                   {"type": "tool_use", "id": "call_2", "name": "set_volume", "input": {}}])
        );
        // both results in one turn; the empty last answer is no turn at all
        assert_eq!(m[4]["content"].as_array().unwrap().len(), 2);
        assert_eq!(
            m[4]["content"][1],
            json!({"type": "tool_result", "tool_use_id": "call_2", "content": "{\"ok\":true}"})
        );
        assert_eq!(m[6]["content"][0]["text"], "Так включила или нет?");
        // «none» holds, and the warm-up writes the cache only
        let none = request(
            &json!({"messages": [{"role": "user", "content": "x"}], "tools": body["tools"], "tool_choice": "none", "max_tokens": 0}),
            "m",
            &Map::new(),
            None,
            false,
        );
        assert_eq!(
            (none["tool_choice"].clone(), none["max_tokens"].as_u64(), none.get("stream")),
            (json!({"type": "none"}), Some(0), None)
        );
    }

    #[test]
    fn the_notes_form_as_claude_takes_it() {
        let body = json!({"messages": [{"role": "user", "content": "x"}],
            "response_format": {"type": "json_schema", "json_schema": {"name": "notes", "schema": crate::memory::facts_schema()}}});
        let r = request(&body, "m", &extra(json!({"output_config": {"effort": "low"}})), None, false);
        let f = &r["output_config"]["format"];
        assert_eq!((f["type"].as_str(), r["output_config"]["effort"].as_str()), (Some("json_schema"), Some("low")));
        assert_eq!(f["schema"]["additionalProperties"], false);
        assert_eq!(f["schema"]["properties"]["update"]["items"]["additionalProperties"], false);
        assert_eq!(strict(&json!({"type": "integer", "minimum": 0, "maximum": 10})), json!({"type": "integer"}));
    }

    /// A stream as Claude sends it: it thinks, says a word, calls a tool.
    pub fn events() -> Vec<Value> {
        vec![
            json!({"type": "message_start", "message": {"id": "msg_1", "usage": {"input_tokens": 10}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Сейчас "}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "включу."}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "home_set", "input": {}}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"control\": \"light.l#po"}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "wer\", \"value\": \"true\"}"}}),
            json!({"type": "content_block_stop", "index": 2}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 40}}),
            json!({"type": "message_stop"}),
        ]
    }

    #[test]
    fn a_stream_read() {
        let (mut content, mut said) = (String::new(), vec![]);
        let mut r = Reading::default();
        for e in events() {
            r.event(&e, &mut content, &mut |t: &str| said.push(t.to_string())).unwrap();
        }
        assert_eq!(r.stop.as_deref(), Some("tool_use"));
        let (calls, blocks) = r.finish();
        assert_eq!((content.as_str(), said.len()), ("Сейчас включу.", 2));
        assert_eq!(
            (calls[0].id.as_str(), calls[0].name.as_str(), calls[0].arguments.as_str()),
            ("toolu_1", "home_set", "{\"control\": \"light.l#power\", \"value\": \"true\"}")
        );
        assert_eq!(blocks[0], json!({"type": "thinking", "thinking": "", "signature": "sig"}));
        assert_eq!(blocks[2]["input"], json!({"control": "light.l#power", "value": "true"}));
        // the next round of this request gives them back as they came; another request words its own history
        let replay = Replay::default();
        replay.keep(blocks.clone());
        let round = json!({"messages": [{"role": "user", "content": "включи свет"},
            {"role": "assistant", "content": "Сейчас включу.", "tool_calls": [calls[0].to_json()]},
            {"role": "tool", "tool_call_id": "toolu_1", "content": "{\"ok\":true}"}]});
        assert_eq!(request(&round, "m", &Map::new(), Some(&replay), true)["messages"][1]["content"], json!(blocks));
        assert_eq!(request(&round, "m", &Map::new(), None, true)["messages"][1]["content"][0]["type"], "text");
        // an error midway is an error
        let e = json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}});
        assert_eq!(
            Reading::default().event(&e, &mut String::new(), &mut |_: &str| {}).unwrap_err(),
            "overloaded_error: Overloaded"
        );
    }

    #[test]
    fn a_whole_answer_read() {
        let v = json!({"model": "m", "stop_reason": "tool_use", "usage": {"input_tokens": 3},
            "content": [{"type": "thinking", "thinking": "", "signature": "s"}, {"type": "text", "text": "{\"add\": []}"},
                        {"type": "tool_use", "id": "t", "name": "calculate", "input": {"expression": "2+2"}}]});
        let w = whole(&v);
        assert_eq!(w["choices"][0]["message"]["content"], "{\"add\": []}");
        assert_eq!(
            w["choices"][0]["message"]["tool_calls"][0]["function"],
            json!({"name": "calculate", "arguments": "{\"expression\":\"2+2\"}"})
        );
        assert_eq!(w["choices"][0]["finish_reason"], "tool_calls");
        // the warm-up's answer: nothing
        assert_eq!(whole(&json!({"content": [], "stop_reason": "max_tokens"}))["choices"][0]["message"]["content"], "");
    }

    #[test]
    fn the_models_and_what_each_takes() {
        let cap = |efforts: &[&str], off: &[&str]| {
            let mut e = json!({"supported": !efforts.is_empty()});
            for x in EFFORTS {
                e[x] = json!({"supported": efforts.contains(&x)});
            }
            let mut types = json!({});
            for t in ["enabled", "adaptive", "disabled", "between_tools"] {
                types[t] = json!({"supported": t == "adaptive" || off.contains(&t)});
            }
            json!({"effort": e, "thinking": {"supported": true, "types": types}})
        };
        let list = json!({"data": [
            {"id": "claude-haiku-5-5", "display_name": "Claude Haiku 5.5", "max_input_tokens": 1000000, "capabilities": cap(&EFFORTS, &["disabled"])},
            {"id": "claude-sonnet-5-5", "display_name": "Claude Sonnet 5.5", "max_input_tokens": 1000000, "capabilities": cap(&EFFORTS, &["between_tools"])},
            {"id": "claude-opus-5-5", "display_name": "Claude Opus 5.5", "max_input_tokens": 1000000, "capabilities": cap(&EFFORTS, &[])},
            {"id": "claude-haiku-4-5-20251001", "display_name": "Claude Haiku 4.5", "max_input_tokens": 200000, "capabilities": cap(&[], &["disabled"])},
        ], "has_more": false});
        let m = models(&list);
        let pick =
            |i: usize| (m[i]["id"].as_str().unwrap(), m[i]["efforts"].as_array().unwrap().len(), m[i]["off"].as_str());
        assert_eq!(pick(0), ("claude-haiku-5-5", 5, Some("disabled")));
        assert_eq!(pick(1), ("claude-sonnet-5-5", 5, Some("between_tools")));
        assert_eq!(pick(2), ("claude-opus-5-5", 5, None));
        assert_eq!(pick(3), ("claude-haiku-4-5-20251001", 0, Some("disabled")));
        assert_eq!((m[0]["name"].as_str(), m[0]["tools"].as_bool()), (Some("Claude Haiku 5.5"), Some(true)));
    }

    /// Anthropic's API as a few lines: each request is kept, and answered by the next of `answers` (status, body).
    async fn anthropic(answers: Vec<(u16, String)>) -> (String, std::sync::Arc<Mutex<Vec<(Value, String)>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", l.local_addr().unwrap());
        let seen: std::sync::Arc<Mutex<Vec<(Value, String)>>> = Default::default();
        let (kept, answers) = (seen.clone(), std::sync::Arc::new(Mutex::new(VecDeque::from(answers))));
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let (kept, answers) = (kept.clone(), answers.clone());
                tokio::spawn(async move {
                    let (mut buf, mut chunk) = (vec![], [0u8; 65536]);
                    let (head, len) = loop {
                        let n = s.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..i + 4]).to_ascii_lowercase();
                            let len: usize = head
                                .lines()
                                .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                                .unwrap_or(0);
                            break (head, i + 4 + len);
                        }
                    };
                    while buf.len() < len {
                        let n = s.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let start = head.len();
                    kept.lock().unwrap().push((serde_json::from_slice(&buf[start..len]).unwrap(), head));
                    let (code, body) = answers.lock().unwrap().pop_front().unwrap();
                    let kind = if body.starts_with("event:") { "text/event-stream" } else { "application/json" };
                    let r = format!("HTTP/1.1 {code} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    let _ = s.write_all(r.as_bytes()).await;
                });
            }
        });
        (base, seen)
    }

    fn sse(events: &[Value]) -> String {
        events.iter().map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap())).collect()
    }

    fn said(text: &str) -> Vec<Value> {
        vec![
            json!({"type": "message_start", "message": {"id": "m"}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
            json!({"type": "message_stop"}),
        ]
    }

    #[tokio::test]
    async fn a_request_and_its_rounds_with_claude() {
        let refused = json!({"type": "error", "error": {"type": "invalid_request_error",
                             "message": "messages.1.content.0: Invalid `signature` in `thinking` block."}});
        let (base, seen) = anthropic(vec![
            (200, sse(&events())),
            (400, refused.to_string()),
            (200, sse(&said("Включила."))),
            (
                200,
                sse(&[
                    json!({"type": "message_start", "message": {}}),
                    json!({"type": "message_delta", "delta": {"stop_reason": "refusal"}}),
                ]),
            ),
            (200, json!({"content": [{"type": "text", "text": "готово"}], "stop_reason": "end_turn"}).to_string()),
        ])
        .await;
        let llm = crate::llm::Llm::claude(
            &base,
            "claude-haiku-5-5",
            "sk-ant-test",
            extra(json!({"output_config": {"effort": "high"}})),
        );
        let wait = std::time::Duration::from_secs(5);
        let mut msgs = vec![
            json!({"role": "system", "content": "Ты Мразота."}),
            json!({"role": "user", "content": "включи свет"}),
        ];
        let body = |m: &Vec<Value>| json!({"messages": m, "tools": [], "temperature": 0.3, "max_tokens": 1500});
        let mut heard = vec![];
        let (text, calls) = llm.stream(&body(&msgs), wait, |t| heard.push(t.to_string()), || false).await.unwrap();
        assert_eq!((text.as_str(), calls.len(), heard.len()), ("Сейчас включу.", 1, 2));
        msgs.push(json!({"role": "assistant", "content": text, "tool_calls": [calls[0].to_json()]}));
        msgs.push(json!({"role": "tool", "tool_call_id": calls[0].id, "content": "{\"ok\":true}"}));
        let (text, calls) = llm.stream(&body(&msgs), wait, |_| {}, || false).await.unwrap();
        assert_eq!((text.as_str(), calls.len()), ("Включила.", 0));
        // declined with nothing said: a word of it, not silence
        let mut heard = vec![];
        let (text, _) = llm.stream(&body(&msgs), wait, |t| heard.push(t.to_string()), || false).await.unwrap();
        assert_eq!((text.as_str(), heard), (DECLINED, vec![DECLINED.to_string()]));
        // a whole answer, as a chat completion
        let v = llm
            .complete(&json!({"messages": [{"role": "user", "content": "Ответь: готово"}], "max_tokens": 400}), wait)
            .await
            .unwrap();
        assert_eq!(v["choices"][0]["message"]["content"], "готово");
        let seen = seen.lock().unwrap();
        let (first, head) = &seen[0];
        assert!(
            head.starts_with("post /v1/messages ")
                && head.contains("x-api-key: sk-ant-test")
                && head.contains(&format!("anthropic-version: {VERSION}"))
        );
        assert!(!head.contains("authorization"));
        assert_eq!((first["output_config"]["effort"].as_str(), first.get("temperature")), (Some("high"), None));
        // the next round: what was thought before the call goes back with it; refused, the round is asked without it
        assert_eq!(
            seen[1].0["messages"][1]["content"][0],
            json!({"type": "thinking", "thinking": "", "signature": "sig"})
        );
        assert_eq!(seen[2].0["messages"][1]["content"][0], json!({"type": "text", "text": "Сейчас включу."}));
        assert_eq!(seen[2].0["messages"][1]["content"][1]["type"], "tool_use");
        assert_eq!(seen[4].0.get("stream"), None);
    }
}
