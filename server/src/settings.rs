//! The settings the web UI changes, applied at once — no restart: whoever needs one reads it when it is needed (a timer
//! reads its sound and how long to ring when it is due). Kept in server.db, a JSON object per section.
//!   timer:       {sound: "chime" | a file in the sounds folder, ring: 3 … 600 s}
//!   alarm:       {sound, ring: 3 … 600 s, ramp: 0 … 300 s (rising from quiet), volume: 0 … 10 by halves | null (as is)}
//!                — what an alarm has unless it sets its own
//!   preferences: {text: up to PREFS_MAX characters, address: "neutral" | "male" | "female"} — how the user wants to be
//!                talked to, in their words, and spoken of: into the system prompt (brain.rs), which itself stays out
//!                of the UI
//!   home:        {city, lat, lon, timezone} — where the home is: the weather when no city is named, and the time zone
//!                everything is said and counted in (IANA name; "" — the server machine's). A city is given by its
//!                name: the geocoder finds where it is and its time zone. Read back with `zone`: the zone in force.
//! A section kept without a field (one added since) — that field's default; with one this server does not know — read
//! all the same; not readable at all — the defaults for now, and what is kept is left as it is.
//! Changes go one at a time (two tabs at once: each is made on what the other left), the sections a change touches
//! written in one transaction.
use crate::brain::{Address, Profile};
use crate::events;
use crate::ring;
use crate::session::log;
use crate::sounds;
use crate::store::Store;
use crate::text::now_s;
use crate::tools::Place;
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::future::Future;
use std::sync::{Arc, Mutex, RwLock};

pub const PREFS_MAX: usize = 2000;
pub const RAMP_MAX: f64 = 300.0;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Timer {
    pub sound: String,
    pub ring: f64,
}

impl Default for Timer {
    fn default() -> Timer {
        Timer { sound: sounds::CHIME.into(), ring: 60.0 }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Alarm {
    pub sound: String,
    pub ring: f64,
    pub ramp: f64,
    pub volume: Option<f64>,
}

impl Default for Alarm {
    fn default() -> Alarm {
        Alarm { sound: sounds::CHIME.into(), ring: 300.0, ramp: 30.0, volume: Some(5.0) }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Preferences {
    pub text: String,
    pub address: Address,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Home {
    /// as the geocoder names it ("": not set)
    pub city: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    /// IANA name ("": the server machine's)
    pub timezone: String,
}

impl Home {
    /// The time zone in force.
    fn zone(&self) -> Tz {
        self.timezone.parse().unwrap_or_else(|_| crate::text::system_tz())
    }
}

#[derive(Clone, Debug, PartialEq)]
struct All {
    timer: Timer,
    alarm: Alarm,
    preferences: Preferences,
    home: Home,
}

impl All {
    fn json(&self) -> Value {
        let mut v =
            json!({"timer": self.timer, "alarm": self.alarm, "preferences": self.preferences, "home": self.home});
        v["home"]["zone"] = json!(self.home.zone().name());
        v
    }

    /// (section, as JSON) — as kept
    fn sections(&self) -> [(&'static str, Value); 4] {
        [
            ("timer", json!(self.timer)),
            ("alarm", json!(self.alarm)),
            ("preferences", json!(self.preferences)),
            ("home", json!(self.home)),
        ]
    }
}

pub struct Settings {
    store: Arc<Store>,
    /// the sounds folder: a sound chosen must be there and play
    dir: String,
    cur: RwLock<All>,
    /// one change at a time, its sound check (an await) and all: two tabs' changes, both made on what was there before
    /// either, would have the later one undo the other
    changing: tokio::sync::Mutex<()>,
    /// one writer at a time — a change's last step, a sound replaced: what it is made on is what it replaces
    writing: Mutex<()>,
}

/// A change refused: what the user is told.
#[derive(Debug, PartialEq)]
pub struct Refused(pub String);

fn refused(s: impl Into<String>) -> Refused {
    Refused(s.into())
}

/// A volume level: 0 … 10 by halves.
pub fn level_ok(l: f64) -> bool {
    (0.0..=10.0).contains(&l) && (l * 2.0).fract() == 0.0
}

impl Settings {
    /// From server.db; a section never kept is the default, written there.
    pub fn load(store: Arc<Store>, sounds_dir: &str) -> Arc<Settings> {
        /// None kept — the default, written there; not readable — the default for now, nothing written: what is kept
        /// stays for a server that can read it.
        fn section<T: serde::de::DeserializeOwned + Serialize + Default>(store: &Store, key: &str) -> T {
            match store.setting_kept(key) {
                Ok(Some(v)) => serde_json::from_value::<T>(v).unwrap_or_else(|e| {
                    log("server", format!("   (settings «{key}»: not understood ({e}): the defaults for now, what is kept is left alone)"));
                    T::default()
                }),
                Ok(None) => {
                    if let Err(e) = store.set_setting(key, &json!(T::default()), now_s()) {
                        log("server", format!("   (settings: {e})"));
                    }
                    T::default()
                }
                Err(e) => {
                    log("server", format!("   (settings «{key}»: not read ({e}): the defaults for now, what is kept is left alone)"));
                    T::default()
                }
            }
        }
        let all = All {
            timer: section(&store, "timer"),
            alarm: section(&store, "alarm"),
            preferences: section(&store, "preferences"),
            home: section(&store, "home"),
        };
        crate::text::set_tz(all.home.zone());
        Arc::new(Settings {
            store,
            dir: sounds_dir.into(),
            cur: RwLock::new(all),
            changing: Default::default(),
            writing: Default::default(),
        })
    }

    pub fn timer(&self) -> Timer {
        self.cur.read().unwrap().timer.clone()
    }

    pub fn alarm(&self) -> Alarm {
        self.cur.read().unwrap().alarm.clone()
    }

    /// What the prompt is told about the home and the user.
    pub fn profile(&self) -> Profile {
        let cur = self.cur.read().unwrap();
        Profile {
            address: cur.preferences.address,
            city: cur.home.city.clone(),
            preferences: cur.preferences.text.clone(),
        }
    }

    /// Where the weather is told for when no city is named (None: not set).
    pub fn place(&self) -> Option<Place> {
        let h = self.cur.read().unwrap().home.clone();
        Some(Place { name: h.city, lat: h.lat?, lon: h.lon?, timezone: h.timezone })
    }

    pub fn all(&self) -> Value {
        self.cur.read().unwrap().json()
    }

    /// A partial change, e.g. {"timer": {"ring": 90}}: all of it or nothing. → all the settings as they are now.
    pub async fn change(&self, patch: &Value) -> Result<Value, Refused> {
        let http = crate::http().build().expect("an HTTP client");
        let patch = &located(patch, |city| {
            let http = http.clone();
            async move { crate::tools::locate(&http, &city).await }
        })
        .await?;
        let dir = self.dir.clone();
        self.change_with(patch, move |name| {
            let dir = dir.clone();
            async move { sounds::load(&dir, &name).await.map(|_| ()) }
        })
        .await
    }

    /// `change`, a new sound checked by `check` (the tests': one that takes its time, as ffmpeg does).
    async fn change_with<F: Future<Output = Result<(), String>>>(
        &self,
        patch: &Value,
        check: impl Fn(String) -> F,
    ) -> Result<Value, Refused> {
        let Some(p) = patch.as_object() else { return Err(refused("нужен объект настроек")) };
        let _one = self.changing.lock().await;
        let old = self.cur.read().unwrap().clone();
        let mut new = old.clone();
        apply(&mut new, p)?;
        // a sound must play: a file that ffmpeg cannot read is refused now, not found out when it is due
        for (was, now) in [(&old.timer.sound, &new.timer.sound), (&old.alarm.sound, &new.alarm.sound)] {
            if was != now {
                check(now.clone()).await.map_err(Refused)?;
            }
        }
        // a sound replaced meanwhile (that does not wait for a change): the change onto what is there now
        self.update(|a| apply(a, p))
    }

    /// A sound is gone (deleted, renamed): `to` instead wherever it was `from`. → whether it was used.
    pub fn sound_replaced(&self, from: &str, to: &str) -> bool {
        let mut used = false;
        let done = self.update(|a| {
            for s in [&mut a.timer.sound, &mut a.alarm.sound] {
                if s == from {
                    *s = to.to_string();
                    used = true;
                }
            }
            Ok(())
        });
        used && done.is_ok()
    }

    /// `f` on the settings as they are now → kept (the sections it changed: one transaction), applied, told.
    fn update(&self, f: impl FnOnce(&mut All) -> Result<(), Refused>) -> Result<Value, Refused> {
        let _w = self.writing.lock().unwrap();
        let old = self.cur.read().unwrap().clone();
        let mut new = old.clone();
        f(&mut new)?;
        if new == old {
            return Ok(new.json());
        }
        let changed: Vec<(&str, Value)> = old
            .sections()
            .into_iter()
            .zip(new.sections())
            .filter(|((_, was), (_, now))| was != now)
            .map(|(_, now)| now)
            .collect();
        self.store.set_settings(&changed, now_s()).map_err(Refused)?;
        if new.timer != old.timer {
            log("server", format!("   (settings: a timer rings «{}» for {} s)", new.timer.sound, new.timer.ring));
        }
        if new.alarm != old.alarm {
            log(
                "server",
                format!(
                    "   (settings: an alarm rings «{}» for {} s, rising for {} s, volume {:?})",
                    new.alarm.sound, new.alarm.ring, new.alarm.ramp, new.alarm.volume
                ),
            );
        }
        if new.preferences != old.preferences {
            log("server", "   (settings: how to talk changed)");
        }
        if new.home != old.home {
            crate::text::set_tz(new.home.zone());
            log("server", format!("   (settings: the home changed, the time zone is {})", new.home.zone().name()));
        }
        let all = new.json();
        *self.cur.write().unwrap() = new;
        events::emit("settings", all.clone());
        Ok(all)
    }
}

/// `home.city` of a change as typed → as the geocoder knows it: its name, where it is, and its time zone unless the
/// change names one. "" — no home city (the time zone stays).
async fn located<F: Future<Output = Result<Option<Place>, String>>>(
    patch: &Value,
    locate: impl Fn(String) -> F,
) -> Result<Value, Refused> {
    let mut patch = patch.clone();
    let Some(home) = patch.get_mut("home").and_then(|h| h.as_object_mut()) else { return Ok(patch) };
    let Some(city) = home.get("city") else { return Ok(patch) };
    let city = city.as_str().ok_or_else(|| refused("home.city: нужно название города"))?.trim().to_string();
    if city.chars().count() > 100 {
        return Err(refused("home.city: до 100 знаков"));
    }
    if city.is_empty() {
        home.insert("city".into(), json!(""));
        home.insert("lat".into(), Value::Null);
        home.insert("lon".into(), Value::Null);
        return Ok(patch);
    }
    let place = locate(city.clone())
        .await
        .map_err(|e| refused(format!("Не удалось найти город: {e}")))?
        .ok_or_else(|| refused(format!("Город «{city}» не найден. Проверьте название.")))?;
    home.insert("city".into(), json!(place.name));
    home.insert("lat".into(), json!(place.lat));
    home.insert("lon".into(), json!(place.lon));
    if !home.contains_key("timezone") && place.timezone.parse::<Tz>().is_ok() {
        home.insert("timezone".into(), json!(place.timezone));
    }
    Ok(patch)
}

/// A change (checked: all of it or nothing) onto `new`.
fn apply(new: &mut All, p: &Map<String, Value>) -> Result<(), Refused> {
    let obj = |v: &'_ Value, s: &str| -> Result<Map<String, Value>, Refused> {
        v.as_object().cloned().ok_or_else(|| refused(format!("{s}: нужен объект")))
    };
    let ring_s = |v: &Value, k: &str| -> Result<f64, Refused> {
        let r = v.as_f64().filter(|r| r.is_finite()).ok_or_else(|| refused(format!("{k}: нужно число секунд")))?;
        if !(ring::MIN..=ring::MAX).contains(&r) {
            return Err(refused(format!("{k}: от {} до {} с", ring::MIN, ring::MAX)));
        }
        Ok(r)
    };
    let name = |v: &Value, k: &str| -> Result<String, Refused> {
        Ok(v.as_str().ok_or_else(|| refused(format!("{k}: нужно имя звука")))?.to_string())
    };
    for (section, v) in p {
        match section.as_str() {
            "timer" => {
                for (k, v) in obj(v, "timer")? {
                    match k.as_str() {
                        "sound" => new.timer.sound = name(&v, "timer.sound")?,
                        "ring" => new.timer.ring = ring_s(&v, "timer.ring")?,
                        other => return Err(refused(format!("timer.{other}: такой настройки нет"))),
                    }
                }
            }
            "alarm" => {
                for (k, v) in obj(v, "alarm")? {
                    match k.as_str() {
                        "sound" => new.alarm.sound = name(&v, "alarm.sound")?,
                        "ring" => new.alarm.ring = ring_s(&v, "alarm.ring")?,
                        "ramp" => {
                            new.alarm.ramp = v
                                .as_f64()
                                .filter(|r| (0.0..=RAMP_MAX).contains(r))
                                .ok_or_else(|| refused(format!("alarm.ramp: от 0 до {RAMP_MAX} с")))?
                        }
                        "volume" => {
                            new.alarm.volume = match v {
                                Value::Null => None,
                                v => Some(
                                    v.as_f64()
                                        .filter(|l| level_ok(*l))
                                        .ok_or_else(|| refused("alarm.volume: от 0 до 10 с шагом 0,5 или null"))?,
                                ),
                            }
                        }
                        other => return Err(refused(format!("alarm.{other}: такой настройки нет"))),
                    }
                }
            }
            "preferences" => {
                for (k, v) in obj(v, "preferences")? {
                    match k.as_str() {
                        "text" => {
                            let t =
                                v.as_str().ok_or_else(|| refused("preferences.text: нужен текст"))?.trim().to_string();
                            if t.chars().count() > PREFS_MAX {
                                return Err(refused(format!("предпочтения: не больше {PREFS_MAX} знаков")));
                            }
                            new.preferences.text = t;
                        }
                        "address" => {
                            new.preferences.address = serde_json::from_value(v)
                                .map_err(|_| refused("preferences.address: neutral, male или female"))?
                        }
                        other => return Err(refused(format!("preferences.{other}: такой настройки нет"))),
                    }
                }
            }
            "home" => {
                let coord = |v: &Value, k: &str, max: f64| -> Result<Option<f64>, Refused> {
                    match v {
                        Value::Null => Ok(None),
                        v => Ok(Some(
                            v.as_f64()
                                .filter(|x| x.abs() <= max)
                                .ok_or_else(|| refused(format!("home.{k}: от −{max} до {max} или null")))?,
                        )),
                    }
                };
                for (k, v) in obj(v, "home")? {
                    match k.as_str() {
                        "city" => {
                            new.home.city = v
                                .as_str()
                                .ok_or_else(|| refused("home.city: нужно название города"))?
                                .trim()
                                .to_string()
                        }
                        "lat" => new.home.lat = coord(&v, "lat", 90.0)?,
                        "lon" => new.home.lon = coord(&v, "lon", 180.0)?,
                        "timezone" => {
                            let t = v
                                .as_str()
                                .ok_or_else(|| refused("home.timezone: нужно название часового пояса"))?
                                .trim();
                            if !t.is_empty() && t.parse::<Tz>().is_err() {
                                return Err(refused(format!(
                                    "Часового пояса «{t}» нет. Пример названия: Europe/Moscow."
                                )));
                            }
                            new.home.timezone = t.to_string();
                        }
                        other => return Err(refused(format!("home.{other}: такой настройки нет"))),
                    }
                }
                if new.home.lat.is_some() != new.home.lon.is_some() {
                    return Err(refused("home: широта и долгота задаются вместе"));
                }
            }
            other => return Err(refused(format!("{other}: такой настройки нет"))),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn changed_kept_and_checked() {
        let d = std::env::temp_dir().join(format!("server-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let db = d.join("server.db").to_string_lossy().to_string();
        let sounds = d.to_string_lossy().to_string();
        // the first start: the defaults, written into the store
        let s = Settings::load(Store::open(&db).unwrap(), &sounds);
        assert_eq!(s.timer(), Timer { sound: "chime".into(), ring: 60.0 });
        assert_eq!(s.alarm(), Alarm::default());
        assert_eq!(
            s.change(&json!({"timer": {"ring": 90}})).await.unwrap()["timer"],
            json!({"sound": "chime", "ring": 90.0})
        );
        let all = s
            .change(&json!({"alarm": {"ramp": 60, "volume": null}, "preferences": {"text": "  Зови меня Котик. "}}))
            .await
            .unwrap();
        assert_eq!(
            (all["alarm"]["ramp"].as_f64(), all["alarm"]["volume"].is_null(), all["preferences"]["text"].as_str()),
            (Some(60.0), true, Some("Зови меня Котик."))
        );
        // refused, and nothing of it applied
        for (bad, why) in [
            (json!({"timer": {"ring": 2}}), "от 3"),
            (json!({"timer": {"ring": 601}}), "до 600"),
            (json!({"timer": {"ring": "x"}}), "число"),
            (json!({"timer": {"ring": 30, "sound": "nope.mp3"}}), "нет такого файла"),
            (json!({"timer": {"sound": "../etc/passwd"}}), "не имя файла"),
            (json!({"timer": {"loud": 1}}), "такой настройки нет"),
            (json!({"oven": {}}), "такой настройки нет"),
            (json!([1]), "объект"),
            (json!({"alarm": {"ramp": 301}}), "до 300"),
            (json!({"alarm": {"volume": 4.3}}), "шагом"),
            (json!({"alarm": {"volume": 11}}), "от 0 до 10"),
            (json!({"alarm": {"ring": 30}, "preferences": {"text": "x".repeat(PREFS_MAX + 1)}}), "не больше"),
        ] {
            let e = s.change(&bad).await.unwrap_err().0;
            assert!(e.contains(why), "{bad}: {e}");
        }
        assert_eq!((s.timer().ring, s.alarm().ring), (90.0, 300.0));
        // a later start: as kept
        let s2 = Settings::load(Store::open(&db).unwrap(), &sounds);
        assert_eq!(
            (s2.timer().ring, s2.alarm().ramp, s2.profile().preferences.as_str()),
            (90.0, 60.0, "Зови меня Котик.")
        );
        // the sound in use is gone: the chime instead, for the timer and the alarm alike; another one — nothing changes
        assert!(s2
            .update(|a| {
                a.timer.sound = "x.wav".into();
                a.alarm.sound = "x.wav".into();
                Ok(())
            })
            .is_ok());
        assert!(!s2.sound_replaced("other.wav", "chime"));
        assert!(s2.sound_replaced("x.wav", "chime"));
        let s3 = Settings::load(Store::open(&db).unwrap(), &sounds);
        assert_eq!((s3.timer().sound.as_str(), s3.alarm().sound.as_str()), ("chime", "chime"));
        let _ = std::fs::remove_dir_all(&d);
    }

    fn fresh(name: &str) -> (std::path::PathBuf, Arc<Store>, String) {
        let d = std::env::temp_dir().join(format!("server-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let store = Store::open(&d.join("server.db").to_string_lossy()).unwrap();
        let sounds = d.to_string_lossy().to_string();
        (d, store, sounds)
    }

    #[test]
    fn kept_as_they_are() {
        let (d, store, sounds) = fresh("kept");
        // a field missing (added since): its default; one this server does not know (a newer one's): read all the same;
        // a section it cannot read: the default for now — and none of them written over
        let timer = json!({"sound": "bell.mp3"});
        let alarm = json!({"sound": "gong.ogg", "ring": 120, "ramp": 0, "volume": null, "snooze": 5});
        let prefs = json!({"text": 5});
        for (k, v) in [("timer", &timer), ("alarm", &alarm), ("preferences", &prefs)] {
            store.set_setting(k, v, 1.0).unwrap();
        }
        let s = Settings::load(store.clone(), &sounds);
        assert_eq!(s.timer(), Timer { sound: "bell.mp3".into(), ring: 60.0 });
        assert_eq!(s.alarm(), Alarm { sound: "gong.ogg".into(), ring: 120.0, ramp: 0.0, volume: None });
        assert_eq!(s.profile().preferences, "");
        assert_eq!(
            (store.setting("timer"), store.setting("alarm"), store.setting("preferences")),
            (Some(timer), Some(alarm.clone()), Some(prefs))
        );
        // not even JSON (as a read that fails): the same
        store.db.lock().unwrap().execute("UPDATE settings SET value = '{\"te' WHERE key = 'preferences'", []).unwrap();
        let s = Settings::load(store.clone(), &sounds);
        assert_eq!(s.profile().preferences, "");
        let raw: String = store
            .db
            .lock()
            .unwrap()
            .query_row("SELECT value FROM settings WHERE key = 'preferences'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(raw, "{\"te");
        // a change of another section leaves them be
        s.update(|a| {
            a.timer.ring = 30.0;
            Ok(())
        })
        .unwrap();
        assert_eq!(store.setting("alarm"), Some(alarm));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn the_home_and_the_form_of_address() {
        let (d, store, sounds) = fresh("home");
        let s = Settings::load(store.clone(), &sounds);
        assert_eq!((s.place(), s.profile()), (None, Profile::default()));
        // a city by its name: the geocoder's name, where it is and its time zone
        let kazan = |city: String| async move {
            Ok(match city.as_str() {
                "казань" => {
                    Some(Place {
                        name: "Казань".into(), lat: 55.79, lon: 49.12, timezone: "Europe/Moscow".into()
                    })
                }
                _ => None,
            })
        };
        let patch =
            located(&json!({"home": {"city": " казань "}, "preferences": {"address": "female"}}), kazan).await.unwrap();
        assert_eq!(patch["home"], json!({"city": "Казань", "lat": 55.79, "lon": 49.12, "timezone": "Europe/Moscow"}));
        let all = s.change_with(&patch, |_| async { Ok(()) }).await.unwrap();
        assert_eq!(
            (all["home"]["zone"].as_str(), all["preferences"]["address"].as_str()),
            (Some("Europe/Moscow"), Some("female"))
        );
        assert_eq!(
            s.place(),
            Some(Place { name: "Казань".into(), lat: 55.79, lon: 49.12, timezone: "Europe/Moscow".into() })
        );
        assert_eq!(
            s.profile(),
            Profile { address: Address::Female, city: "Казань".into(), preferences: String::new() }
        );
        // a time zone named with the city stays the user's; an unknown city and a wrong zone are refused
        let patch = located(&json!({"home": {"city": "казань", "timezone": "Asia/Tokyo"}}), kazan).await.unwrap();
        assert_eq!(patch["home"]["timezone"], "Asia/Tokyo");
        assert!(located(&json!({"home": {"city": "Нигдеград"}}), kazan).await.unwrap_err().0.contains("не найден"));
        for bad in [
            json!({"home": {"timezone": "Mars/Olympus"}}),
            json!({"home": {"lat": 91, "lon": 0}}),
            json!({"home": {"lat": null}}),
            json!({"home": {"zone": "Europe/Moscow"}}),
            json!({"preferences": {"address": "they"}}),
        ] {
            assert!(s.change_with(&bad, |_| async { Ok(()) }).await.is_err(), "{bad}");
        }
        // the city taken away: no home for the weather, the time zone as it was
        let patch = located(&json!({"home": {"city": ""}}), kazan).await.unwrap();
        s.change_with(&patch, |_| async { Ok(()) }).await.unwrap();
        assert_eq!(
            (s.place(), store.setting("home").unwrap()["timezone"].as_str().map(String::from)),
            (None, Some("Europe/Moscow".into()))
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn changes_at_once() {
        let (d, store, sounds) = fresh("once");
        let s = Settings::load(store.clone(), &sounds);
        // a new sound is checked (ffmpeg: a while): another tab's change comes in meanwhile — it waits, and neither
        // undoes the other
        let slow = |_: String| async {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            Ok::<(), String>(())
        };
        let (first, second) =
            (json!({"timer": {"sound": "a.wav"}}), json!({"alarm": {"ring": 90}, "preferences": {"text": "Коротко."}}));
        let (a, b) = tokio::join!(s.change_with(&first, slow), async {
            tokio::task::yield_now().await;
            s.change(&second).await
        });
        assert!(a.is_ok() && b.is_ok());
        assert_eq!(
            (s.timer().sound.as_str(), s.alarm().ring, s.profile().preferences.as_str()),
            ("a.wav", 90.0, "Коротко.")
        );
        let again = Settings::load(store.clone(), &sounds);
        assert_eq!(
            (again.timer().sound.as_str(), again.alarm().ring, again.profile().preferences.as_str()),
            ("a.wav", 90.0, "Коротко.")
        );
        // a sound deleted while a change checks another one (that does not wait): both stand
        let third = json!({"alarm": {"sound": "b.wav"}});
        let (c, used) = tokio::join!(s.change_with(&third, slow), async {
            tokio::task::yield_now().await;
            s.sound_replaced("a.wav", sounds::CHIME)
        });
        assert!(c.is_ok() && used);
        assert_eq!((s.timer().sound.as_str(), s.alarm().sound.as_str()), (sounds::CHIME, "b.wav"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
