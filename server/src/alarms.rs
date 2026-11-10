//! Alarms: a time of day on a device, once or on chosen weekdays (the home's time), each with its sound, how long it rings,
//! its volume and how it rises — or the alarm settings' defaults (settings.rs). Kept in server.db, rung by the server the way
//! a timer rings (ring.rs: the wake word or a touch stops it).
//! - When it is due: the next time of day on an allowed day after it was set (`armed`) or last rang; once — then it is
//!   switched off (kept in the list, as a phone keeps it).
//! - A due one is looked at every second; its sound is decoded AHEAD s before. Down when it was due (a restart): it rings
//!   late if that is under LOST s, else it is skipped (logged), as a timer.
//! - The list changed — an event (`alarms`) for the web UI.
use crate::events;
use crate::ring::{self, Opts};
use crate::session::{log, Shared};
use crate::sounds;
use crate::store::AlarmRow;
use crate::text::{now_s, tz};
use chrono::{Datelike, Duration as Days, NaiveTime, TimeZone};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const AHEAD: f64 = 3.0;
const LOST: f64 = 600.0;

/// The first time after `after` at `minute` (of the home's day) on an allowed weekday (`days`: bit 0 = Monday … bit 6 =
/// Sunday; 0 — any day: once).
pub fn next_due(minute: u32, days: u8, after: f64) -> f64 {
    let start = tz().timestamp_opt(after.floor() as i64, 0).single().unwrap().date_naive();
    let at = NaiveTime::from_hms_opt(minute / 60, minute % 60, 0).unwrap();
    for k in 0..8 {
        let d = start + Days::days(k);
        let allowed = days == 0 || days & (1 << d.weekday().num_days_from_monday()) != 0;
        let t = crate::text::local(&d.and_time(at)).timestamp() as f64;
        if allowed && t > after {
            return t;
        }
    }
    unreachable!("a week has every weekday")
}

/// When it rings next (None: switched off).
pub fn due(a: &AlarmRow) -> Option<f64> {
    a.enabled.then(|| next_due(a.minute, a.days, a.armed.max(a.rang.unwrap_or(0.0))))
}

/// «07:30»
pub fn time(minute: u32) -> String {
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

/// For the web UI: days as 1 (Monday) … 7.
pub fn json(a: &AlarmRow) -> Value {
    json!({"id": a.id, "device": a.device, "time": time(a.minute), "days": (0..7).filter(|i| a.days & (1 << i) != 0).map(|i| i + 1).collect::<Vec<_>>(),
           "label": a.label, "enabled": a.enabled, "sound": a.sound, "ring": a.ring, "volume": a.volume, "ramp": a.ramp,
           "next": due(a), "rang": a.rang})
}

pub fn list(shared: &Shared) -> Value {
    let mut v = shared.store.alarms().unwrap_or_default();
    v.sort_by(|a, b| due(a).unwrap_or(f64::MAX).total_cmp(&due(b).unwrap_or(f64::MAX)).then(a.minute.cmp(&b.minute)));
    json!(v.iter().map(json).collect::<Vec<_>>())
}

pub fn changed(shared: &Shared) {
    events::emit("alarms", list(shared));
}

/// Every second: what is due within AHEAD s gets its sound ready and rings on time; what was due more than LOST s ago is
/// skipped.
pub async fn run(shared: Arc<Shared>) {
    let store = shared.store.clone();
    let taken: Arc<Mutex<HashSet<(i64, i64)>>> = Default::default(); // (alarm, due) being rung
    loop {
        let now = now_s();
        for a in store.alarms().unwrap_or_default() {
            let Some(at) = due(&a) else { continue };
            if at - now > AHEAD || !taken.lock().unwrap().insert((a.id, at as i64)) {
                continue;
            }
            if now - at > LOST {
                log(&a.device, format!("   (the alarm for {} dropped: {:.0} s late)", time(a.minute), now - at));
                rang(&shared, &a, at);
                continue;
            }
            tokio::spawn(sound_it(shared.clone(), a, at, taken.clone()));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// It rang (or was skipped) at `at`: noted; once — switched off.
fn rang(shared: &Shared, a: &AlarmRow, at: f64) {
    let store = &shared.store;
    // the newest of it: the user may have changed it meanwhile
    let Some(mut cur) = store.alarms().unwrap_or_default().into_iter().find(|x| x.id == a.id) else { return };
    cur.rang = Some(at);
    if cur.days == 0 {
        cur.enabled = false;
    }
    if let Err(e) = store.save_alarm(&cur) {
        log(&a.device, format!("   (alarm: {e})"));
    }
    changed(shared);
}

async fn sound_it(shared: Arc<Shared>, a: AlarmRow, at: f64, taken: Arc<Mutex<HashSet<(i64, i64)>>>) {
    let id = a.id;
    let set = shared.settings.alarm();
    let name = a.sound.clone().unwrap_or(set.sound.clone());
    let sound = sounds::load_or_chime(&shared.sounds_dir, &name, &a.device).await;
    let wait = at - now_s();
    if wait > 0.0 {
        tokio::time::sleep(Duration::from_secs_f64(wait)).await;
    }
    // changed or deleted meanwhile: it is not this one any more
    let still = shared.store.alarms().ok().and_then(|v| v.into_iter().find(|x| x.id == a.id));
    if still.as_ref().and_then(due) != Some(at) {
        taken.lock().unwrap().remove(&(id, at as i64));
        return;
    }
    let late = now_s() - at;
    let what = format!(
        "будильник {}{}",
        time(a.minute),
        if a.label.is_empty() { String::new() } else { format!(" ({})", a.label) }
    );
    if late > 5.0 {
        log(&a.device, format!("   (the alarm for {} rings {late:.0} s late: the server was down)", time(a.minute)));
    }
    let opts = Opts {
        ramp: a.ramp.unwrap_or(set.ramp),
        volume: a.volume.or(set.volume).map(|l| (l * 25.5).round_ties_even() as u8),
    };
    let seconds = a.ring.unwrap_or(set.ring);
    let (sh, device) = (shared.clone(), a.device.clone());
    ring::ring_with(&shared, &device, sound, seconds, (ring::Kind::Alarm, &what), at + LOST, opts, move || {
        rang(&sh, &a, at)
    })
    .await;
    taken.lock().unwrap().remove(&(id, at as i64));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msk_t(s: &str) -> f64 {
        tz().from_local_datetime(&chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap())
            .single()
            .unwrap()
            .timestamp() as f64
    }

    #[test]
    fn when_it_rings() {
        // 2026-11-07 is a Saturday
        let sat_noon = msk_t("2026-11-07 12:00");
        assert_eq!(
            next_due(7 * 60 + 30, 0, sat_noon),
            msk_t("2026-11-08 07:30"),
            "once: tomorrow, the time has passed today"
        );
        assert_eq!(next_due(13 * 60, 0, sat_noon), msk_t("2026-11-07 13:00"), "once: later today");
        assert_eq!(next_due(7 * 60, 0x1f, sat_noon), msk_t("2026-11-09 07:00"), "weekdays: Monday");
        assert_eq!(
            next_due(9 * 60, 0x60, sat_noon),
            msk_t("2026-11-08 09:00"),
            "weekends: Sunday (Saturday's 9:00 is past)"
        );
        let mon_tue_thu_fri = 0b1_1011;
        assert_eq!(
            next_due(7 * 60, mon_tue_thu_fri, msk_t("2026-11-10 07:00")),
            msk_t("2026-11-12 07:00"),
            "Tuesday's rang: Thursday"
        );
        assert_eq!(next_due(0, 0x7f, msk_t("2026-11-07 23:59")), msk_t("2026-11-08 00:00"), "midnight");
        assert_eq!(
            next_due(12 * 60, 1 << 5, sat_noon),
            msk_t("2026-11-14 12:00"),
            "exactly now is not next: a week later"
        );
        // as kept: rang moves it on; switched off — none
        let mut a = AlarmRow {
            id: 1,
            device: "d".into(),
            minute: 7 * 60,
            days: 0x7f,
            label: String::new(),
            enabled: true,
            sound: None,
            ring: None,
            volume: None,
            ramp: None,
            armed: sat_noon,
            rang: None,
        };
        assert_eq!(due(&a), Some(msk_t("2026-11-08 07:00")));
        a.rang = Some(msk_t("2026-11-08 07:00"));
        assert_eq!(due(&a), Some(msk_t("2026-11-09 07:00")));
        a.enabled = false;
        assert_eq!(due(&a), None);
    }

    #[test]
    fn the_time_as_it_is_written() {
        assert_eq!(time(7 * 60 + 5), "07:05");
    }
}
