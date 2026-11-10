//! Timers: kept in a file, so a restart of the server does not lose them. One that came due while the server was down
//! rings as soon as the device's station is back; one more than LOST s late is dropped. What is pending goes into the
//! last line's stamp when the request is about timers (tools/), so the model answers from it instead of guessing.
//! A timer rings with a sound, not the voice (ring.rs: the settings' timer sound, looped for its `ring` s, stopped by the
//! wake word, a touch or the web UI); a file of the user's is decoded AHEAD s before, so the ring starts on the second.
//! The settings are read when the timer is due: a change in the web UI counts for the timers already set.
//! The list changed — an event (`timers`) for the web UI, which can also cancel one.
use crate::events;
use crate::ring;
use crate::session::{log, Shared};
use crate::sounds;
use crate::text::{now_s, tz};
use chrono::TimeZone;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const LOST: f64 = 600.0; // s: a timer this late is no use any more
const LATE: f64 = 5.0;
const AHEAD: f64 = 3.0;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Timer {
    pub id: u64,
    pub device: String,
    pub label: String,
    pub seconds: u64,
    pub at: f64,
}

impl Timer {
    /// For the stamp: «на 10 мин (чай) — зазвонит в 03:28:47, через 9 мин 12 с»
    pub fn describe(&self, now: f64) -> String {
        let at = tz().timestamp_opt(self.at as i64, 0).single().unwrap().format("%H:%M:%S");
        let left = (self.at - now).max(0.0) as u64;
        format!(
            "на {}{} — зазвонит в {at}, через {}",
            span(self.seconds),
            if self.label.is_empty() { String::new() } else { format!(" ({})", self.label) },
            span(left)
        )
    }
}

fn span(s: u64) -> String {
    let (h, m, sec) = (s / 3600, s % 3600 / 60, s % 60);
    let mut out = vec![];
    if h > 0 {
        out.push(format!("{h} ч"));
    }
    if m > 0 {
        out.push(format!("{m} мин"));
    }
    if sec > 0 || out.is_empty() {
        out.push(format!("{sec} с"));
    }
    out.join(" ")
}

pub struct Timers {
    path: Option<String>,
    list: Mutex<Vec<Timer>>,
    ids: AtomicU64,
}

impl Timers {
    /// The pending timers from `path` (none if it is missing); None — in memory only (tests).
    pub fn load(path: Option<&str>) -> Arc<Timers> {
        let list: Vec<Timer> = path
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let next = list.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        Arc::new(Timers { path: path.map(String::from), list: Mutex::new(list), ids: AtomicU64::new(next) })
    }

    pub fn all(&self) -> Vec<Timer> {
        self.list.lock().unwrap().clone()
    }

    pub fn pending(&self, device: &str) -> Vec<Timer> {
        let mut v: Vec<Timer> = self.list.lock().unwrap().iter().filter(|t| t.device == device).cloned().collect();
        v.sort_by(|a, b| a.at.total_cmp(&b.at));
        v
    }

    pub fn add(&self, device: &str, seconds: u64, label: &str) -> Timer {
        let t = Timer {
            id: self.ids.fetch_add(1, Ordering::SeqCst),
            device: device.into(),
            label: label.into(),
            seconds,
            at: now_s() + seconds as f64,
        };
        self.edit(|l| {
            l.push(t.clone());
            true
        });
        t
    }

    /// → whether it was there.
    pub fn remove(&self, id: u64) -> bool {
        self.edit(|l| {
            let n = l.len();
            l.retain(|t| t.id != id);
            l.len() < n
        })
    }

    /// Still pending (not cancelled, not rung).
    pub fn has(&self, id: u64) -> bool {
        self.list.lock().unwrap().iter().any(|t| t.id == id)
    }

    /// A change (`f` → whether it changed anything), saved under the same lock: the file is written in the order of
    /// the changes (written after the lock, through one shared `.tmp`, two changes at once could leave the older list
    /// in the file).
    fn edit(&self, f: impl FnOnce(&mut Vec<Timer>) -> bool) -> bool {
        let changed = {
            let mut l = self.list.lock().unwrap();
            let changed = f(&mut l);
            if changed {
                self.save(&l);
            }
            changed
        };
        if changed {
            self.changed();
        }
        changed
    }

    /// All pending, soonest first, for the web UI.
    pub fn json(&self) -> serde_json::Value {
        let mut v = self.all();
        v.sort_by(|a, b| a.at.total_cmp(&b.at));
        serde_json::json!(v)
    }

    fn changed(&self) {
        events::emit("timers", self.json());
    }

    /// Atomically (a temporary file, then rename), 600. Under `list`'s lock (`edit`). The write waits for the disk
    /// (fsync): on the server's runtime its other tasks move to another worker meanwhile.
    fn save(&self, list: &[Timer]) {
        let Some(p) = &self.path else { return };
        let body = serde_json::to_string_pretty(list).unwrap_or_else(|_| "[]".into());
        let tmp = format!("{p}.tmp");
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&tmp)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, p)
        };
        let multi = tokio::runtime::Handle::try_current()
            .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread);
        let w = if multi { tokio::task::block_in_place(write) } else { write() };
        if let Err(e) = w {
            log("server", format!("timers: {p}: {e}"));
        }
    }
}

/// Rings `t` when it is due (on the device's station, waiting for it to connect), then forgets it.
pub fn schedule(shared: Arc<Shared>, t: Timer) {
    tokio::spawn(async move {
        let wait = t.at - now_s();
        if wait > AHEAD {
            tokio::time::sleep(Duration::from_secs_f64(wait - AHEAD)).await;
        }
        if !shared.timers.has(t.id) {
            return; // cancelled: its sound is not decoded for nothing
        }
        let set = shared.settings.timer();
        let sound = sounds::load_or_chime(&shared.sounds_dir, &set.sound, &t.device).await;
        let wait = t.at - now_s();
        if wait > 0.0 {
            tokio::time::sleep(Duration::from_secs_f64(wait)).await;
        }
        if !shared.timers.has(t.id) {
            return; // cancelled meanwhile
        }
        let late = now_s() - t.at;
        if late > LOST {
            log(&t.device, format!("   (a timer dropped: {late:.0} s late)"));
            shared.timers.remove(t.id);
            return;
        }
        let what = format!(
            "таймер на {}{}",
            span(t.seconds),
            if t.label.is_empty() { String::new() } else { format!(" ({})", t.label) }
        );
        if late > LATE {
            log(&t.device, format!("   (a timer rings {late:.0} s late: the server was down)"));
        }
        let (timers, id) = (shared.timers.clone(), t.id);
        // forgotten once it sounds: a restart before that (the station away) still rings it
        ring::ring(
            &shared,
            &t.device,
            sound,
            shared.settings.timer().ring,
            (ring::Kind::Timer, &what),
            t.at + LOST,
            move || {
                timers.remove(id);
            },
        )
        .await;
    });
}

/// At the server's start: everything pending from the file is scheduled again (overdue ones ring once the station is back).
pub fn resume(shared: &Arc<Shared>) {
    for t in shared.timers.all() {
        log(&t.device, format!("   (a timer kept over the restart: due in {:.0} s)", (t.at - now_s()).max(0.0)));
        schedule(shared.clone(), t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans() {
        assert_eq!(span(10), "10 с");
        assert_eq!(span(600), "10 мин");
        assert_eq!(span(5400), "1 ч 30 мин");
        assert_eq!(span(3725), "1 ч 2 мин 5 с");
        assert_eq!(span(0), "0 с");
    }

    #[test]
    fn kept_in_a_file() {
        let d = std::env::temp_dir().join(format!("server-timers-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("timers.json").to_string_lossy().to_string();
        let t = Timers::load(Some(&p));
        let a = t.add("kitchen", 600, "чай");
        t.add("bedroom", 60, "");
        let again = Timers::load(Some(&p));
        assert_eq!(again.all().len(), 2);
        assert_eq!(again.pending("kitchen")[0].label, "чай");
        assert!(again.pending("kitchen")[0].describe(a.at - 30.0).starts_with("на 10 мин (чай) — зазвонит в "));
        again.remove(a.id);
        assert_eq!(Timers::load(Some(&p)).all().len(), 1);
        assert!(Timers::load(Some(&p)).add("x", 1, "").id > a.id, "ids go on after a restart");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn changes_at_once_saved_in_order() {
        // set and cancelled from many places at once: the file ends as the list ends, not as an older snapshot
        let d = std::env::temp_dir().join(format!("server-timers-par-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("timers.json").to_string_lossy().to_string();
        let t = Timers::load(Some(&p));
        let ids = |t: &Timers| {
            let mut v: Vec<u64> = t.all().iter().map(|x| x.id).collect();
            v.sort();
            v
        };
        let (n, rounds) = (8, 40);
        let go = std::sync::Barrier::new(n);
        let stale = AtomicU64::new(0);
        std::thread::scope(|s| {
            for k in 0..n {
                let (t, go, stale, p, ids) = (&t, &go, &stale, &p, &ids);
                s.spawn(move || {
                    for i in 0..rounds {
                        go.wait();
                        let x = t.add(&format!("d{k}"), 60 + i, "");
                        if i % 3 == 0 {
                            assert!(t.remove(x.id));
                        }
                        // all of this round done: the file must hold all of it
                        if go.wait().is_leader() && ids(&Timers::load(Some(p))) != ids(t) {
                            stale.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                });
            }
        });
        assert_eq!(stale.load(Ordering::SeqCst), 0, "rounds that left an older list in the file");
        assert_eq!(ids(&t).len(), n * (rounds as usize - 14));
        assert_eq!(ids(&Timers::load(Some(&p))), ids(&t), "the file is the list");
        assert!(!std::path::Path::new(&format!("{p}.tmp")).exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
