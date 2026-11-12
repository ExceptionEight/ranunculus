//! The assistant's memory (memory.db).
//! - Every exchange goes to SQLite: when, what was said, the answer, the tools used. Its vector (the embeddings model
//!   chosen in the web UI, providers.rs) is added in the background; the word index (FTS5) by triggers.
//! - search(): by meaning (cosine over all vectors, kept in RAM) and by words (FTS5 with crude Russian stems:
//!   «работу» finds «работа»): meaning first, words only fill in after it (mixed in, they made the ranking worse),
//!   optionally within dates. With no embeddings model, or while its service is away, it finds by words; the
//!   missing vectors are filled in once it is back. Another model: the vectors are made again, all of them.
//! - facts: one-line notes about the user and the home, kept by the note-taker (notes.rs).
use crate::services::Endpoint;
use crate::session::log;
use crate::text::{tz, MONTHS_GEN, WEEKDAYS};
use chrono::{Datelike, NaiveDate, NaiveDateTime, TimeZone, Timelike};
use regex::Regex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::Duration;
use tokio::sync::mpsc;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS exchanges (id INTEGER PRIMARY KEY, t REAL NOT NULL, user TEXT NOT NULL, answer TEXT NOT NULL,
                                      tools TEXT, vec BLOB);
CREATE INDEX IF NOT EXISTS exchanges_t ON exchanges(t);
CREATE VIRTUAL TABLE IF NOT EXISTS words USING fts5(user, answer, content='exchanges', content_rowid='id',
                                                    tokenize='unicode61 remove_diacritics 2');
CREATE TRIGGER IF NOT EXISTS exchanges_ai AFTER INSERT ON exchanges BEGIN
  INSERT INTO words(rowid, user, answer) VALUES (new.id, new.user, new.answer); END;
CREATE TRIGGER IF NOT EXISTS exchanges_ad AFTER DELETE ON exchanges BEGIN
  INSERT INTO words(words, rowid, user, answer) VALUES ('delete', old.id, old.user, old.answer); END;
CREATE TABLE IF NOT EXISTS facts (id INTEGER PRIMARY KEY, t REAL NOT NULL, updated REAL NOT NULL, text TEXT NOT NULL,
                                  active INTEGER NOT NULL DEFAULT 1, source TEXT);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);
";

/// words that say nothing about what to find («что я говорил про…»)
const STOP: &str =
    "что как где когда кто чем чего кого кому это эта этот эти мне меня мой моя моё мое мои моего моей моих
мою мы нас нам наш наша наше наши ты тебе тебя твой твоя вы я он она оно они его её ее их про при был была было были
быть есть у в во на и а но не ни ли да нет же бы то там тут так уже ещё еще вот если или из за от до по для со с о об
говорил говорила говорили сказал сказала помнишь помнит напомни расскажи скажи знаешь какой какая какое какие
каком какую сколько почему зачем разве всё все весь вся";

/// On a test set of 30 questions meaning alone put the right exchange first 29 times; words mixed in at 1.0 / 0.3 /
/// 0.15 made it 25 / 27 / 27. So words only fill in (after the meaning hits) and stand in when there are no vectors
/// (no embeddings model, or its service down).
const WORD_WEIGHT: f64 = 0.0;
/// how long a search waits for its question's vector (it takes a fraction of a second when the service is there)
const QUERY_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct Exchange {
    pub id: i64,
    pub t: f64,
    pub user: String,
    pub answer: String,
    pub tools: Value,
}

#[derive(Debug, Clone)]
pub struct Found {
    pub t: f64,
    pub when: String,
    pub user: String,
    pub answer: String,
}

#[derive(Debug, Clone)]
pub struct Fact {
    pub id: i64,
    pub text: String,
}

#[derive(Default)]
struct Vecs {
    ids: Vec<i64>,
    ts: Vec<f64>,
    mat: Vec<f32>, // ids.len() × dim
    dim: usize,
}

pub struct Memory {
    db: Mutex<Connection>,
    vecs: Mutex<Vecs>,
    /// who makes the vectors (`embed_with`); None: nobody, the search is by words
    embed: RwLock<Option<Endpoint>>,
    todo: mpsc::UnboundedSender<i64>,
}

fn dt(t: f64) -> chrono::DateTime<chrono_tz::Tz> {
    tz().timestamp_opt(t.floor() as i64, 0).single().unwrap()
}

/// For recall results: «среда, 4 ноября 2026, 19:05».
pub fn when(t: f64) -> String {
    let d = dt(t);
    format!(
        "{}, {} {} {}, {:02}:{:02}",
        WEEKDAYS[d.weekday().num_days_from_monday() as usize],
        d.day(),
        MONTHS_GEN[d.month0() as usize],
        d.year(),
        d.hour(),
        d.minute()
    )
}

/// For the note-taker: «среда, 04.11.2026, 08:10».
pub fn numeric(t: f64) -> String {
    let d = dt(t);
    format!("{}, {}", WEEKDAYS[d.weekday().num_days_from_monday() as usize], d.format("%d.%m.%Y, %H:%M"))
}

/// '2026-11-02' → the start (or the end) of that day; '2026-11-02 05:00' / '…T05:00:00' → that moment. The home's time.
pub fn day(s: &str, end: bool) -> Result<f64, String> {
    let s = s.trim();
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > 10 {
        let iso: String = s.replace(' ', "T").chars().take(19).collect();
        for f in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%dT%H"] {
            if let Ok(d) = NaiveDateTime::parse_from_str(&iso, f) {
                return Ok(crate::text::local(&d).timestamp() as f64);
            }
        }
    }
    let head: String = chars.iter().take(10).collect();
    let d = NaiveDate::parse_from_str(&head, "%Y-%m-%d").map_err(|_| format!("Invalid isoformat string: '{head}'"))?;
    let at = if end { d.and_hms_micro_opt(23, 59, 59, 999_999).unwrap() } else { d.and_hms_opt(0, 0, 0).unwrap() };
    let ts = crate::text::local(&at);
    Ok(ts.timestamp() as f64 + ts.timestamp_subsec_micros() as f64 / 1e6)
}

/// Words of a question → an FTS5 query: servers with a prefix star, OR-ed (crude Russian: up to two last letters
/// dropped, at least three kept).
pub fn fts_query(q: &str) -> String {
    static WORD: OnceLock<Regex> = OnceLock::new();
    static STOPS: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new();
    let word = WORD.get_or_init(|| Regex::new("[а-яёa-z0-9]+").unwrap());
    let stops = STOPS.get_or_init(|| STOP.split_whitespace().collect());
    let low = q.to_lowercase().replace('ё', "е");
    let mut terms: Vec<String> = vec![];
    for m in word.find_iter(&low) {
        let w = m.as_str();
        let n = w.chars().count();
        if stops.contains(w) || n < 3 {
            continue;
        }
        let server: String = if n <= 3 { w.into() } else { w.chars().take(3.max(n - 2)).collect() };
        let term = format!("\"{server}\"*");
        if !terms.contains(&term) {
            terms.push(term);
        }
    }
    terms.join(" OR ")
}

pub fn doc(user: &str, answer: &str) -> String {
    format!("Пользователь: {user}\nМразота: {answer}")
}

/// Who made the vectors: a model of a service with its request fields; not the key (another key, the same model).
fn made_by(e: &Endpoint) -> String {
    format!("{} {} {}", e.url, e.model, Value::Object(e.extra.clone()))
}

/// What of an exchange its vector is made of, in characters: a service refuses a text over its limit, and one refused
/// would hold up every vector after it. The e5 the memory began with read 512 tokens, about as much.
const DOC_MAX: usize = 2000;

fn clip(mut s: String, max: usize) -> String {
    if let Some((i, _)) = s.char_indices().nth(max) {
        s.truncate(i);
    }
    s
}

fn sql(e: rusqlite::Error) -> String {
    format!("memory: {e}")
}

impl Memory {
    /// Opens (creating it if needed) the database. Searched by words until `embed_with` names who makes the vectors.
    /// Must be called within the tokio runtime: the embedder runs there.
    pub fn open(path: &str) -> Result<Arc<Memory>, String> {
        let p = std::path::Path::new(path);
        if let Some(dir) = p.parent() {
            if !dir.exists() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        let new = !p.exists();
        let db = Connection::open(p).map_err(|e| format!("{path}: {e}"))?;
        if new {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
        }
        db.busy_timeout(Duration::from_secs(5)).map_err(sql)?;
        db.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0)).map_err(sql)?;
        db.execute_batch(SCHEMA).map_err(sql)?;
        let mut vecs = Vecs::default();
        {
            let mut st =
                db.prepare("SELECT id, t, vec FROM exchanges WHERE vec IS NOT NULL ORDER BY id").map_err(sql)?;
            let rows = st
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?, r.get::<_, Vec<u8>>(2)?)))
                .map_err(sql)?;
            for r in rows {
                let (id, t, b) = r.map_err(sql)?;
                let v: Vec<f32> = b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
                if vecs.dim == 0 {
                    vecs.dim = v.len();
                }
                if v.len() != vecs.dim {
                    continue;
                }
                vecs.ids.push(id);
                vecs.ts.push(t);
                vecs.mat.extend(v);
            }
        }
        let (todo, rx) = mpsc::unbounded_channel();
        let m = Arc::new(Memory { db: Mutex::new(db), vecs: Mutex::new(vecs), embed: RwLock::new(None), todo });
        tokio::spawn(embedder(Arc::downgrade(&m), rx));
        Ok(m)
    }

    /// Who makes the vectors from now on; None: nobody, the search is by words. Another model's vectors say nothing
    /// to this one's questions: they are dropped, and every exchange gets a new one in the background.
    pub fn embed_with(&self, e: Option<Endpoint>) {
        {
            let mut now = self.embed.write().unwrap();
            if *now == e {
                return;
            }
            *now = e.clone();
        }
        let Some(e) = e else { return };
        let made_by = made_by(&e);
        // whose the vectors are, unknown (a read that failed): nothing dropped or made; the next change or start asks
        // again
        let was = match self.meta_read("embedder") {
            Ok(was) => was,
            Err(e) => return log("server", format!("   ({e}: the vectors are left as they are)")),
        };
        if was.as_deref() != Some(made_by.as_str()) {
            let dropped = {
                // the index emptied under the database's lock too: the embedder writes under it (both, in this order)
                let db = self.db.lock().unwrap();
                *self.vecs.lock().unwrap() = Vecs::default();
                db.execute("UPDATE exchanges SET vec = NULL WHERE vec IS NOT NULL", []).map_err(sql)
            };
            // the old ones still there: not said to be the new model's, so they are dropped again next time
            let dropped = match dropped {
                Ok(n) => n,
                Err(e) => return log("server", format!("   ({e}: searched by words until the next start)")),
            };
            if let Err(e) = self.set_meta("embedder", &made_by) {
                log("server", format!("   ({e})"));
            }
            if dropped > 0 {
                log("server", format!("memory: another embeddings model, {dropped} vectors are made again"));
            }
        }
        match self.backfill() {
            Ok(0) => {}
            Ok(n) => log("server", format!("memory: {n} exchanges get their vectors ({})", e.model)),
            Err(e) => log("server", format!("   ({e})")),
        }
    }

    // ------------------------------------------------------------ writing
    pub fn add(&self, user: &str, answer: &str, tools: &Value, t: f64) -> Result<i64, String> {
        let id = {
            let db = self.db.lock().unwrap();
            db.execute(
                "INSERT INTO exchanges (t, user, answer, tools) VALUES (?, ?, ?, ?)",
                params![t, user, answer, tools.to_string()],
            )
            .map_err(sql)?;
            db.last_insert_rowid()
        };
        let _ = self.todo.send(id);
        Ok(id)
    }

    /// Every exchange without a vector yet (no embeddings model then, its service down, an import) goes to the
    /// embedder.
    pub fn backfill(&self) -> Result<usize, String> {
        let ids: Vec<i64> = {
            let db = self.db.lock().unwrap();
            let mut st = db.prepare("SELECT id FROM exchanges WHERE vec IS NULL ORDER BY id").map_err(sql)?;
            let r = st.query_map([], |r| r.get(0)).map_err(sql)?.collect::<Result<_, _>>().map_err(sql)?;
            r
        };
        for id in &ids {
            let _ = self.todo.send(*id);
        }
        Ok(ids.len())
    }

    /// Texts → their vectors, by the model in use.
    pub async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let e = self.embed.read().unwrap().clone().ok_or("no embeddings model is chosen")?;
        crate::services::embed(&e, texts).await
    }

    // ------------------------------------------------------------ reading
    pub fn recent(&self, n: usize) -> Result<Vec<Exchange>, String> {
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare("SELECT id, t, user, answer, tools FROM exchanges ORDER BY t DESC, id DESC LIMIT ?")
            .map_err(sql)?;
        let rows = st
            .query_map([n as i64], |r| {
                let tools: Option<String> = r.get(4)?;
                Ok(Exchange {
                    id: r.get(0)?,
                    t: r.get(1)?,
                    user: r.get(2)?,
                    answer: r.get(3)?,
                    tools: serde_json::from_str(tools.as_deref().unwrap_or("[]")).unwrap_or(json!([])),
                })
            })
            .map_err(sql)?;
        let mut v: Vec<Exchange> = rows.collect::<Result<_, _>>().map_err(sql)?;
        v.reverse();
        Ok(v)
    }

    pub fn count(&self) -> usize {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT COUNT(*) FROM exchanges", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize
    }

    /// → best first. since/until: 'YYYY-MM-DD' (the home's days).
    pub async fn search(
        &self,
        query: &str,
        since: Option<&str>,
        until: Option<&str>,
        k: usize,
    ) -> Result<Vec<Found>, String> {
        let pool = 30usize;
        let lo = match since {
            Some(s) => day(s, false)?,
            None => -1e18,
        };
        let hi = match until {
            Some(s) => day(s, true)?,
            None => 1e18,
        };
        // ranks in the order they came in: ties keep it
        let mut order: Vec<i64> = vec![];
        let mut ranks: HashMap<i64, f64> = HashMap::new();
        let mut add = |id: i64, s: f64, order: &mut Vec<i64>| {
            let e = ranks.entry(id).or_insert_with(|| {
                order.push(id);
                0.0
            });
            *e += s;
        };
        let have_vecs = !self.vecs.lock().unwrap().ids.is_empty() && self.embed.read().unwrap().is_some();
        if have_vecs {
            // the embeddings service may be away, or slow today: by words then
            if let Ok(Ok(q)) = tokio::time::timeout(QUERY_WAIT, self.embed(&[query.to_string()])).await {
                if let Some(q) = q.first() {
                    let hits: Vec<i64> = {
                        let v = self.vecs.lock().unwrap();
                        if q.len() == v.dim {
                            let mut scored: Vec<(f32, usize)> = (0..v.ids.len())
                                .map(|j| {
                                    (
                                        v.mat[j * v.dim..(j + 1) * v.dim]
                                            .iter()
                                            .zip(q)
                                            .map(|(a, b)| a * b)
                                            .sum::<f32>(),
                                        j,
                                    )
                                })
                                .collect();
                            scored.sort_by(|a, b| b.0.total_cmp(&a.0));
                            scored
                                .iter()
                                .filter(|(_, j)| lo <= v.ts[*j] && v.ts[*j] <= hi)
                                .take(pool)
                                .map(|(_, j)| v.ids[*j])
                                .collect()
                        } else {
                            vec![]
                        }
                    };
                    for (r, id) in hits.iter().enumerate() {
                        add(*id, 1.0 / (60.0 + r as f64), &mut order);
                    }
                }
            }
        }
        let fq = fts_query(query);
        if !fq.is_empty() {
            let rows: Vec<i64> = {
                let db = self.db.lock().unwrap();
                let mut st = db
                    .prepare(
                        "SELECT e.id FROM words JOIN exchanges e ON e.id = words.rowid WHERE words MATCH ? \
                                         AND e.t BETWEEN ? AND ? ORDER BY bm25(words) LIMIT ?",
                    )
                    .map_err(sql)?;
                let r = st
                    .query_map(params![fq, lo, hi, pool as i64], |r| r.get(0))
                    .map_err(sql)?
                    .collect::<Result<_, _>>()
                    .map_err(sql)?;
                r
            };
            let w = if order.is_empty() { 1.0 } else { WORD_WEIGHT }; // words decide alone only when there are no vectors
            for (r, id) in rows.iter().enumerate() {
                add(*id, w / (60.0 + r as f64), &mut order);
            }
        }
        let mut best: Vec<(i64, f64)> = order.iter().map(|id| (*id, ranks[id])).collect();
        best.sort_by(|a, b| b.1.total_cmp(&a.1)); // stable: ties keep their order
        best.truncate(k);
        if best.is_empty() {
            return Ok(vec![]);
        }
        let db = self.db.lock().unwrap();
        let ids = best.iter().map(|b| b.0.to_string()).collect::<Vec<_>>().join(",");
        let mut st =
            db.prepare(&format!("SELECT id, t, user, answer FROM exchanges WHERE id IN ({ids})")).map_err(sql)?;
        let rows: HashMap<i64, (f64, String, String)> = st
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))
            .map_err(sql)?
            .collect::<Result<_, _>>()
            .map_err(sql)?;
        Ok(best
            .iter()
            .filter_map(|(id, _)| {
                rows.get(id).map(|(t, u, a)| Found { t: *t, when: when(*t), user: u.clone(), answer: a.clone() })
            })
            .collect())
    }

    // ------------------------------------------------------------ facts
    pub fn facts(&self) -> Vec<Fact> {
        let db = self.db.lock().unwrap();
        let Ok(mut st) = db.prepare("SELECT id, text FROM facts WHERE active = 1 ORDER BY id") else { return vec![] };
        st.query_map([], |r| Ok(Fact { id: r.get(0)?, text: r.get(1)? }))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    /// ops: {"add": [text], "update": [{"id", "text"}], "remove": [id]} → (added, updated, removed).
    pub fn apply_facts(&self, ops: &Value, source: &str) -> Result<(usize, usize, usize), String> {
        let now = crate::text::now_s();
        let known: std::collections::HashSet<i64> = self.facts().iter().map(|f| f.id).collect();
        let id_of = |v: &Value| v.as_i64().or_else(|| v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64));
        let mut n = (0, 0, 0);
        let db = self.db.lock().unwrap();
        for text in ops["add"].as_array().into_iter().flatten() {
            if let Some(t) = text.as_str().map(str::trim).filter(|t| !t.is_empty()) {
                db.execute(
                    "INSERT INTO facts (t, updated, text, source) VALUES (?, ?, ?, ?)",
                    params![now, now, t, source],
                )
                .map_err(sql)?;
                n.0 += 1;
            }
        }
        for u in ops["update"].as_array().into_iter().flatten() {
            let text = match &u["text"] {
                Value::String(s) => s.trim().to_string(),
                Value::Null => String::new(),
                v => v.to_string(),
            };
            if let Some(id) = id_of(&u["id"]).filter(|id| known.contains(id)) {
                if !text.is_empty() {
                    db.execute("UPDATE facts SET text = ?, updated = ? WHERE id = ?", params![text, now, id])
                        .map_err(sql)?;
                    n.1 += 1;
                }
            }
        }
        for r in ops["remove"].as_array().into_iter().flatten() {
            if let Some(id) = id_of(r).filter(|id| known.contains(id)) {
                db.execute("UPDATE facts SET active = 0, updated = ? WHERE id = ?", params![now, id]).map_err(sql)?;
                n.2 += 1;
            }
        }
        Ok(n)
    }

    pub fn meta(&self, key: &str) -> Option<String> {
        self.meta_read(key).ok().flatten()
    }

    /// A read that failed told apart from nothing there.
    fn meta_read(&self, key: &str) -> Result<Option<String>, String> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT value FROM meta WHERE key = ?", [key], |r| r.get::<_, Option<String>>(0))
            .optional()
            .map(Option::flatten)
            .map_err(sql)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), String> {
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )
        .map_err(sql)?;
        Ok(())
    }

    pub fn since_id(&self, rid: i64) -> Result<Vec<Exchange>, String> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare("SELECT id, t, user, answer FROM exchanges WHERE id > ? ORDER BY id").map_err(sql)?;
        let rows = st
            .query_map([rid], |r| {
                Ok(Exchange { id: r.get(0)?, t: r.get(1)?, user: r.get(2)?, answer: r.get(3)?, tools: Value::Null })
            })
            .map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }
}

/// In the background: vectors for new exchanges, in batches of up to 32. The service down — again in 30 s; words
/// work meanwhile.
async fn embedder(mem: Weak<Memory>, mut rx: mpsc::UnboundedReceiver<i64>) {
    let mut failing = false;
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while batch.len() < 32 {
            match rx.try_recv() {
                Ok(id) => batch.push(id),
                Err(_) => break,
            }
        }
        let Some(m) = mem.upgrade() else { return };
        // nobody to make them: they wait in the database for `embed_with`
        let Some(by) = m.embed.read().unwrap().clone() else { continue };
        let rows: Vec<(i64, String, String, f64)> = {
            let db = m.db.lock().unwrap();
            let ids = batch.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
            let Ok(mut st) =
                db.prepare(&format!("SELECT id, user, answer, t FROM exchanges WHERE vec IS NULL AND id IN ({ids})"))
            else {
                continue;
            };
            let r = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map(|x| x.filter_map(Result::ok).collect())
                .unwrap_or_default();
            r
        };
        if rows.is_empty() {
            continue;
        }
        let texts: Vec<String> = rows.iter().map(|(_, u, a, _)| clip(doc(u, a), DOC_MAX)).collect();
        let made = crate::services::embed(&by, &texts).await;
        // another model chosen while these were made: they are not its vectors, and `embed_with` asked for new ones
        // (another key alone: the same model, the vectors are kept)
        let same = |m: &Memory| m.embed.read().unwrap().as_ref().map(made_by) == Some(made_by(&by));
        if !same(&m) {
            continue;
        }
        match made {
            Ok(v) => {
                failing = false;
                let db = m.db.lock().unwrap();
                // and once more under the lock the old ones are dropped under: a switch is wholly before or after this
                if !same(&m) {
                    continue;
                }
                let mut vecs = m.vecs.lock().unwrap();
                for ((id, _, _, t), vec) in rows.iter().zip(v) {
                    let b: Vec<u8> = vec.iter().flat_map(|x| x.to_le_bytes()).collect();
                    if db.execute("UPDATE exchanges SET vec = ? WHERE id = ?", params![b, id]).is_err() {
                        continue;
                    }
                    if vecs.dim == 0 {
                        vecs.dim = vec.len();
                    }
                    if vec.len() == vecs.dim {
                        vecs.ids.push(*id);
                        vecs.ts.push(*t);
                        vecs.mat.extend(vec);
                    }
                }
            }
            Err(e) => {
                if !failing {
                    log("server", format!("   (memory: {e}: searched by words until the vectors can be made)"));
                }
                failing = true;
                drop(m);
                tokio::time::sleep(Duration::from_secs(30)).await;
                let Some(m) = mem.upgrade() else { return };
                for (id, ..) in &rows {
                    let _ = m.todo.send(*id);
                }
            }
        }
    }
}

// ---------------------------------------------------------------- the note-taker's prompt
pub const FACTS_PROMPT: &str = "Ты ведёшь заметки голосового ассистента Мразоты о её хозяине и его доме. Ниже — нынешние заметки (с номерами) и новые реплики разговора с временем.

Верни JSON: {\"add\": [новые заметки], \"update\": [{\"id\": номер, \"text\": новый текст}], \"remove\": [номера]}.

Что записывать — только долгое и личное: имена и роли близких, друзей, коллег и питомцев; дни рождения, годовщины и другие даты; где хозяин работает и кем; вкусы и что не нравится; здоровье и аллергии; привычки; планы и договорённости с датами; важные события в жизни; где что лежит; просьбы к Мразоте, как себя вести.
Что не записывать: разовые команды (погода, таймер, музыка, громкость, посчитать), общие знания и ответы на вопросы о мире, то, что Мразота сказала о себе, догадки.
Обязательно: кем и где хозяин работает; любые его просьбы к Мразоте, как себя вести («не шути про…», «по вечерам тише»).
Кто кем приходится — ровно как сказано: брат — это брат, а не друг; не смешивай разных людей с одним именем.
Как: одна короткая фраза на заметку, от третьего лица, без «пользователь сказал»: «Кота зовут Барсик.» Относительные даты переводи в настоящие по дате той реплики, где они сказаны: «сегодня» — это её дата, «завтра» — следующий день, «в пятницу» — ближайшая пятница после неё. Пример: реплика [среда, 04.11.2026, 08:10] «Сегодня иду на собеседование» — заметка «Собеседование в Яндексе 04.11.2026.» Если новое меняет старую заметку (передумал, переехал, событие прошло, стало лучше) — update с её номером; если заметка больше не верна совсем — remove. Не дублируй то, что уже записано. Нечего записать — пустые списки.";

pub fn facts_schema() -> Value {
    json!({"type": "object", "properties": {
        "add": {"type": "array", "items": {"type": "string"}},
        "update": {"type": "array", "items": {"type": "object", "properties": {"id": {"type": "integer"}, "text": {"type": "string"}}, "required": ["id", "text"]}},
        "remove": {"type": "array", "items": {"type": "integer"}}}, "required": ["add", "update", "remove"]})
}

pub fn facts_messages(facts: &[Fact], exchanges: &[Exchange]) -> Value {
    let notes = facts.iter().map(|f| format!("{}. {}", f.id, f.text)).collect::<Vec<_>>().join("\n");
    let notes = if notes.is_empty() { "(пока нет)".to_string() } else { notes };
    let talk = exchanges
        .iter()
        .map(|x| format!("[{}] Хозяин: {}\nМразота: {}", numeric(x.t), x.user, x.answer))
        .collect::<Vec<_>>()
        .join("\n");
    json!([{"role": "system", "content": FACTS_PROMPT}, {"role": "user", "content": format!("Заметки:\n{notes}\n\nНовые реплики:\n{talk}")}])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> String {
        let d = std::env::temp_dir().join(format!("server-mem-{}-{}", std::process::id(), rand::random::<u32>()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("m.db").to_string_lossy().into()
    }

    #[test]
    fn fts_like_python() {
        // memory.fts_query
        assert_eq!(fts_query("Что я говорил про работу?"), r#""рабо"*"#);
        assert_eq!(fts_query("Помнишь, как зовут моего кота Барсика?"), r#""зов"* OR "кот"* OR "барси"*"#);
        assert_eq!(fts_query("ещё ёлка кот"), r#""елк"* OR "кот"*"#);
        assert_eq!(fts_query("я в на"), "");
    }

    #[test]
    fn a_long_exchange_is_embedded_by_its_beginning() {
        let long = clip(doc("Расскажи сказку", &"я".repeat(3000)), DOC_MAX);
        assert!(long.starts_with("Пользователь: Расскажи сказку\n") && long.chars().count() == DOC_MAX);
        assert_eq!(clip("кот".into(), DOC_MAX), "кот");
    }

    #[test]
    fn days_in_moscow() {
        // memory._day, when, _numeric
        assert_eq!(day("2026-11-02", false).unwrap(), 1_793_566_800.0);
        assert!((day("2026-11-02", true).unwrap() - 1_793_653_199.999_999).abs() < 1e-5);
        assert_eq!(day("2026-11-02 05:00", false).unwrap(), 1_793_584_800.0);
        assert!(day("вчера", false).is_err());
        assert_eq!(when(1_793_566_800.0 + 19.0 * 3600.0 + 300.0), "понедельник, 2 ноября 2026, 19:05");
        assert_eq!(numeric(1_793_566_800.0 + 8.0 * 3600.0 + 600.0), "понедельник, 02.11.2026, 08:10");
    }

    #[tokio::test]
    async fn store_search_facts() {
        let path = tmp();
        let m = Memory::open(&path).unwrap();
        let t0 = day("2026-11-01", false).unwrap();
        m.add("У меня кот рыжий, Барсик зовут.", "Привет Барсику!", &json!([]), t0 + 3600.0).unwrap();
        m.add(
            "Включи музыку",
            "Включаю.",
            &json!([{"tool": "play_music", "args": {"query": "музыка"}, "res": {"ok": true}}]),
            t0 + 2.0 * 86400.0,
        )
        .unwrap();
        m.add("Какая погода?", "Пасмурно, плюс пять.", &json!([]), t0 + 3.0 * 86400.0).unwrap();
        assert_eq!(m.count(), 3);
        let r = m.recent(2).unwrap();
        assert_eq!(r.iter().map(|x| x.user.as_str()).collect::<Vec<_>>(), ["Включи музыку", "Какая погода?"]);
        assert_eq!(r[0].tools[0]["tool"], "play_music");
        let f = m.search("как зовут кота", None, None, 6).await.unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].user, "У меня кот рыжий, Барсик зовут.");
        assert!(
            m.search("кот", Some("2026-11-02"), Some("2026-11-04"), 6).await.unwrap().is_empty(),
            "outside the dates"
        );
        let n = m.apply_facts(&json!({"add": ["Кота зовут Барсик.", " "], "update": [], "remove": []}), "1-1").unwrap();
        assert_eq!(n, (1, 0, 0));
        let id = m.facts()[0].id;
        assert_eq!(m.apply_facts(&json!({"add": [], "update": [{"id": id, "text": "Кота зовут Барсик, он рыжий."}, {"id": 99, "text": "x"}], "remove": []}), "").unwrap(), (0, 1, 0));
        assert_eq!(m.facts()[0].text, "Кота зовут Барсик, он рыжий.");
        assert_eq!(m.apply_facts(&json!({"add": [], "update": [], "remove": [id]}), "").unwrap(), (0, 0, 1));
        assert!(m.facts().is_empty());
        m.set_meta("facts_upto", "2").unwrap();
        assert_eq!(m.meta("facts_upto").as_deref(), Some("2"));
        assert_eq!(m.since_id(2).unwrap().len(), 1);
        let msgs = facts_messages(&[], &m.since_id(0).unwrap()[..1]);
        assert!(msgs[1]["content"]
            .as_str()
            .unwrap()
            .contains("(пока нет)\n\nНовые реплики:\n[воскресенье, 01.11.2026, 01:00] Хозяин: У меня кот"));
        let _ = std::fs::remove_dir_all(std::path::Path::new(&path).parent().unwrap());
    }
    #[tokio::test]
    async fn vectors_by_the_model_in_use() {
        let path = tmp();
        let f = crate::services::fake::Service::start().await;
        let by = |model: &str| Endpoint { url: f.url.clone(), model: model.into(), ..Default::default() };
        let vectors = |m: &Memory| {
            let db = m.db.lock().unwrap();
            db.query_row("SELECT COUNT(*) FROM exchanges WHERE vec IS NOT NULL", [], |r| r.get::<_, i64>(0)).unwrap()
        };
        async fn until(what: impl Fn() -> bool) {
            for _ in 0..200 {
                if what() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("not in two seconds");
        }
        let m = Memory::open(&path).unwrap();
        m.add("Мою собаку зовут Грета.", "Запомнила.", &json!([]), 1.0).unwrap();
        m.add("Какая погода?", "Плюс пять.", &json!([]), 2.0).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!((vectors(&m), f.asked().len()), (0, 0), "nobody to make them yet");
        // a model chosen: what was waiting gets its vectors, and the search asks for the question's
        m.embed_with(Some(by("a/one")));
        until(|| vectors(&m) == 2).await;
        assert_eq!(m.meta("embedder").as_deref(), Some(format!("{} a/one {{}}", f.url).as_str()));
        let found = m.search("питомец", None, None, 6).await.unwrap();
        assert_eq!(found.len(), 2, "by the vectors: the words find nothing here");
        // the same one named again (a new key, a restart): nothing is made again
        let asked = f.asked().len();
        m.embed_with(Some(Endpoint { key: "sk-new".into(), ..by("a/one") }));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!((vectors(&m), f.asked().len()), (2, asked));
        // another model: all of them again, by it
        m.embed_with(Some(by("a/two")));
        until(|| vectors(&m) == 2 && f.asked().last().is_some_and(|a| a.body["model"] == "a/two")).await;
        assert_eq!(m.vecs.lock().unwrap().ids.len(), 2);
        // nobody again: the vectors stay for the day it is back, the search is by words
        m.embed_with(None);
        assert!(m.search("питомец", None, None, 6).await.unwrap().is_empty());
        assert_eq!(m.search("собака", None, None, 6).await.unwrap().len(), 1);
        drop(m);
        // a restart: the vectors are read, and the same model keeps them
        let m = Memory::open(&path).unwrap();
        assert_eq!(m.vecs.lock().unwrap().ids.len(), 2);
        let asked = f.asked().len();
        m.embed_with(Some(by("a/two")));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!((vectors(&m), f.asked().len()), (2, asked));
        let _ = std::fs::remove_dir_all(std::path::Path::new(&path).parent().unwrap());
    }
}
