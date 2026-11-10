//! The dialog: the model with tools and memory, and the guards that keep what is said true to what was done.
//!
//! Working memory: the last exchanges in the prompt — between KEEP_MIN and KEEP_MAX, cut in blocks so that the prompt
//! prefix a model has cached survives most requests — each user line stamped with its time. Loaded from memory.db
//! at start: a restart forgets nothing. Older exchanges: the recall tool. Notes about the user (notes.rs): in the
//! system prompt. The current time is the stamp of the last line, not in the system prompt: there it would change
//! every minute and make a model at home read the whole history again.
//!
//! The guards:
//! - CLAIM: the first sentence says a command is done («Включаю …», «Пауза.») and no tool was called — held back, and
//!   the model is asked again (a call required when the user's words are a command at all);
//! - FACT: a factual question answered with numbers and units from the model's own head — held back, checked by
//!   search first; CHALLENGE: the user says the answer was wrong — a search (or the right tool) before any answer;
//! - MARKUP: a tool call the model's server did not parse, come as text — never said; tool_args: junk in arguments
//!   is cut so that the next round does not fail on it;
//! - 5xx from the model's server: once or twice again; a forced call it could not parse: again, not forced;
//! - no words at all after the retries: an error, as a failed request (nothing is kept, the session says so).
//!
//! Who waits for whom:
//! - a device: one request at a time — an interrupted one winds down before its next one starts;
//! - a model at home: one call at a time (a local server has one slot and one model loaded) — the warm-up, the
//!   notes and each round of a request to it take turns; a model in the internet waits for nobody.
//!
//! Requests of different devices may run side by side; each sees the history as it was when it started, and its
//! exchange goes into the history in the order of the time it was said (as memory.db gives it back after a restart).
use crate::llm::{Call, Llm, LlmError};
use crate::memory::Memory;
use crate::session::log;
use crate::text::{now_s, stamp, tz, year, Sentences};
use crate::tools::{action_re, schemas, volume_change, Tools};
use chrono::TimeZone;
use regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const ROUNDS: usize = 6; // model turns per request: search, a page that refuses robots, another page, the answer…
const KEEP_MIN: usize = 40;
const KEEP_MAX: usize = 60;
const LOAD: usize = 50;
const FULL_FOR: f64 = 600.0; // s the last exchange keeps its tool calls and results in the prompt («дальше» needs the page)
const CLOUD_WAIT: Duration = Duration::from_secs(10); // without a byte from a model in the internet (the first comes in about a second)
const LOCAL_WAIT: Duration = Duration::from_secs(120);
const CLAIM_GUARD: bool = true;
const FACT_GUARD: bool = true;

/// The system prompt: `{where}` (WHERE_*), `{address}` and `{home}` (what the settings say about the home), `{year}`,
/// `{facts}` (the notes). A prompt.md in the data folder replaces it.
const PROMPT: &str = include_str!("prompt.md");
const WHERE_HOME: &str = "Ты живёшь дома у пользователя, а не в облаке и не на чужих серверах.";
const WHERE_CLOUD: &str = "Ты живёшь в колонке у пользователя дома, а думаешь через облачный сервис в интернете.";

const NUDGE: &str =
    "[служебное, не от хозяина: ты сказала, что выполнила команду, но не вызвала инструмент. Если это команда \
(музыка, громкость, таймер) — вызови нужный инструмент, потом коротко подтверди. Если просто ответ — ответь ещё раз. \
Хозяин твоей первой попытки не слышал: про заминку не говори.]";
const FACT_NUDGE: &str = "[служебное, не от хозяина: ты назвала цифры о реальном мире по памяти. Проверь через web_search и ответь \
по найденному, коротко; если в найденном нет ответа — так и скажи. Хозяин твоей первой попытки не слышал: про проверку не говори.]";

macro_rules! rx {
    ($name:ident, $re:expr) => {
        fn $name() -> &'static Regex {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| Regex::new($re).unwrap())
        }
    };
}

// The first sentence says a command is done.
rx!(
    claim,
    r"(?i)^\W*(включаю|включила|продолжаю|выключаю|выключила|переключаю|переключила|ставлю на паузу|поставила на паузу|пауза\W*$|ставлю таймер|поставила таймер|засекаю|засекла|убавила|прибавила|сделала (громче|тише|погромче|потише))"
);
// A question about the world (and not about the station), and numbers with units: an answer from the model's own head.
rx!(
    fact_q,
    r"(?i)\b(сколько|как(ой|ая|ое|ие|ую|ов|ова|ово)|с какой|насколько|когда|где|кто|почему|правда|точно|разве|ты же|но ты|быстрее|медленнее|больше|меньше|выше|ниже|тяжелее|дальше|старше)\b"
);
rx!(
    not_fact,
    r"(?i)громк|тише|громче|музык|песн|трек|играет|таймер|будильник|погод|который час|сколько времени|(какое|какой) (сегодня )?(число|день)|день недели|ты кто|как тебя|как дела|настроени|как ты"
);
rx!(
    num_unit,
    r"(?i)\d[\d\s.,]*(?:\s*(?:-|–|—|до|или|и)\s*\d[\d.,]*)?\s*(?:км|километр|метр|сантиметр|миллиметр|кг|килограмм|грамм|тонн|лет\b|год|век|градус|процент|%|миллион|миллиард|тысяч|литр|узл|мил[ья]|человек|штук|калори|ватт|вольт|гектар|квадратн|световых)"
);
// «Ты же сказала, что они плывут быстрее»: caught out, a model apologises and makes up new numbers.
rx!(
    challenge,
    r"(?i)\b(ты же|но ты( же)?|ты (ведь|сама)|а ты) (сказал|говорил)|ты ошиб|это не так|неправда|не может быть|ты путаешь|ты не прав|что-то не то|не сходится|противореч"
);
// a model's own tool-call syntax, come as text when its server failed to parse it
rx!(
    markup,
    r#"(?s)<\|?tool_call\|?>.*?(?:<\|?/?tool_call\|?>|$)|<\|?/?(?:tool_call|channel|"|im_start|im_end|turn)\|?>|</?think>|\bcall:\w+\{[^}]*\}?"#
);
rx!(word, r"\w");
rx!(dated, r"\b(\d\d\.\d\d)(?:\.\d{4})?\b");
rx!(args_cut, r"<\|?/?(?:tool_call|channel)");
rx!(args_pair, r#""(\w+)"\s*:\s*(?:"([^"<]*)"?|(-?\d+(?:\.\d+)?))"#);

/// A tool call's arguments as an object. A model at home now and then streams junk into them —
/// '{"expression":"(15*60+40) - (50 + 10)}<tool_call|>><|channel>thought…' — and that call, sent back in the next
/// round, makes its server answer 500 to every retry. What can be saved is: "key": "value" / number pairs before it.
pub fn tool_args(raw: &str) -> Map<String, Value> {
    let raw = if raw.is_empty() { "{}" } else { raw };
    if let Ok(v) = serde_json::from_str::<Value>(raw) {
        return match v {
            Value::Object(o) => o,
            _ => Map::new(),
        };
    }
    let head = args_cut().split(raw).next().unwrap_or("");
    let mut out = Map::new();
    for c in args_pair().captures_iter(head) {
        let k = c[1].to_string();
        let v = match c.get(3) {
            Some(n) if !n.as_str().is_empty() => {
                let n = n.as_str();
                if n.contains('.') {
                    json!(n.parse::<f64>().unwrap_or(0.0))
                } else {
                    n.parse::<i64>().map(|i| json!(i)).unwrap_or_else(|_| json!(n.parse::<f64>().unwrap_or(0.0)))
                }
            }
            _ => json!(c.get(2).map(|m| m.as_str()).unwrap_or("").trim_end_matches('}').trim()),
        };
        out.insert(k, v);
    }
    out
}

/// A tool result as the history keeps it: an action with its outcome; found text (search, pages, recall) left out.
/// The calls themselves must stay: 50 lines of «Включаю …» with no call in the history teach a model to say
/// «Включаю» without calling play_music.
pub fn brief(name: &str, res: &Value) -> Value {
    if ["web_search", "read_page", "recall"].contains(&name) {
        return json!({"ok": true, "note": "результат был, в истории опущен"});
    }
    let Value::Object(o) = res else { return json!({"ok": true}) };
    let mut out = Map::new();
    for k in ["ok", "playing", "state", "track", "error", "rings_at", "level", "result"] {
        if let Some(v) = o.get(k) {
            out.insert(k.into(), v.clone());
        }
    }
    if out.is_empty() {
        json!({"ok": true})
    } else {
        Value::Object(out)
    }
}

/// Says something: not null, false, zero or empty.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[derive(Clone)]
struct Turn {
    t: f64,
    user: String,
    answer: String,
    tools: Value,             // [{tool, args, res}] as memory keeps them
    msgs: Option<Vec<Value>>, // the exchange's full messages with tools (None: loaded from memory)
}

pub type Done = (String, Value, Value); // a tool call made: name, arguments, result

/// How the user is spoken of.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Address {
    /// not said: phrases that need no gender
    #[default]
    Neutral,
    Male,
    Female,
}

/// What the settings say about the home and the user (settings.rs `home`, `preferences`): it goes into the prompt.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    pub address: Address,
    /// the city the weather is for when none is named ("": not set)
    pub city: String,
    /// how the user wants to be talked to, in their words
    pub preferences: String,
}

/// What `ask` gives back.
#[derive(Debug, Default)]
pub struct Answer {
    pub text: String,
    pub calls: Vec<Done>,
    /// the model that wrote it ("": none did)
    pub model: String,
}

pub struct Brain {
    pub memory: Option<Arc<Memory>>,
    /// a file that replaces the built-in prompt, if it is there
    prompt: String,
    profile: std::sync::RwLock<Profile>,
    turns: Mutex<Vec<Turn>>,
    last: Mutex<f64>,
    /// device → its request in progress: an interrupted one winds down before the device's next one starts
    devices: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// the model at home: one call at a time (the warm-up, the notes, a request's round)
    slot: tokio::sync::Mutex<()>,
}

impl Brain {
    pub fn new(memory: Option<Arc<Memory>>, prompt: &str) -> Result<Brain, String> {
        let turns: Vec<Turn> = match &memory {
            Some(m) => m
                .recent(LOAD)?
                .into_iter()
                .map(|r| Turn { t: r.t, user: r.user, answer: r.answer, tools: r.tools, msgs: None })
                .collect(),
            None => vec![],
        };
        let last = turns.last().map(|t| t.t).unwrap_or(0.0);
        Ok(Brain {
            memory,
            prompt: prompt.into(),
            profile: Default::default(),
            turns: Mutex::new(turns),
            last: Mutex::new(last),
            devices: Default::default(),
            slot: tokio::sync::Mutex::new(()),
        })
    }

    pub fn turns(&self) -> usize {
        self.turns.lock().unwrap().len()
    }

    /// When the last exchange was (the note-taker waits for a quiet minute)
    pub fn last(&self) -> f64 {
        *self.last.lock().unwrap()
    }

    /// The turn of a model at home (its server's one slot); a model in the internet needs none.
    async fn slot(&self, llm: &Llm) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        if llm.cloud() {
            None
        } else {
            Some(self.slot.lock().await)
        }
    }

    /// A whole completion (the notes), in its turn with the warm-up and the requests' rounds.
    pub async fn complete(&self, llm: &Llm, body: &Value, timeout: Duration) -> Result<Value, LlmError> {
        let _slot = self.slot(llm).await;
        llm.complete(body, timeout).await
    }

    /// What the settings say now. → whether it changed (then the prompt in a model's cache is stale: prewarm).
    pub fn set_profile(&self, profile: Profile) -> bool {
        let mut p = self.profile.write().unwrap();
        if *p == profile {
            return false;
        }
        *p = profile;
        true
    }

    /// The system prompt, read every time (a prompt.md put into the data folder applies without a restart), with the
    /// year, the notes and what the settings say. `cloud`: the model is in the internet.
    pub fn system(&self, cloud: bool) -> String {
        let home = self.profile.read().unwrap();
        let address = match home.address {
            Address::Neutral => "Пол собеседника заранее не известен. Пока он сам не скажет о себе в мужском или женском роде, строй фразы без рода: не «ты спросил», а «ты спрашиваешь» или «твой вопрос».",
            Address::Male => "Хозяин колонки — мужчина: о нём говори в мужском роде («ты спросил», «ты просил»), если собеседник сам не говорит о себе в женском.",
            Address::Female => "Хозяйка колонки — женщина: о ней говори в женском роде («ты спросила», «ты просила»), если собеседник сам не говорит о себе в мужском.",
        };
        let weather = match home.city.as_str() {
            "" => "Погода — get_weather. Домашний город в настройках не указан: если город не назвали, спроси, для какого нужен прогноз.".to_string(),
            city => format!("Погода — get_weather; без города это дом, {city}."),
        };
        let facts = self.memory.as_ref().map(|m| m.facts()).unwrap_or_default();
        let notes = facts.iter().map(|f| format!("- {}", f.text)).collect::<Vec<_>>().join("\n");
        let mut p = std::fs::read_to_string(&self.prompt)
            .unwrap_or_else(|_| PROMPT.to_string())
            .replace("{where}", if cloud { WHERE_CLOUD } else { WHERE_HOME })
            .replace("{address}", address)
            .replace("{home}", &weather)
            .replace("{year}", &year().to_string())
            .replace("{facts}", if notes.is_empty() { "- пока ничего" } else { &notes });
        if !home.preferences.is_empty() {
            p.push_str("\n\nКак хозяин просит с ним общаться (написал сам, в настройках; следуй этому):\n");
            p.push_str(&home.preferences);
        }
        p
    }

    /// Notes dated yesterday, today or tomorrow: they go next to the last line, where the model looks — in the
    /// system prompt, behind 50 exchanges, «я дома» is not connected with that morning's interview.
    fn near_notes(&self, now: f64) -> Vec<String> {
        let Some(m) = &self.memory else { return vec![] };
        let days: Vec<String> = [-1.0, 0.0, 1.0]
            .iter()
            .map(|d| tz().timestamp_opt((now + d * 86400.0) as i64, 0).single().unwrap().format("%d.%m").to_string())
            .collect();
        m.facts()
            .into_iter()
            .filter(|f| dated().captures_iter(&f.text).any(|c| days.contains(&c[1].to_string())))
            .map(|f| f.text)
            .collect()
    }

    /// The last exchange goes into the prompt whole (FULL_FOR) with what a web tool found in it.
    fn web_in_prompt(&self, now: f64) -> bool {
        let turns = self.turns.lock().unwrap();
        turns.last().is_some_and(|tr| {
            tr.msgs.as_ref().is_some_and(|m| !m.is_empty())
                && now - tr.t < FULL_FOR
                && tr
                    .tools
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|x| x["tool"].as_str().is_some_and(|n| crate::tools::WEB.contains(&n)))
        })
    }

    pub async fn messages(&self, text: &str, now: f64, tools: &Tools, cloud: bool) -> Vec<Value> {
        let hist = {
            let turns = self.turns.lock().unwrap();
            let mut hist: Vec<Value> = vec![];
            let n = turns.len();
            for (k, tr) in turns.iter().enumerate() {
                if let Some(msgs) = tr.msgs.as_ref().filter(|m| !m.is_empty() && k == n - 1 && now - tr.t < FULL_FOR) {
                    hist.extend(msgs.iter().cloned());
                    continue;
                }
                hist.push(json!({"role": "user", "content": format!("[{}] {}", stamp(tr.t), tr.user)}));
                let used: Vec<&Value> =
                    tr.tools.as_array().into_iter().flatten().filter(|x| x.is_object() && truthy(&x["tool"])).collect();
                if !used.is_empty() {
                    // the calls stay in the history, with a short outcome (brief())
                    let ids: Vec<String> = (0..used.len()).map(|j| format!("h{}_{j}", tr.t as i64)).collect();
                    let calls: Vec<Value> = used
                        .iter()
                        .zip(&ids)
                        .map(|(x, id)| {
                            let args = if truthy(&x["args"]) { x["args"].to_string() } else { "{}".into() };
                            json!({"id": id, "type": "function", "function": {"name": x["tool"], "arguments": args}})
                        })
                        .collect();
                    hist.push(json!({"role": "assistant", "content": null, "tool_calls": calls}));
                    for (x, id) in used.iter().zip(&ids) {
                        let res = if truthy(&x["res"]) { x["res"].to_string() } else { r#"{"ok":true}"#.into() };
                        hist.push(json!({"role": "tool", "tool_call_id": id, "content": res}));
                    }
                }
                hist.push(json!({"role": "assistant", "content": if tr.answer.is_empty() { "…" } else { tr.answer.as_str() }}));
            }
            hist
        };
        let near = self.near_notes(now);
        let live = tools.context(text).await; // read now: the volume when the request is about it
        let mut head = stamp(now);
        for x in &live {
            head.push_str(&format!("; {x}"));
        }
        if !near.is_empty() {
            head.push_str(&format!("; в заметках на эти дни: {}", near.join(" ")));
        }
        let mut msgs = vec![json!({"role": "system", "content": self.system(cloud)})];
        msgs.extend(hist);
        msgs.push(json!({"role": "user", "content": format!("[{head}] {text}")}));
        msgs
    }

    fn keep(&self, t: f64, text: &str, answer: &str, msgs: Option<Vec<Value>>, calls: &[Done]) {
        let tools: Value = calls.iter().map(|(n, a, r)| json!({"tool": n, "args": a, "res": brief(n, r)})).collect();
        {
            let mut turns = self.turns.lock().unwrap();
            // in the order it was said: another device's request, begun later, may have finished first (the end is
            // where it goes almost always: the prompt's cached prefix is kept)
            let at = turns.iter().rposition(|x| x.t <= t).map_or(0, |i| i + 1);
            turns.insert(at, Turn { t, user: text.into(), answer: answer.into(), tools: tools.clone(), msgs });
            if turns.len() > KEEP_MAX {
                let cut = turns.len() - KEEP_MIN;
                turns.drain(..cut);
            }
        }
        *self.last.lock().unwrap() = now_s();
        if let Some(m) = &self.memory {
            if let Err(e) = m.add(text, answer, &tools, t) {
                log("memory", e);
            }
        }
    }

    /// An exchange handled without the model (a plain player command): into memory like any other, with its call.
    pub fn remember(&self, text: &str, answer: &str, calls: &[Done]) {
        self.keep(now_s(), text, answer, None, calls);
    }

    /// Lets a model at home read the current prompt (system, notes, memory) into its cache while nobody waits: after
    /// a start, a notes update. → seconds; None: a model in the internet (it keeps no cache of ours), or no answer.
    pub async fn prewarm(&self, llm: &Llm) -> Option<f64> {
        if llm.cloud() {
            return None;
        }
        let tools = Tools::new(crate::tools::Io::default(), None);
        let msgs = self.messages("…", now_s(), &tools, false).await;
        let body = json!({"messages": msgs, "tools": schemas(), "temperature": 0.3, "max_tokens": 1, "chat_template_kwargs": {"enable_thinking": false}});
        let t = now_s();
        self.complete(llm, &body, Duration::from_secs(180)).await.ok().map(|_| now_s() - t)
    }

    /// `llm` answers `text`; every finished sentence goes to on_sentence() while the model is still writing the rest.
    /// cancelled(): the user interrupted — stop writing, run no more tools, forget this exchange.
    /// on_tool(name, args): a tool is about to run (the session says «ищу в интернете» and shows the search).
    /// now: when it is said (default: now). dev: the device (one request of it at a time) and the log's name.
    #[allow(clippy::too_many_arguments)]
    pub async fn ask(
        &self,
        llm: &Llm,
        text: &str,
        tools: &mut Tools,
        on_sentence: &mut (dyn FnMut(String) + Send),
        cancelled: &(dyn Fn() -> bool + Send + Sync),
        on_tool: &mut (dyn FnMut(&str, &Value) + Send),
        now: Option<f64>,
        dev: &str,
    ) -> Result<Answer, String> {
        let device = self.devices.lock().unwrap().entry(dev.to_string()).or_default().clone();
        let _device = device.lock_owned().await;
        if cancelled() {
            return Ok(Answer::default());
        }
        let mut model = String::new();
        let now = now.unwrap_or_else(now_s);
        let mut msgs = self.messages(text, now, tools, llm.cloud()).await;
        let mut new = vec![msgs.last().unwrap().clone()];
        let (mut calls_done, mut said): (Vec<Done>, Vec<String>) = (vec![], vec![]);
        let (mut retried, mut nudged, mut checked, mut force) = (0, false, false, false);
        let fact_q = FACT_GUARD && fact_q().is_match(text) && !not_fact().is_match(text);
        // a volume change asked for: whatever is said first, a set_volume call must come with it — the CLAIM words do
        // not cover every way to say «done» («Сделала три»)
        let vol_change = CLAIM_GUARD && volume_change(text);
        if FACT_GUARD && challenge().is_match(text) && !not_fact().is_match(text) {
            (checked, force) = (true, true); // caught out: a search (or the right tool) before any answer
            log(dev, "   (the user objects: a check first)");
        }
        let mut server_errors = 0;
        let wait = if llm.cloud() { CLOUD_WAIT } else { LOCAL_WAIT };
        tools.new_request(text);
        if self.web_in_prompt(now) {
            tools.web_in_prompt();
        }
        for i in 0..ROUNDS {
            let mut held: Vec<String> = vec![];
            let mut cut = Sentences::new();
            let mut body = json!({"messages": msgs, "tools": schemas(), "temperature": 0.3 + 0.3 * (retried > 1) as i32 as f64, "max_tokens": 1500,
                                  "chat_template_kwargs": {"enable_thinking": false}});
            // the last round must answer in words; after a claim with no call (told so) — a call it must be
            if i == ROUNDS - 1 {
                body["tool_choice"] = json!("none");
            } else if force {
                body["tool_choice"] = json!("required");
            }
            let no_calls = calls_done.is_empty();
            // a model at home: its turn first (the warm-up, the notes, another device's round); given up when interrupted
            let slot = if llm.cloud() {
                None
            } else if let Ok(g) = self.slot.try_lock() {
                Some(g)
            } else {
                log(dev, "   (the model at home is busy: waiting for its turn)");
                tokio::select! {
                    g = self.slot.lock() => Some(g),
                    _ = async { while !cancelled() { tokio::time::sleep(Duration::from_millis(50)).await } } => {
                        return Ok(Answer { text: said.join(" "), calls: calls_done, model });
                    }
                }
            };
            let mut emit = |s: String, held: &mut Vec<String>, said: &mut Vec<String>| {
                let s = markup().replace_all(&s, " ").trim().to_string(); // a tool call the server did not recognise, as text
                if !word().is_match(&s) {
                    return;
                }
                if !held.is_empty()
                    || (no_calls
                        && said.is_empty()
                        && ((CLAIM_GUARD && !nudged && (claim().is_match(&s) || vol_change)) || (fact_q && !checked)))
                {
                    held.push(s); // «Включаю.» / a factual answer — not yet: is a tool called, are there numbers?
                } else {
                    said.push(s.clone());
                    on_sentence(s);
                }
            };
            let mut out = vec![];
            let r = llm
                .stream(
                    &body,
                    wait,
                    |t| {
                        cut.feed(t, &mut out);
                        for s in out.drain(..) {
                            emit(s, &mut held, &mut said);
                        }
                    },
                    cancelled,
                )
                .await;
            drop(slot); // the tools run with the model free
            let (content, mut calls): (String, Vec<Call>) = match r {
                Ok(x) => {
                    model = llm.model.clone();
                    x
                }
                Err(e) => {
                    let LlmError::Http(code, _) = e else { return Err(e.to_string()) };
                    if force {
                        force = false; // the server could not parse the forced call (HTTP 500): once more, not forced
                        log(dev, "   (the model's server did not parse the forced call: again, not forced)");
                        continue;
                    }
                    if code >= 500 && server_errors < 2 && said.is_empty() {
                        // a local server now and then answers 500 (a tool call it cannot parse), a proxy before it 502
                        server_errors += 1;
                        log(dev, format!("   (the model's server answered {code}: again)"));
                        tokio::time::sleep(Duration::from_millis(300 * server_errors)).await;
                        continue;
                    }
                    return Err(e.to_string());
                }
            };
            force = false; // one forced round: after its tool result the model answers in words
            if cancelled() {
                return Ok(Answer { text: said.join(" "), calls: calls_done, model });
            }
            cut.flush(&mut out);
            for s in out.drain(..) {
                emit(s, &mut held, &mut said);
            }
            if content.trim().is_empty() && calls.is_empty() && retried < 2 {
                // now and then, after a tool result, a model emits a stray «<tool_call|>» and stops: asked again; the
                // second retry samples a bit warmer (the same draw repeats)
                retried += 1;
                continue;
            }
            if !held.is_empty()
                && calls.is_empty()
                && !nudged
                && CLAIM_GUARD
                && (claim().is_match(&held[0]) || vol_change)
            {
                // said done, called nothing: once more, told so (neither goes to the history). Told alone, a model
                // apologises and still calls nothing half the time — so a call is required, when the user's words
                // are a command at all (their «пауза», not the model's own «пауза» in an answer)
                let command = ["play_music", "player_control", "set_volume", "set_timer"]
                    .iter()
                    .any(|k| action_re(k).is_some_and(|r| r.is_match(text)));
                (nudged, force) = (true, command);
                log(
                    dev,
                    format!(
                        "   (said done with no tool call: asked again{})",
                        if command { ", a call required" } else { "" }
                    ),
                );
                msgs.push(json!({"role": "assistant", "content": content.trim()}));
                msgs.push(json!({"role": "user", "content": NUDGE}));
                continue;
            }
            if !held.is_empty() && calls.is_empty() && fact_q && !checked && num_unit().is_match(&content) {
                (checked, force) = (true, true); // numbers from the model's own head: a search first (the attempt is not said)
                log(dev, "   (numbers with no check: a search first)");
                msgs.push(json!({"role": "assistant", "content": content.trim()}));
                msgs.push(json!({"role": "user", "content": FACT_NUDGE}));
                continue;
            }
            for s in held.drain(..) {
                // a tool is called after all (or the model insists): said
                said.push(s.clone());
                on_sentence(s);
            }
            for c in calls.iter_mut() {
                // broken arguments must not go back to the server: it cannot parse them (500)
                c.arguments = Value::Object(tool_args(&c.arguments)).to_string();
            }
            let mut msg = json!({"role": "assistant", "content": if content.trim().is_empty() { Value::Null } else { json!(content.trim()) }});
            if !calls.is_empty() {
                msg["tool_calls"] = calls.iter().map(Call::to_json).collect();
            }
            msgs.push(msg.clone());
            new.push(msg);
            if calls.is_empty() {
                break;
            }
            for c in &calls {
                let args: Value = serde_json::from_str(&c.arguments).unwrap_or(json!({}));
                on_tool(&c.name, &args);
                let res = tokio::select! {
                    r = tools.run(&c.name, &args) => r,
                    _ = async { while !cancelled() { tokio::time::sleep(Duration::from_millis(50)).await } } => {
                        return Ok(Answer { text: said.join(" "), calls: calls_done, model });
                    }
                };
                let tm = json!({"role": "tool", "tool_call_id": if c.id.is_empty() { "call" } else { c.id.as_str() }, "content": res.to_string()});
                calls_done.push((c.name.clone(), args, res));
                msgs.push(tm.clone());
                new.push(tm);
            }
        }
        let answer = said.join(" ");
        if answer.is_empty() && calls_done.is_empty() {
            // no words after the retries, nothing done: a failed request — the session says so, nothing is kept
            return Err(format!("{} gave no words", if model.is_empty() { "the model" } else { model.as_str() }));
        }
        self.keep(now, text, &answer, Some(new), &calls_done);
        Ok(Answer { text: answer, calls: calls_done, model })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_guards_catch() {
        assert!(claim().is_match("Включаю Монеточку."));
        assert!(claim().is_match("Пауза."));
        assert!(!claim().is_match("Пауза в работе была долгой."));
        assert!(claim().is_match("— Сделала потише."));
        assert!(!claim().is_match("Я включаю свет только вечером."), "at the start only");
        assert!(fact_q().is_match("Сколько весит слон?") && !not_fact().is_match("Сколько весит слон?"));
        assert!(not_fact().is_match("Какая громкость?"));
        assert!(num_unit().is_match("Крокодилы бегают до 35 км/ч."));
        assert!(num_unit().is_match("от 11 до 17 километров"));
        assert!(!num_unit().is_match("Он родился давно."));
        assert!(challenge().is_match("Ты же сказала, что они плывут быстрее"));
        assert!(challenge().is_match("это не так"));
    }

    #[test]
    fn markup_is_never_said() {
        let s = "<|tool_call>call:calculate{expression:<|\"|>(15*60+40)<|\"|>}<tool_call|> Готово.";
        assert_eq!(markup().replace_all(s, " ").trim(), "Готово.");
        assert_eq!(markup().replace_all("<think>хм</think>Да.", " ").trim(), "хм Да.");
    }

    #[test]
    fn broken_arguments_saved() {
        let a = tool_args(r#"{"expression":"(15*60+40) - (50 + 10)}<tool_call|>><|channel>thought…"#);
        assert_eq!(Value::Object(a), json!({"expression": "(15*60+40) - (50 + 10)"}));
        assert_eq!(
            Value::Object(tool_args(r#"{"seconds": 600, "label": "чай"}"#)),
            json!({"seconds": 600, "label": "чай"})
        );
        assert_eq!(Value::Object(tool_args(r#"{"level": 7, "x": 1.5"#)), json!({"level": 7, "x": 1.5}));
        assert!(tool_args("").is_empty());
        assert!(tool_args("[1]").is_empty());
    }

    #[test]
    fn brief_keeps_outcomes() {
        assert_eq!(
            brief("web_search", &json!({"results": []})),
            json!({"ok": true, "note": "результат был, в истории опущен"})
        );
        assert_eq!(
            brief("set_volume", &json!({"level": 4, "ok": true, "x": 1})).to_string(),
            r#"{"ok":true,"level":4}"#,
            "in this order"
        );
        assert_eq!(brief("calculate", &json!({"result": 84})), json!({"result": 84}));
        assert_eq!(brief("date_calc", &json!({"today": "x"})), json!({"ok": true}));
    }

    // ---- who waits for whom, on a model at 127.0.0.1 (llm::fake): «slow» — a second, «fail» — the cloud breaks off,
    // «silent» — no words; a whole completion (the warm-up) takes what the test says

    use crate::llm::fake;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    const NO_PROMPT: &str = "/nonexistent/prompt.md"; // the built-in one

    #[test]
    fn the_prompt_says_what_the_settings_say() {
        let brain = Brain::new(None, NO_PROMPT).unwrap();
        let p = brain.system(false);
        assert!(
            p.contains(WHERE_HOME)
                && p.contains("строй фразы без рода")
                && p.contains("Домашний город в настройках не указан")
                && p.contains("- пока ничего")
        );
        assert!(!p.contains('{'), "every placeholder is filled");
        assert!(brain.set_profile(Profile {
            address: Address::Female,
            city: "Казань".into(),
            preferences: "Зови меня Котик.".into()
        }));
        assert!(
            !brain.set_profile(Profile {
                address: Address::Female,
                city: "Казань".into(),
                preferences: "Зови меня Котик.".into()
            }),
            "the same: no change"
        );
        let p = brain.system(true);
        assert!(
            p.contains(WHERE_CLOUD)
                && p.contains("Хозяйка колонки")
                && p.contains("без города это дом, Казань.")
                && p.ends_with("Зови меня Котик.")
        );
    }

    /// a model in the internet
    fn cloud(m: &fake::Model) -> Llm {
        Llm::new(&m.url, "cloud", "", Map::new()).into_cloud()
    }

    /// a model at home
    fn home(m: &fake::Model, model: &str) -> Llm {
        Llm::new(&m.url, model, "", Map::new())
    }

    /// One request as a device makes it → (the answer, when it came back).
    async fn ask(
        brain: &Brain,
        llm: &Llm,
        dev: &str,
        text: &str,
        now: Option<f64>,
        stop: &AtomicBool,
    ) -> (Result<Answer, String>, Instant) {
        let mut tools = Tools::new(crate::tools::Io::default(), None);
        let r = brain
            .ask(llm, text, &mut tools, &mut |_| {}, &|| stop.load(Ordering::SeqCst), &mut |_, _| {}, now, dev)
            .await;
        (r, Instant::now())
    }

    async fn sleep(ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn devices_answer_side_by_side() {
        let m = fake::Model::start(0).await;
        let llm = cloud(&m);
        let brain = Brain::new(None, NO_PROMPT).unwrap();
        let no = AtomicBool::new(false);
        // the kitchen asks first and slowly; the hall a moment later — and is not kept waiting
        let ((a, ta), (b, tb)) = tokio::join!(ask(&brain, &llm, "kitchen", "slow first", Some(1000.0), &no), async {
            sleep(100).await;
            ask(&brain, &llm, "hall", "second", Some(1001.0), &no).await
        });
        assert!(tb + Duration::from_millis(500) < ta, "the hall answered while the kitchen's model was still writing");
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_eq!(
            (a.text.as_str(), a.model.as_str(), b.text.as_str(), b.model.as_str()),
            ("Ответ cloud.", "cloud", "Ответ cloud.", "cloud")
        );
        // the hall's exchange was kept first, the kitchen's goes before it: in the order they were said
        let order: Vec<(f64, String)> = brain.turns.lock().unwrap().iter().map(|x| (x.t, x.user.clone())).collect();
        assert_eq!(order, [(1000.0, "slow first".to_string()), (1001.0, "second".to_string())]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_interrupted_request_winds_down_before_its_device_asks_again() {
        let m = fake::Model::start(0).await;
        let llm = cloud(&m);
        let brain = Brain::new(None, NO_PROMPT).unwrap();
        let (stop, no) = (AtomicBool::new(false), AtomicBool::new(false));
        let ((a, ta), (b, _), (c, _), _) = tokio::join!(
            ask(&brain, &llm, "kitchen", "slow first", None, &stop),
            async {
                sleep(100).await;
                ask(&brain, &llm, "kitchen", "again", None, &no).await
            },
            async {
                sleep(100).await;
                ask(&brain, &llm, "hall", "meanwhile", None, &no).await
            },
            async {
                sleep(300).await;
                stop.store(true, Ordering::SeqCst)
            },
        );
        let a = a.unwrap();
        assert_eq!((a.text.as_str(), a.calls.len()), ("", 0), "interrupted before a word");
        let seen = m.seen();
        let at = |w: &str| seen.iter().find(|s| s.text.contains(w)).unwrap().at;
        assert!(at("again") >= ta, "the kitchen's next request went once the interrupted one was over");
        assert!(at("meanwhile") < ta, "another device did not wait for it");
        assert_eq!((b.unwrap().text, c.unwrap().text), ("Ответ cloud.".to_string(), "Ответ cloud.".to_string()));
        assert_eq!(brain.turns(), 2, "the interrupted one is forgotten");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_model_at_home_takes_one_call_at_a_time() {
        let m = fake::Model::start(1500).await;
        let brain = Arc::new(Brain::new(None, NO_PROMPT).unwrap());
        let no = AtomicBool::new(false);
        assert!(
            brain.prewarm(&cloud(&m)).await.is_none() && m.seen().is_empty(),
            "a model in the internet is not warmed"
        );
        let (b, local) = (brain.clone(), home(&m, "local"));
        let t0 = Instant::now();
        let warm = tokio::spawn(async move { b.prewarm(&local).await });
        sleep(100).await;
        // a model in the internet answers during the warm-up of the one at home
        let (a, ta) = ask(&brain, &cloud(&m), "station-1", "привет", None, &no).await;
        assert!(ta - t0 < Duration::from_millis(1000), "answered by the cloud during the warm-up: {:?}", ta - t0);
        assert_eq!(a.unwrap().model, "cloud");
        // a request to the model at home: its turn after the warm-up
        let (q, _) = ask(&brain, &home(&m, "local"), "kitchen", "привет", None, &no).await;
        assert_eq!(q.unwrap().model, "local");
        assert!(warm.await.unwrap().is_some());
        let seen = m.seen();
        let w = seen.iter().find(|s| !s.stream).unwrap();
        let q = seen.iter().find(|s| s.stream && s.model == "local").unwrap();
        assert!(q.at >= w.at + Duration::from_millis(1400), "the model at home: one call at a time");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn waiting_for_the_model_at_home_ends_when_interrupted() {
        let m = fake::Model::start(1500).await;
        let llm = home(&m, "local");
        let brain = Arc::new(Brain::new(None, NO_PROMPT).unwrap());
        let (b, l) = (brain.clone(), llm.clone());
        let warm = tokio::spawn(async move { b.prewarm(&l).await });
        sleep(100).await;
        let stop = AtomicBool::new(false);
        let t0 = Instant::now();
        let ((a, ta), _) = tokio::join!(ask(&brain, &llm, "kitchen", "привет", None, &stop), async {
            sleep(200).await;
            stop.store(true, Ordering::SeqCst)
        });
        assert!(ta - t0 < Duration::from_millis(700), "gave up waiting: {:?}", ta - t0);
        assert_eq!(a.unwrap().text, "");
        assert!(m.seen().iter().all(|s| !s.stream), "it never reached the model");
        assert!(warm.await.unwrap().is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_answer_is_an_error_and_nothing_is_kept() {
        let m = fake::Model::start(0).await;
        let brain = Brain::new(None, NO_PROMPT).unwrap();
        let no = AtomicBool::new(false);
        // OpenRouter's error event midway: an error (the session says «Что-то я задумалась…»), no other model steps in
        let (e, _) = ask(&brain, &cloud(&m), "station-1", "fail", None, &no).await;
        assert!(e.err().is_some_and(|e| e.contains("an error in the stream")));
        assert_eq!(m.seen().iter().map(|s| s.model.as_str()).collect::<Vec<_>>(), ["cloud"]);
        // no words after the retries: an error too
        let (e, _) = ask(&brain, &home(&m, "qwen"), "kitchen", "silent", None, &no).await;
        assert_eq!(e.err().as_deref(), Some("qwen gave no words"));
        assert_eq!(m.seen().iter().filter(|s| s.model == "qwen").count(), 3, "asked again twice");
        assert_eq!(brain.turns(), 0, "nothing kept");
    }
}
