//! server.db (600): the settings, the devices seen (and the names the user gave them), the alarms, the providers, the
//! wakes recorded, the music listened to, the web UI's users and sessions (web/auth.rs) and the chats: every exchange
//! of every device, with what was heard, the answer, the tools and the timings.
//! The assistant's memory (memory.rs) is another thing: what it remembers; this is the record.
//! The schema grows by numbered steps (PRAGMA user_version), never edited in place.
use crate::text::tz;
use chrono::TimeZone;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MIGRATIONS: &[&str] = &["
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL, t REAL NOT NULL);
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE devices (id TEXT PRIMARY KEY, name TEXT NOT NULL DEFAULT '', first_seen REAL NOT NULL, last_seen REAL NOT NULL);
CREATE TABLE exchanges (id INTEGER PRIMARY KEY, t REAL NOT NULL, device TEXT NOT NULL, heard TEXT NOT NULL, text TEXT NOT NULL,
                        answer TEXT NOT NULL, calls TEXT NOT NULL, ring TEXT, cut INTEGER NOT NULL DEFAULT 0,
                        asr_s REAL, llm_s REAL, from_hit_s REAL, model TEXT);
CREATE INDEX exchanges_device ON exchanges (device, id);
CREATE TABLE alarms (id INTEGER PRIMARY KEY, device TEXT NOT NULL, minute INTEGER NOT NULL, days INTEGER NOT NULL,
                     label TEXT NOT NULL DEFAULT '', enabled INTEGER NOT NULL, sound TEXT, ring REAL, volume REAL, ramp REAL,
                     armed REAL NOT NULL, rang REAL);
CREATE TABLE providers (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, url TEXT NOT NULL, model TEXT NOT NULL DEFAULT '',
                        key TEXT NOT NULL DEFAULT '', voice TEXT NOT NULL DEFAULT '', speed REAL, extra TEXT NOT NULL DEFAULT '{}',
                        updated REAL NOT NULL);
CREATE TABLE wakes (id INTEGER PRIMARY KEY, t REAL NOT NULL, device TEXT NOT NULL, p REAL NOT NULL, barge INTEGER NOT NULL,
                    button INTEGER NOT NULL, pre REAL NOT NULL, seconds REAL NOT NULL, file TEXT NOT NULL, quiet_before REAL,
                    heard_window TEXT, heard TEXT, label TEXT);
CREATE INDEX wakes_device ON wakes (device, id);
CREATE TABLE tracks (id TEXT PRIMARY KEY, provider TEXT NOT NULL, title TEXT NOT NULL, artist TEXT NOT NULL, album TEXT,
                     duration REAL, isrc TEXT, song TEXT NOT NULL, first_t REAL NOT NULL, last_t REAL NOT NULL);
CREATE INDEX tracks_song ON tracks (song);
CREATE INDEX tracks_isrc ON tracks (isrc) WHERE isrc IS NOT NULL;
CREATE TABLE listens (id INTEGER PRIMARY KEY, t REAL NOT NULL, device TEXT NOT NULL, track TEXT NOT NULL, heard REAL NOT NULL,
                      reached REAL NOT NULL, duration REAL, ended TEXT, t_end REAL);
CREATE INDEX listens_t ON listens (t);
CREATE INDEX listens_device ON listens (device, t);
CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, key TEXT UNIQUE, password TEXT, issuer TEXT, subject TEXT,
                    created REAL NOT NULL, changed REAL NOT NULL);
CREATE UNIQUE INDEX users_oidc ON users (issuer, subject) WHERE issuer IS NOT NULL;
CREATE TABLE sessions (hash TEXT PRIMARY KEY, user INTEGER NOT NULL, t REAL NOT NULL, seen REAL NOT NULL, auth_t REAL NOT NULL,
                       ip TEXT NOT NULL, agent TEXT NOT NULL);
CREATE INDEX sessions_user ON sessions (user);
"];

pub struct Store {
    pub(crate) db: Mutex<Connection>,
}

/// A provider as kept (providers.rs has what it means). `key` never leaves the server.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRow {
    pub id: i64,
    pub kind: String,
    pub name: String,
    pub url: String,
    pub model: String,
    pub key: String,
    pub voice: String,
    pub speed: Option<f64>,
    pub extra: Value,
    pub updated: f64,
}

/// An alarm as kept (alarms.rs has what it means).
#[derive(Debug, Clone, PartialEq)]
pub struct AlarmRow {
    pub id: i64,
    pub device: String,
    pub minute: u32,
    pub days: u8,
    pub label: String,
    pub enabled: bool,
    pub sound: Option<String>,
    pub ring: Option<f64>,
    pub volume: Option<f64>,
    pub ramp: Option<f64>,
    pub armed: f64,
    pub rang: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub first_seen: f64,
    pub last_seen: f64,
}

pub(crate) fn sql(e: rusqlite::Error) -> String {
    format!("server.db: {e}")
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().filter(|x| x.is_finite())
}

impl Store {
    /// Opens (creating, 600, in a 700 folder) and brings the schema up to date.
    pub fn open(path: &str) -> Result<Arc<Store>, String> {
        use std::os::unix::fs::PermissionsExt;
        let p = std::path::Path::new(path);
        if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty() && !d.exists()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
        let new = !p.exists();
        let db = Connection::open(p).map_err(|e| format!("{path}: {e}"))?;
        if new {
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
        }
        db.busy_timeout(Duration::from_secs(5)).map_err(sql)?;
        db.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0)).map_err(sql)?;
        // the statistics count by the home's days and hours, whatever its time zone does with the clocks
        let at = |ctx: &rusqlite::functions::Context| -> rusqlite::Result<Option<chrono::DateTime<chrono_tz::Tz>>> {
            Ok(tz().timestamp_opt(ctx.get::<f64>(0)? as i64, 0).single())
        };
        let flags = rusqlite::functions::FunctionFlags::SQLITE_UTF8;
        db.create_scalar_function("local_day", 1, flags, move |ctx| {
            Ok(at(ctx)?.map(|d| d.format("%Y-%m-%d").to_string()))
        })
        .map_err(sql)?;
        db.create_scalar_function("local_hour", 1, flags, move |ctx| {
            Ok(at(ctx)?.map(|d| chrono::Timelike::hour(&d) as i64))
        })
        .map_err(sql)?;
        let at: usize = db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)).map_err(sql)? as usize;
        if at > MIGRATIONS.len() {
            return Err(format!("{path}: schema {at} is newer than this server's {}", MIGRATIONS.len()));
        }
        for (i, step) in MIGRATIONS.iter().enumerate().skip(at) {
            let tx = db.unchecked_transaction().map_err(sql)?;
            tx.execute_batch(step).map_err(sql)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1)).map_err(sql)?;
            tx.commit().map_err(sql)?;
        }
        Ok(Arc::new(Store { db: Mutex::new(db) }))
    }

    // ------------------------------------------------------------ settings
    /// None: none kept, or it could not be read (`setting_kept` tells which).
    pub fn setting(&self, key: &str) -> Option<Value> {
        self.setting_kept(key).ok().flatten()
    }

    /// Ok(None): none kept; Err: the store could not be read, or what it keeps is not JSON — nothing is to be written
    /// over it then (taken for «none», a failed read would put the defaults over the user's settings).
    pub fn setting_kept(&self, key: &str) -> Result<Option<Value>, String> {
        let db = self.db.lock().unwrap();
        let Some(s) = db
            .query_row("SELECT value FROM settings WHERE key = ?", [key], |r| r.get::<_, String>(0))
            .optional()
            .map_err(sql)?
        else {
            return Ok(None);
        };
        serde_json::from_str(&s).map(Some).map_err(|e| format!("server.db: settings «{key}» is not JSON: {e}"))
    }

    pub fn set_setting(&self, key: &str, value: &Value, t: f64) -> Result<(), String> {
        self.set_settings(&[(key, value.clone())], t)
    }

    /// Several at once: all of them or none (one transaction).
    pub fn set_settings(&self, items: &[(&str, Value)], t: f64) -> Result<(), String> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction().map_err(sql)?;
        for (key, value) in items {
            tx.execute("INSERT INTO settings (key, value, t) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, t = excluded.t",
                       params![key, value.to_string(), t]).map_err(sql)?;
        }
        tx.commit().map_err(sql)
    }

    // ------------------------------------------------------------ devices
    /// The device connected (or spoke) at `t`.
    pub fn seen(&self, device: &str, t: f64) -> Result<(), String> {
        let db = self.db.lock().unwrap();
        seen(&db, device, t)
    }

    pub fn devices(&self) -> Result<Vec<Device>, String> {
        let db = self.db.lock().unwrap();
        let mut st =
            db.prepare("SELECT id, name, first_seen, last_seen FROM devices ORDER BY first_seen, id").map_err(sql)?;
        let rows = st
            .query_map([], |r| {
                Ok(Device { id: r.get(0)?, name: r.get(1)?, first_seen: r.get(2)?, last_seen: r.get(3)? })
            })
            .map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }

    /// → false: no such device.
    pub fn rename(&self, device: &str, name: &str) -> Result<bool, String> {
        let db = self.db.lock().unwrap();
        Ok(db.execute("UPDATE devices SET name = ? WHERE id = ?", params![name, device]).map_err(sql)? > 0)
    }

    // ------------------------------------------------------------ the chats
    /// One exchange as the journal has it (session::journal: device, heard, text, answer, calls, ring, cut, timings),
    /// at `t`. → (its id, the row as the API shows it)
    pub fn add_exchange(&self, line: &Value, t: f64) -> Result<(i64, Value), String> {
        let db = self.db.lock().unwrap();
        let id = insert(&db, line, t)?;
        let row = db.query_row(&format!("{SELECT} WHERE id = ?"), [id], row).map_err(sql)?;
        Ok((id, row))
    }

    /// Newest first: up to `limit` before the exchange `before` (None — the latest); a device's, or everyone's.
    pub fn exchanges(&self, device: Option<&str>, before: Option<i64>, limit: usize) -> Result<Vec<Value>, String> {
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare(&format!("{SELECT} WHERE (?1 IS NULL OR device = ?1) AND id < ?2 ORDER BY id DESC LIMIT ?3"))
            .map_err(sql)?;
        let rows = st.query_map(params![device, before.unwrap_or(i64::MAX), limit as i64], row).map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }

    /// Exchanges a device has in the chats (all devices: None).
    pub fn count(&self, device: Option<&str>) -> usize {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT COUNT(*) FROM exchanges WHERE ?1 IS NULL OR device = ?1", params![device], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap_or(0) as usize
    }

    // ------------------------------------------------------------ providers
    pub fn providers(&self) -> Result<Vec<ProviderRow>, String> {
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare(
                "SELECT id, kind, name, url, model, key, voice, speed, extra, updated FROM providers ORDER BY kind, id",
            )
            .map_err(sql)?;
        let rows = st
            .query_map([], |r| {
                let extra: String = r.get(8)?;
                Ok(ProviderRow {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    name: r.get(2)?,
                    url: r.get(3)?,
                    model: r.get(4)?,
                    key: r.get(5)?,
                    voice: r.get(6)?,
                    speed: r.get(7)?,
                    extra: serde_json::from_str(&extra).unwrap_or(json!({})),
                    updated: r.get(9)?,
                })
            })
            .map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }

    /// A new one (`p.id` 0) → its id; or an existing one written over.
    pub fn save_provider(&self, p: &ProviderRow) -> Result<i64, String> {
        let db = self.db.lock().unwrap();
        if p.id > 0 {
            let n = db.execute("UPDATE providers SET kind = ?1, name = ?2, url = ?3, model = ?4, key = ?5, voice = ?6, speed = ?7, extra = ?8, updated = ?9
                                WHERE id = ?10", params![p.kind, p.name, p.url, p.model, p.key, p.voice, p.speed, p.extra.to_string(), p.updated, p.id]).map_err(sql)?;
            if n == 0 {
                return Err(format!("нет подключения {}", p.id));
            }
            return Ok(p.id);
        }
        db.execute("INSERT INTO providers (kind, name, url, model, key, voice, speed, extra, updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                   params![p.kind, p.name, p.url, p.model, p.key, p.voice, p.speed, p.extra.to_string(), p.updated]).map_err(sql)?;
        Ok(db.last_insert_rowid())
    }

    pub fn delete_provider(&self, id: i64) -> Result<bool, String> {
        let db = self.db.lock().unwrap();
        Ok(db.execute("DELETE FROM providers WHERE id = ?", [id]).map_err(sql)? > 0)
    }

    // ------------------------------------------------------------ wakes (wakes.rs)
    pub fn add_wake(&self, w: &Value) -> Result<i64, String> {
        let db = self.db.lock().unwrap();
        let f = |k: &str| w[k].as_f64();
        db.execute("INSERT INTO wakes (t, device, p, barge, button, pre, seconds, file, quiet_before) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                   params![f("t"), w["device"].as_str(), f("p"), w["barge"].as_bool().unwrap_or(false) as i64, w["button"].as_bool().unwrap_or(false) as i64,
                           f("pre"), f("seconds"), w["file"].as_str(), f("quiet_before")]).map_err(sql)?;
        Ok(db.last_insert_rowid())
    }

    /// One field of a wake: heard_window, heard (the command), label ('me' | 'not' | null).
    pub fn set_wake(&self, id: i64, field: &str, value: Option<&str>) -> Result<bool, String> {
        let col = match field {
            "heard_window" | "heard" | "label" => field,
            _ => return Err(format!("{field}: нет такого поля")),
        };
        let db = self.db.lock().unwrap();
        Ok(db.execute(&format!("UPDATE wakes SET {col} = ? WHERE id = ?"), params![value, id]).map_err(sql)? > 0)
    }

    pub fn wake(&self, id: i64) -> Option<Value> {
        let db = self.db.lock().unwrap();
        db.query_row(&format!("{WAKE_SELECT} WHERE id = ?"), [id], wake_row).optional().ok()?
    }

    /// Newest first; `label`: "me", "not", "none" (not yet marked), or all.
    pub fn wakes(
        &self,
        device: Option<&str>,
        label: Option<&str>,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<Value>, String> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare(&format!("{WAKE_SELECT} WHERE (?1 IS NULL OR device = ?1) AND id < ?2
                                          AND (?3 IS NULL OR (?3 = 'none' AND label IS NULL) OR label = ?3) ORDER BY id DESC LIMIT ?4")).map_err(sql)?;
        let rows =
            st.query_map(params![device, before.unwrap_or(i64::MAX), label, limit as i64], wake_row).map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }

    /// {all, me, not, none} for a device or everyone.
    pub fn wake_counts(&self, device: Option<&str>) -> Value {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT COUNT(*), SUM(label = 'me'), SUM(label = 'not'), SUM(label IS NULL) FROM wakes WHERE ?1 IS NULL OR device = ?1", params![device],
                     |r| Ok(json!({"all": r.get::<_, i64>(0)?, "me": r.get::<_, Option<i64>>(1)?.unwrap_or(0), "not": r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                                   "none": r.get::<_, Option<i64>>(3)?.unwrap_or(0)}))).unwrap_or(json!({}))
    }

    /// → its file, to be deleted too.
    pub fn delete_wake(&self, id: i64) -> Result<Option<String>, String> {
        let db = self.db.lock().unwrap();
        let file: Option<String> =
            db.query_row("SELECT file FROM wakes WHERE id = ?", [id], |r| r.get(0)).optional().map_err(sql)?;
        db.execute("DELETE FROM wakes WHERE id = ?", [id]).map_err(sql)?;
        Ok(file)
    }

    // ------------------------------------------------------------ alarms
    pub fn alarms(&self) -> Result<Vec<AlarmRow>, String> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare("SELECT id, device, minute, days, label, enabled, sound, ring, volume, ramp, armed, rang FROM alarms ORDER BY minute, id").map_err(sql)?;
        let rows = st
            .query_map([], |r| {
                Ok(AlarmRow {
                    id: r.get(0)?,
                    device: r.get(1)?,
                    minute: r.get(2)?,
                    days: r.get(3)?,
                    label: r.get(4)?,
                    enabled: r.get::<_, i64>(5)? != 0,
                    sound: r.get(6)?,
                    ring: r.get(7)?,
                    volume: r.get(8)?,
                    ramp: r.get(9)?,
                    armed: r.get(10)?,
                    rang: r.get(11)?,
                })
            })
            .map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }

    /// A new one (`a.id` ignored) → its id; or an existing one written over.
    pub fn save_alarm(&self, a: &AlarmRow) -> Result<i64, String> {
        let db = self.db.lock().unwrap();
        let p = params![
            a.device,
            a.minute,
            a.days,
            a.label,
            a.enabled as i64,
            a.sound,
            a.ring,
            a.volume,
            a.ramp,
            a.armed,
            a.rang
        ];
        if a.id > 0 {
            let n = db.execute("UPDATE alarms SET device = ?1, minute = ?2, days = ?3, label = ?4, enabled = ?5, sound = ?6, ring = ?7, volume = ?8,
                                ramp = ?9, armed = ?10, rang = ?11 WHERE id = ?12",
                               params![a.device, a.minute, a.days, a.label, a.enabled as i64, a.sound, a.ring, a.volume, a.ramp, a.armed, a.rang, a.id]).map_err(sql)?;
            if n == 0 {
                return Err(format!("нет будильника {}", a.id));
            }
            return Ok(a.id);
        }
        db.execute(
            "INSERT INTO alarms (device, minute, days, label, enabled, sound, ring, volume, ramp, armed, rang)
                    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            p,
        )
        .map_err(sql)?;
        Ok(db.last_insert_rowid())
    }

    /// → whether it was there.
    pub fn delete_alarm(&self, id: i64) -> Result<bool, String> {
        let db = self.db.lock().unwrap();
        Ok(db.execute("DELETE FROM alarms WHERE id = ?", [id]).map_err(sql)? > 0)
    }

    /// A sound renamed or deleted: the alarms that had it follow (None — the alarms' default sound).
    pub fn alarm_sound_replaced(&self, from: &str, to: Option<&str>) -> Result<usize, String> {
        let db = self.db.lock().unwrap();
        db.execute("UPDATE alarms SET sound = ? WHERE sound = ?", params![to, from]).map_err(sql)
    }

    // ------------------------------------------------------------ analytics
    /// The exchanges per day (the home's) of the last `days` days, a device's or everyone's, oldest first; a day with
    /// none is left out (the UI fills the gaps).
    pub fn stats(&self, device: Option<&str>, since: f64) -> Result<Vec<Value>, String> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare("SELECT local_day(t) AS day, COUNT(*), SUM(cut), SUM(ring IS NOT NULL),
                                        SUM(text = ''), AVG(asr_s), AVG(llm_s), AVG(from_hit_s), group_concat(calls, char(30))
                                 FROM exchanges WHERE (?1 IS NULL OR device = ?1) AND t >= ?2 GROUP BY day ORDER BY day").map_err(sql)?;
        let rows = st.query_map(params![device, since], |r| {
            let calls: Option<String> = r.get(8)?;
            let mut tools: std::collections::BTreeMap<String, i64> = Default::default();
            for c in calls.unwrap_or_default().split('\u{1e}') {
                if let Ok(Value::Array(a)) = serde_json::from_str::<Value>(c) {
                    for x in a {
                        if let Some(n) = x[0].as_str() {
                            *tools.entry(n.to_string()).or_default() += 1;
                        }
                    }
                }
            }
            Ok(json!({"day": r.get::<_, String>(0)?, "requests": r.get::<_, i64>(1)?, "cut": r.get::<_, i64>(2)?, "ring_stopped": r.get::<_, i64>(3)?,
                      "unheard": r.get::<_, i64>(4)?, "asr_s": r.get::<_, Option<f64>>(5)?, "llm_s": r.get::<_, Option<f64>>(6)?,
                      "from_hit_s": r.get::<_, Option<f64>>(7)?, "tools": tools}))
        }).map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }

    /// Exchanges per device since `since`: (device, count), most first.
    pub fn by_device(&self, since: f64) -> Result<Vec<(String, i64)>, String> {
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare("SELECT device, COUNT(*) FROM exchanges WHERE t >= ? GROUP BY device ORDER BY 2 DESC")
            .map_err(sql)?;
        let rows = st.query_map([since], |r| Ok((r.get(0)?, r.get(1)?))).map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }

    /// Exchanges by the hour of the home's day, 24 counts.
    pub fn hours(&self, device: Option<&str>, since: f64) -> Result<Vec<i64>, String> {
        let db = self.db.lock().unwrap();
        let mut st = db
            .prepare(
                "SELECT local_hour(t), COUNT(*) FROM exchanges
                                 WHERE (?1 IS NULL OR device = ?1) AND t >= ?2 GROUP BY 1",
            )
            .map_err(sql)?;
        let mut h = vec![0i64; 24];
        let rows =
            st.query_map(params![device, since], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(sql)?;
        for r in rows {
            let (k, n) = r.map_err(sql)?;
            if (0..24).contains(&k) {
                h[k as usize] = n;
            }
        }
        Ok(h)
    }
}

const SELECT: &str =
    "SELECT id, t, device, heard, text, answer, calls, ring, cut, asr_s, llm_s, from_hit_s, model FROM exchanges";
const WAKE_SELECT: &str =
    "SELECT id, t, device, p, barge, button, pre, seconds, file, quiet_before, heard_window, heard, label FROM wakes";

fn wake_row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(
        json!({"id": r.get::<_, i64>(0)?, "t": r.get::<_, f64>(1)?, "device": r.get::<_, String>(2)?, "p": r.get::<_, f64>(3)?,
              "barge": r.get::<_, i64>(4)? != 0, "button": r.get::<_, i64>(5)? != 0, "pre": r.get::<_, f64>(6)?, "seconds": r.get::<_, f64>(7)?,
              "file": r.get::<_, String>(8)?, "quiet_before": r.get::<_, Option<f64>>(9)?, "heard_window": r.get::<_, Option<String>>(10)?,
              "heard": r.get::<_, Option<String>>(11)?, "label": r.get::<_, Option<String>>(12)?}),
    )
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let calls: String = r.get(6)?;
    Ok(
        json!({"id": r.get::<_, i64>(0)?, "t": r.get::<_, f64>(1)?, "device": r.get::<_, String>(2)?, "heard": r.get::<_, String>(3)?,
              "text": r.get::<_, String>(4)?, "answer": r.get::<_, String>(5)?, "calls": serde_json::from_str::<Value>(&calls).unwrap_or(json!([])),
              "ring": r.get::<_, Option<String>>(7)?, "cut": r.get::<_, i64>(8)? != 0, "asr_s": r.get::<_, Option<f64>>(9)?,
              "llm_s": r.get::<_, Option<f64>>(10)?, "from_hit_s": r.get::<_, Option<f64>>(11)?, "model": r.get::<_, Option<String>>(12)?}),
    )
}

/// A job done once (the history brought in, the music service's journal, a clean-up by a rule's version) has its mark in
/// `meta`; None — not done yet. On a connection, so a transaction (`&tx`) puts the job and its mark in together.
fn seen(db: &Connection, device: &str, t: f64) -> Result<(), String> {
    db.execute(
        "INSERT INTO devices (id, first_seen, last_seen) VALUES (?1, ?2, ?2)
                ON CONFLICT(id) DO UPDATE SET first_seen = min(first_seen, ?2), last_seen = max(last_seen, ?2)",
        params![device, t],
    )
    .map_err(sql)?;
    Ok(())
}

fn insert(db: &Connection, v: &Value, t: f64) -> Result<i64, String> {
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    let device = s("device");
    if device.is_empty() {
        return Err("an exchange without a device".into());
    }
    let calls = if v["calls"].is_array() { v["calls"].to_string() } else { "[]".into() };
    db.execute(
        "INSERT INTO exchanges (t, device, heard, text, answer, calls, ring, cut, asr_s, llm_s, from_hit_s, model)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            t,
            device,
            s("heard"),
            s("text"),
            s("answer"),
            calls,
            v["ring"].as_str(),
            v["cut"].as_bool().unwrap_or(false) as i64,
            num(&v["asr_s"]),
            num(&v["llm_s"]),
            num(&v["from_hit_s"]),
            v["model"].as_str()
        ],
    )
    .map_err(sql)?;
    let id = db.last_insert_rowid();
    seen(db, &device, t)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> (std::path::PathBuf, String) {
        let d = std::env::temp_dir().join(format!("server-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let p = d.join("sub").join("server.db").to_string_lossy().to_string();
        (d, p)
    }

    #[test]
    fn settings_devices_and_pages() {
        let (d, p) = tmp("pages");
        let s = Store::open(&p).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(d.join("sub")).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(s.setting("timer"), None);
        s.set_setting("timer", &json!({"sound": "chime", "ring": 60}), 1.0).unwrap();
        s.set_setting("timer", &json!({"sound": "bell.mp3", "ring": 90}), 2.0).unwrap();
        assert_eq!(s.setting("timer").unwrap()["ring"], 90);
        for i in 0..5 {
            let dev = if i % 2 == 0 { "kitchen" } else { "bedroom" };
            let (id, r) = s.add_exchange(&json!({"device": dev, "heard": format!("Мразота, раз {i}"), "text": format!("раз {i}"),
                                                 "answer": "Да.", "calls": [["set_timer", {"seconds": 5}, {"ok": true}]], "asr_s": 0.25,
                                                 "llm_s": null, "cut": i == 4}), 100.0 + i as f64).unwrap();
            assert_eq!(
                (r["id"].as_i64(), r["device"].as_str(), r["calls"][0][0].as_str()),
                (Some(id), Some(dev), Some("set_timer"))
            );
            assert_eq!(
                (r["asr_s"].as_f64(), r["llm_s"].is_null(), r["cut"].as_bool()),
                (Some(0.25), true, Some(i == 4))
            );
        }
        // kitchen: 0, 2, 4 — newest first, paged
        let page = s.exchanges(Some("kitchen"), None, 2).unwrap();
        assert_eq!(page.iter().map(|r| r["text"].as_str().unwrap()).collect::<Vec<_>>(), ["раз 4", "раз 2"]);
        let rest = s.exchanges(Some("kitchen"), page[1]["id"].as_i64(), 2).unwrap();
        assert_eq!(rest.iter().map(|r| r["text"].as_str().unwrap()).collect::<Vec<_>>(), ["раз 0"]);
        assert_eq!(s.exchanges(None, None, 100).unwrap().len(), 5);
        assert_eq!((s.count(Some("bedroom")), s.count(None)), (2, 5));
        // the devices: seen by their exchanges; named
        let ds = s.devices().unwrap();
        assert_eq!(
            ds.iter().map(|d| (d.id.as_str(), d.first_seen, d.last_seen)).collect::<Vec<_>>(),
            [("kitchen", 100.0, 104.0), ("bedroom", 101.0, 103.0)]
        );
        assert!(s.rename("kitchen", "Кухня").unwrap());
        assert!(!s.rename("nowhere", "x").unwrap());
        s.seen("kitchen", 50.0).unwrap();
        let k = s.devices().unwrap().into_iter().find(|d| d.id == "kitchen").unwrap();
        assert_eq!((k.name.as_str(), k.first_seen, k.last_seen), ("Кухня", 50.0, 104.0));
        assert!(s.add_exchange(&json!({"heard": "x"}), 1.0).is_err(), "no device, no exchange");
        // opened again: the same, the schema not re-run
        drop(s);
        let s = Store::open(&p).unwrap();
        assert_eq!(s.count(None), 5);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn alarms_and_stats() {
        let (d, p) = tmp("alarms");
        let s = Store::open(&p).unwrap();
        let mut a = AlarmRow {
            id: 0,
            device: "kitchen".into(),
            minute: 450,
            days: 0x1f,
            label: "работа".into(),
            enabled: true,
            sound: Some("bell.mp3".into()),
            ring: None,
            volume: Some(5.0),
            ramp: Some(30.0),
            armed: 1.0,
            rang: None,
        };
        a.id = s.save_alarm(&a).unwrap();
        let b = s.save_alarm(&AlarmRow { id: 0, minute: 420, sound: None, ..a.clone() }).unwrap();
        assert_eq!(s.alarms().unwrap().iter().map(|x| x.id).collect::<Vec<_>>(), [b, a.id], "by time of day");
        a.rang = Some(100.0);
        a.enabled = false;
        s.save_alarm(&a).unwrap();
        assert_eq!(s.alarms().unwrap()[1], a);
        assert_eq!(s.alarm_sound_replaced("bell.mp3", Some("gong.ogg")).unwrap(), 1);
        assert_eq!(s.alarms().unwrap()[1].sound.as_deref(), Some("gong.ogg"));
        assert_eq!(s.alarm_sound_replaced("gong.ogg", None).unwrap(), 1);
        assert!(s.delete_alarm(b).unwrap() && !s.delete_alarm(b).unwrap());
        assert!(s.save_alarm(&AlarmRow { id: 999, ..a.clone() }).is_err(), "no such alarm to write over");
        // analytics: two days, Moscow time (21:30 UTC is the next day there); t0 is 2026-11-06 23:30 in Moscow
        let t0 = 1_793_997_000.0;
        for (i, (dt, dev, calls, cut)) in [
            (0.0, "kitchen", json!([["set_timer", {}, {}]]), false),
            (60.0, "kitchen", json!([["set_timer", {}, {}], ["play_music", {}, {}]]), true),
            (3600.0, "bedroom", json!([]), false),
        ]
        .into_iter()
        .enumerate()
        {
            s.add_exchange(&json!({"device": dev, "text": if i == 2 { "" } else { "x" }, "answer": "y", "calls": calls, "cut": cut, "llm_s": 2.0 + i as f64}), t0 + dt).unwrap();
        }
        let st = s.stats(None, 0.0).unwrap();
        assert_eq!(
            st.iter().map(|d| (d["day"].as_str().unwrap(), d["requests"].as_i64().unwrap())).collect::<Vec<_>>(),
            [("2026-11-06", 2), ("2026-11-07", 1)]
        );
        assert_eq!(
            (
                st[0]["cut"].as_i64(),
                st[0]["tools"]["set_timer"].as_i64(),
                st[0]["llm_s"].as_f64(),
                st[1]["unheard"].as_i64()
            ),
            (Some(1), Some(2), Some(2.5), Some(1))
        );
        assert_eq!(s.stats(Some("bedroom"), 0.0).unwrap().len(), 1);
        assert_eq!(s.stats(None, t0 + 1000.0).unwrap().len(), 1, "since");
        let h = s.hours(None, 0.0).unwrap();
        assert_eq!((h[23], h[0], h.iter().sum::<i64>()), (2, 1, 3));
        assert_eq!(s.by_device(0.0).unwrap(), [("kitchen".to_string(), 2), ("bedroom".to_string(), 1)]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn settings_kept() {
        let (d, p) = tmp("marks");
        let s = Store::open(&p).unwrap();
        // none kept, or not readable: told apart
        assert_eq!(s.setting_kept("timer"), Ok(None));
        s.db.lock()
            .unwrap()
            .execute("INSERT INTO settings (key, value, t) VALUES ('alarm', '{\"sound\": ', 1)", [])
            .unwrap();
        assert!(s.setting_kept("alarm").unwrap_err().contains("not JSON"));
        assert_eq!(s.setting("alarm"), None);
        // several at once: all or none
        s.set_settings(&[("timer", json!({"ring": 90})), ("preferences", json!({"text": "x"}))], 2.0).unwrap();
        assert_eq!(
            (s.setting("timer"), s.setting("preferences")),
            (Some(json!({"ring": 90})), Some(json!({"text": "x"})))
        );
        s.db.lock().unwrap().execute_batch("CREATE TRIGGER no_alarm BEFORE UPDATE ON settings WHEN NEW.key = 'alarm' BEGIN SELECT RAISE(ABORT, 'no'); END").unwrap();
        assert!(s.set_settings(&[("timer", json!({"ring": 30})), ("alarm", json!({}))], 3.0).is_err());
        assert_eq!(s.setting("timer"), Some(json!({"ring": 90})), "the timer's went back with the alarm's");
        let _ = std::fs::remove_dir_all(&d);
    }
}
