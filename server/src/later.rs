//! Commands put off: «включи чайник через пять минут», «выключи музыку через полчаса», «в семь утра поставь джаз». The
//! model's tool `later` (tools/) keeps what another tool would do now — home_set, play_music, player_control,
//! set_volume — with the time it is due; then it is run here as that tool runs it, silently: nothing is said or
//! sounded.
//! - Kept in server.db with the time each is due: a restart loses none. One that came due while the server was down —
//!   or is more than LATE s late for any other reason — is not run: a kettle must not boil an hour after it was asked to.
//! - What one request puts off for one device of the home at one time is one thing of several steps (a kettle: the
//!   target, then the mode), run in order; a step that fails ends it.
//! - It fails as the tool does — Home Assistant away, the device away, the station away — and nobody is there to hear
//!   of it: the log and the web UI say so.
//! - Nothing is ever deleted: what was run, what failed and why, what was missed, what was cancelled stay as the
//!   record of it (the web UI's history). The main page shows what waits and what ended within SHOWN s, unless it
//!   was put out of sight there.
//! - What is pending on a device goes into the stamp of its requests' last line (tools/): the model says «через
//!   сколько» by it and cancels by its number (the tool `later_cancel`); the web UI cancels too.
//! - The main page's list changed — an event (`later`) for the web UI.
use crate::events;
use crate::session::{log, Shared};
use crate::store::{sql, Store};
use crate::text::{now_s, tz};
use crate::timers::span;
use crate::tools::{Io, Tools};
use chrono::TimeZone;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

/// this late it is still run (a restart takes seconds); later than that it is missed
pub const LATE: f64 = 30.0;
/// how long one that ended stays on the main page
const SHOWN: f64 = 86400.0;
/// waiting on one device at once
const WAITING_MAX: usize = 20;
/// the furthest ahead a thing is put off
pub const AHEAD_MAX: u64 = 24 * 3600;
/// the clocks are looked at this often while one waits: a machine that slept wakes to the right time
const LOOK: f64 = 30.0;

const MISSED: &str = "сервер был недоступен в это время";
const NO_STATION: &str = "колонка не в сети";
const COLUMNS: &str = "id, device, made, at, target, name, steps, state, error, ended";

/// One call of a tool, and what it is in words: «выключить музыку»; for a control of the home — the control («Нагрев:
/// режим») and what it is set to («Кипячение») apart: the model's line and the web UI each join them their own way.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Step {
    pub tool: String,
    pub args: Value,
    pub what: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
}

impl Step {
    /// In one line: «Нагрев: режим: Кипячение», «выключить музыку»
    fn words(&self) -> String {
        if self.value.is_empty() {
            self.what.clone()
        } else {
            format!("{}: {}", self.what, self.value)
        }
    }
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Wait,
    Done,
    Failed,
    /// not run: it was due while the server was down
    Missed,
    /// by the voice or in the web UI, while it waited
    Cancelled,
}

impl State {
    /// As server.db keeps it and the web UI is told.
    fn word(self) -> &'static str {
        match self {
            State::Wait => "wait",
            State::Done => "done",
            State::Failed => "failed",
            State::Missed => "missed",
            State::Cancelled => "cancelled",
        }
    }

    fn of(word: &str) -> State {
        [State::Wait, State::Done, State::Missed, State::Cancelled]
            .into_iter()
            .find(|s| s.word() == word)
            .unwrap_or(State::Failed)
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub id: u64,
    /// the station it was asked on: its music and its volume; its requests are told of it
    pub device: String,
    pub made: f64,
    pub at: f64,
    /// the device of the home it is for, and its name; "": the station itself
    pub target: String,
    pub name: String,
    pub steps: Vec<Step>,
    pub state: State,
    /// why it failed or was missed
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended: Option<f64>,
}

fn entry(r: &rusqlite::Row) -> rusqlite::Result<Entry> {
    Ok(Entry {
        id: r.get::<_, i64>(0)? as u64,
        device: r.get(1)?,
        made: r.get(2)?,
        at: r.get(3)?,
        target: r.get(4)?,
        name: r.get(5)?,
        steps: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
        state: State::of(&r.get::<_, String>(7)?),
        error: r.get(8)?,
        ended: r.get(9)?,
    })
}

impl Entry {
    /// In one line, as the model is told: «Чайник: Целевая температура: 100 °C, Режим: Кипячение», «выключить музыку»
    pub fn what(&self) -> String {
        let steps = self.steps.iter().map(Step::words).collect::<Vec<_>>().join(", ");
        if self.name.is_empty() {
            steps
        } else {
            format!("{}: {steps}", self.name)
        }
    }

    /// For the stamp: «№3 в 10:35 (через 20 мин) — выключить музыку». To the minute, while minutes are left: the
    /// seconds are nothing to say.
    pub fn describe(&self, now: f64) -> String {
        let left = (self.at - now).max(0.0);
        let left = if left >= 90.0 { (left / 60.0).round() * 60.0 } else { left.round() };
        format!("№{} {} (через {}) — {}", self.id, when(self.at, now), span(left as u64), self.what())
    }
}

/// «в 10:35», «завтра в 07:30»
pub fn when(at: f64, now: f64) -> String {
    let day = |t: f64| tz().timestamp_opt(t as i64, 0).single().unwrap();
    let (d, n) = (day(at), day(now));
    format!("{}в {}", if d.date_naive() == n.date_naive() { "" } else { "завтра " }, d.format("%H:%M"))
}

/// «через 20 минут», «через 1 час 30 минут»: as it is said.
pub fn ahead(seconds: u64) -> String {
    let word = |n: u64, forms: [&'static str; 3]| {
        let (a, b) = (n % 100, n % 10);
        forms[if (11..20).contains(&a) || b == 0 || b > 4 {
            2
        } else if b == 1 {
            0
        } else {
            1
        }]
    };
    let (h, m, s) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    let mut out = vec![];
    if h > 0 {
        out.push(format!("{h} {}", word(h, ["час", "часа", "часов"])));
    }
    if m > 0 {
        out.push(format!("{m} {}", word(m, ["минуту", "минуты", "минут"])));
    }
    if s > 0 || out.is_empty() {
        out.push(format!("{s} {}", word(s, ["секунду", "секунды", "секунд"])));
    }
    format!("через {}", out.join(" "))
}

pub struct Later {
    store: Arc<Store>,
}

impl Later {
    /// What is put off, in `store` (server.db: the table `later`).
    pub fn new(store: Arc<Store>) -> Arc<Later> {
        Arc::new(Later { store })
    }

    /// In a server.db of its own, in memory.
    #[cfg(test)]
    pub fn in_memory() -> Arc<Later> {
        Later::new(Store::open(":memory:").unwrap())
    }

    /// The rows `cond` takes (none, and a line in the log, if server.db fails).
    fn select(&self, cond: &str, p: impl rusqlite::Params) -> Vec<Entry> {
        let db = self.store.db.lock().unwrap();
        let rows = db.prepare(&format!("SELECT {COLUMNS} FROM later WHERE {cond}")).and_then(|mut st| {
            let rows = st.query_map(p, entry)?.collect::<rusqlite::Result<Vec<Entry>>>();
            rows
        });
        rows.unwrap_or_else(|e| {
            log("server", format!("later: {}", sql(e)));
            vec![]
        })
    }

    /// A change. → whether it changed any row: the main page is told then.
    fn change(&self, statement: &str, p: impl rusqlite::Params) -> bool {
        let done = self.store.db.lock().unwrap().execute(statement, p);
        let changed = match done {
            Ok(n) => n > 0,
            Err(e) => {
                log("server", format!("later: {}", sql(e)));
                false
            }
        };
        if changed {
            events::emit("later", self.json());
        }
        changed
    }

    /// Everything that waits (the server's start: each is scheduled again).
    pub fn all_waiting(&self) -> Vec<Entry> {
        self.select("state = 'wait' ORDER BY at, id", [])
    }

    /// What waits on a device, soonest first.
    pub fn pending(&self, device: &str) -> Vec<Entry> {
        self.select("device = ?1 AND state = 'wait' ORDER BY at, id", [device])
    }

    /// One that still waits (not cancelled, not run).
    pub fn waiting(&self, id: u64) -> Option<Entry> {
        self.select("id = ?1 AND state = 'wait'", [id as i64]).pop()
    }

    /// Whatever has become of it.
    pub fn get(&self, id: u64) -> Option<Entry> {
        self.select("id = ?1", [id as i64]).pop()
    }

    /// A new one, of one step, for `target` (a device of the home and its name; ("", ""): the station itself). Err:
    /// too many wait on the device already.
    pub fn add(&self, device: &str, at: f64, target: (&str, &str), step: Step) -> Result<Entry, String> {
        let id = {
            let db = self.store.db.lock().unwrap();
            let waiting: i64 = db
                .query_row("SELECT COUNT(*) FROM later WHERE device = ?1 AND state = 'wait'", [device], |r| r.get(0))
                .map_err(sql)?;
            if waiting as usize >= WAITING_MAX {
                return Err(format!("отложено уже {WAITING_MAX}: больше нельзя, пока что-то из этого не выполнится"));
            }
            db.execute(
                "INSERT INTO later (device, made, at, target, name, steps, state) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'wait')",
                params![device, now_s(), at, target.0, target.1, json!([step]).to_string()],
            )
            .map_err(sql)?;
            db.last_insert_rowid() as u64
        };
        events::emit("later", self.json());
        self.get(id).ok_or_else(|| "server.db: the thing put off is not there".into())
    }

    /// One more step for one that waits. → whether it did wait.
    pub fn push(&self, id: u64, step: Step) -> bool {
        let Some(mut e) = self.waiting(id) else { return false };
        e.steps.push(step);
        self.change(
            "UPDATE later SET steps = ?2 WHERE id = ?1 AND state = 'wait'",
            params![id as i64, json!(e.steps).to_string()],
        )
    }

    /// A waiting one cancelled — `device`: only if it is that station's (the voice cancels its own). → what it was.
    /// It stays in the record, as cancelled; the main page shows it no more.
    pub fn cancel(&self, id: u64, device: Option<&str>) -> Option<Entry> {
        let was = self.waiting(id)?;
        self.change(
            "UPDATE later SET state = 'cancelled', ended = ?2, hidden = 1
             WHERE id = ?1 AND state = 'wait' AND (?3 IS NULL OR device = ?3)",
            params![id as i64, now_s(), device],
        )
        .then_some(was)
    }

    /// One that ended, out of the main page's sight (the record keeps it). → whether it was there to see.
    pub fn hide(&self, id: u64) -> bool {
        self.change("UPDATE later SET hidden = 1 WHERE id = ?1 AND state != 'wait' AND hidden = 0", [id as i64])
    }

    /// How a waiting one ended.
    fn finish(&self, id: u64, state: State, error: &str) {
        self.change(
            "UPDATE later SET state = ?2, error = ?3, ended = ?4 WHERE id = ?1 AND state = 'wait'",
            params![id as i64, state.word(), error, now_s()],
        );
    }

    fn shown(e: &Entry) -> Value {
        let mut j = json!(e);
        j["what"] = json!(e.what());
        j
    }

    /// For the main page: what waits, soonest first, then what ended within SHOWN s and was not put out of sight,
    /// the latest first. Each with what it is, in words.
    pub fn json(&self) -> Value {
        let mut v = self.select("state = 'wait' OR (hidden = 0 AND ended > ?1)", [now_s() - SHOWN]);
        v.sort_by(|a, b| match (a.state == State::Wait, b.state == State::Wait) {
            (true, true) => a.at.total_cmp(&b.at).then(a.id.cmp(&b.id)),
            (false, false) => b.ended.unwrap_or(0.0).total_cmp(&a.ended.unwrap_or(0.0)),
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
        });
        json!(v.iter().map(Later::shown).collect::<Vec<_>>())
    }

    /// The record, by when each was due, the latest first (what still waits is on top): up to `limit` after the one
    /// `before` (None — from the top).
    pub fn history(&self, before: Option<u64>, limit: usize) -> Vec<Value> {
        let from = before.and_then(|id| self.get(id)).map(|e| (e.at, e.id as i64));
        let rows = match from {
            Some((at, id)) => self.select(
                "at < ?1 OR (at = ?1 AND id < ?2) ORDER BY at DESC, id DESC LIMIT ?3",
                params![at, id, limit as i64],
            ),
            None if before.is_some() => vec![],
            None => self.select("1 ORDER BY at DESC, id DESC LIMIT ?1", [limit as i64]),
        };
        rows.iter().map(Later::shown).collect()
    }
}

/// Runs `id` when it is due.
pub fn schedule(shared: Arc<Shared>, id: u64, at: f64) {
    tokio::spawn(async move {
        loop {
            let wait = at - now_s();
            if wait <= 0.0 {
                break;
            }
            tokio::time::sleep(Duration::from_secs_f64(wait.min(LOOK))).await;
        }
        run(&shared, id).await;
    });
}

/// At the server's start: what waits is scheduled again (what came due meanwhile is marked missed at once).
pub fn resume(shared: &Arc<Shared>) {
    for e in shared.later.all_waiting() {
        schedule(shared.clone(), e.id, e.at);
    }
}

async fn run(shared: &Arc<Shared>, id: u64) {
    let Some(e) = shared.later.waiting(id) else { return }; // cancelled meanwhile
    let station = shared.stations.lock().unwrap().get(&e.device).cloned();
    let mut tools = Tools::new(
        Io {
            music: station.as_ref().and_then(|_| shared.player(&e.device)),
            station,
            house: Some(shared.house.clone()),
            ..Io::default()
        },
        None,
    );
    perform(&shared.later, &e, &mut tools).await;
}

/// A waiting one, run with `tools` (its station's; of a station that is away — with none): missed if it is late, else
/// its steps in order, the first that fails ending it.
pub(crate) async fn perform(later: &Later, e: &Entry, tools: &mut Tools) {
    let late = now_s() - e.at;
    if late > LATE {
        log(&e.device, format!("   (later №{}: not run, {late:.0} s late: the server was down)", e.id));
        later.finish(e.id, State::Missed, MISSED);
        return;
    }
    for s in &e.steps {
        if let Err(why) = step(tools, s).await {
            log(&e.device, format!("   (later №{}: {} failed: {why})", e.id, s.tool));
            let why = if e.steps.len() > 1 { format!("{why} ({})", s.what) } else { why };
            later.finish(e.id, State::Failed, &why);
            return;
        }
    }
    log(&e.device, format!("   (later №{}: done)", e.id));
    later.finish(e.id, State::Done, "");
}

/// One step, as its tool runs it. Err: why not, as the web UI shows it.
async fn step(tools: &mut Tools, s: &Step) -> Result<(), String> {
    if s.tool != "home_set" && tools.io.station.is_none() {
        return Err(NO_STATION.into());
    }
    let r = tools.run(&s.tool, &s.args).await;
    if r["ok"] != json!(false) {
        return Ok(());
    }
    let why = r["error"].as_str().unwrap_or("не вышло");
    // nothing plays: there is nothing to stop — which is what was asked for
    let quiet = s.tool == "player_control" && matches!(s.args["action"].as_str(), Some("stop" | "pause"));
    if quiet && why == crate::music::NOTHING {
        return Ok(());
    }
    Err(why.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::msk_t;

    fn step(what: &str) -> Step {
        Step { tool: "player_control".into(), args: json!({"action": "stop"}), what: what.into(), value: String::new() }
    }

    /// A control of the home set to a value.
    fn set(what: &str, value: &str) -> Step {
        Step { value: value.into(), ..step(what) }
    }

    #[test]
    fn in_words() {
        let now = msk_t("2026-11-23 10:15");
        assert_eq!(when(msk_t("2026-11-23 10:35"), now), "в 10:35");
        assert_eq!(when(msk_t("2026-11-24 07:30"), now), "завтра в 07:30");
        for (s, want) in [
            (1200, "через 20 минут"),
            (60, "через 1 минуту"),
            (5400, "через 1 час 30 минут"),
            (7200, "через 2 часа"),
            (45, "через 45 секунд"),
            (0, "через 0 секунд"),
            (660, "через 11 минут"),
            (1320, "через 22 минуты"),
        ] {
            assert_eq!(ahead(s), want);
        }
        let mut e = Entry {
            id: 3,
            device: "d".into(),
            made: now,
            at: now + 1200.0,
            target: String::new(),
            name: String::new(),
            steps: vec![step("выключить музыку")],
            state: State::Wait,
            error: String::new(),
            ended: None,
        };
        assert_eq!(e.describe(now), "№3 в 10:35 (через 20 мин) — выключить музыку");
        assert_eq!(e.describe(now + 322.0), "№3 в 10:35 (через 15 мин) — выключить музыку");
        assert_eq!(e.describe(now + 1150.5), "№3 в 10:35 (через 50 с) — выключить музыку");
        (e.name, e.steps) = ("Чайник".into(), vec![set("Цель", "100 °C"), set("Режим", "Кипячение")]);
        assert_eq!(e.what(), "Чайник: Цель: 100 °C, Режим: Кипячение");
    }

    #[test]
    fn kept_and_never_lost() {
        let p = std::env::temp_dir().join(format!("later-{}.db", std::process::id())).to_string_lossy().to_string();
        let _ = std::fs::remove_file(&p);
        let l = Later::new(Store::open(&p).unwrap());
        let at = now_s() + 300.0;
        let a = l.add("x", at, ("water_heater.kettle", "Чайник"), set("Цель", "100 °C")).unwrap();
        assert!(l.push(a.id, set("Режим", "Кипячение")));
        let b = l.add("x", at + 60.0, ("", ""), step("выключить музыку")).unwrap();
        // a restart: what waits is there, as it was
        let again = Later::new(Store::open(&p).unwrap());
        assert_eq!(again.all_waiting(), l.all_waiting(), "server.db is the list");
        assert_eq!(again.pending("x").iter().map(|e| e.steps.len()).collect::<Vec<_>>(), [2, 1]);
        assert!(again.pending("y").is_empty(), "another station's are not this one's");
        // cancelled by the voice of the station it was asked on only; then it waits no more — and is kept
        assert!(l.cancel(b.id, Some("y")).is_none() && l.cancel(b.id, Some("x")).is_some());
        assert!(l.waiting(b.id).is_none() && l.cancel(b.id, None).is_none() && !l.push(b.id, step("…")));
        assert_eq!(l.get(b.id).map(|e| (e.state, e.ended.is_some())), Some((State::Cancelled, true)));
        // ended: on the main page with how — till it is put out of sight there; the record has all of it
        l.finish(a.id, State::Failed, "устройство не в сети");
        assert!(l.waiting(a.id).is_none() && l.cancel(a.id, None).is_none());
        let j = l.json();
        assert_eq!(j.as_array().map(Vec::len), Some(1), "what was cancelled is not shown: {j}");
        assert_eq!(
            (j[0]["state"].as_str(), j[0]["error"].as_str(), j[0]["what"].as_str()),
            (Some("failed"), Some("устройство не в сети"), Some("Чайник: Цель: 100 °C, Режим: Кипячение"))
        );
        assert!(j[0]["ended"].as_f64().is_some());
        // the page joins a control and its value its own way: they come apart
        assert_eq!(
            (j[0]["name"].as_str(), j[0]["steps"][1]["what"].as_str(), j[0]["steps"][1]["value"].as_str()),
            (Some("Чайник"), Some("Режим"), Some("Кипячение"))
        );
        assert!(l.hide(a.id) && !l.hide(a.id) && l.json() == json!([]));
        let record = Later::new(Store::open(&p).unwrap()).history(None, 10);
        assert_eq!(
            record.iter().map(|e| (e["id"].as_u64().unwrap(), e["state"].as_str().unwrap())).collect::<Vec<_>>(),
            [(b.id, "cancelled"), (a.id, "failed")],
            "by when each was due, the latest first"
        );
        assert!(l.get(a.id).is_some() && l.get(99).is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn what_waits_first_and_the_record_by_pages() {
        let l = Later::in_memory();
        let now = now_s();
        for k in 0..WAITING_MAX {
            l.add("x", now + 600.0 - k as f64, ("", ""), step("…")).unwrap();
        }
        assert!(l.add("x", now + 60.0, ("", ""), step("…")).unwrap_err().starts_with("отложено уже"));
        assert!(l.add("y", now + 60.0, ("", ""), step("…")).is_ok(), "each station has its own");
        let ids: Vec<u64> = l.pending("x").iter().map(|e| e.id).collect();
        assert_eq!(ids, (1..=WAITING_MAX as u64).rev().collect::<Vec<_>>(), "the soonest first");
        // the main page: what waits before what ended; of that, the latest first; a day old — not there any more
        l.finish(ids[0], State::Done, "");
        l.finish(ids[1], State::Missed, MISSED);
        let ended = |id: u64, t: f64| {
            let db = l.store.db.lock().unwrap();
            db.execute("UPDATE later SET ended = ?2 WHERE id = ?1", params![id as i64, t]).unwrap();
        };
        ended(ids[0], now - SHOWN - 1.0);
        ended(ids[1], now - 5.0);
        let j = l.json();
        let states: Vec<&str> = j.as_array().unwrap().iter().map(|e| e["state"].as_str().unwrap()).collect();
        assert_eq!(states.len(), WAITING_MAX - 2 + 1 + 1);
        assert_eq!(states.last(), Some(&"missed"));
        assert!(states[..states.len() - 1].iter().all(|s| *s == "wait"));
        // the record has what the main page has let go of, and is read by pages: each after the last of the one before
        let page = |before: Option<u64>| -> Vec<u64> {
            l.history(before, 8).iter().map(|e| e["id"].as_u64().unwrap()).collect()
        };
        let (first, mut all) = (page(None), vec![]);
        let mut next = first.clone();
        while !next.is_empty() {
            all.extend(next.iter().copied());
            next = page(all.last().copied());
        }
        assert_eq!(first.len(), 8);
        assert_eq!(all.len(), WAITING_MAX + 1, "all of it, once: {all:?}");
        assert!(all.contains(&ids[0]) && page(Some(999)).is_empty());
    }
}
