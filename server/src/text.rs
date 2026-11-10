//! Text and time helpers: the sentence cutter, the wake word stripped from the command, the time stamps of the prompt,
//! and the home's time zone, in which every time is said, shown and counted by days.
use chrono::{Datelike, NaiveDateTime, TimeZone, Timelike};
use chrono_tz::Tz;
use regex::Regex;
use std::sync::{OnceLock, RwLock};

/// The home's time zone (the settings': settings.rs sets it at start and when it changes). The tests' is Moscow's.
static ZONE: RwLock<Tz> = RwLock::new(if cfg!(test) { Tz::Europe__Moscow } else { Tz::UTC });

pub fn tz() -> Tz {
    *ZONE.read().unwrap()
}

pub fn set_tz(zone: Tz) {
    *ZONE.write().unwrap() = zone;
}

/// The zone of the machine the server runs on: TZ, /etc/timezone, where /etc/localtime points; else UTC.
pub fn system_tz() -> Tz {
    if cfg!(test) {
        return Tz::Europe__Moscow; // the tests' clocks, wherever they run
    }
    let link = || {
        std::fs::read_link("/etc/localtime").ok()?.to_str()?.split_once("zoneinfo/").map(|(_, name)| name.to_string())
    };
    std::env::var("TZ")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/timezone").ok())
        .or_else(link)
        .and_then(|name| name.trim().trim_start_matches(':').parse().ok())
        .unwrap_or(Tz::UTC)
}

/// A date and time on the home's clocks → the moment: the earlier one where the clocks are set back and the hour
/// comes twice, an hour on where they are set forward and it never comes.
pub fn local(d: &NaiveDateTime) -> chrono::DateTime<Tz> {
    let zone = tz();
    zone.from_local_datetime(d)
        .earliest()
        .or_else(|| zone.from_local_datetime(&(*d + chrono::Duration::hours(1))).earliest())
        .unwrap_or_else(|| zone.from_utc_datetime(d))
}

const WD: [&str; 7] = ["пн", "вт", "ср", "чт", "пт", "сб", "вс"];
pub const WEEKDAYS: [&str; 7] = ["понедельник", "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье"];
pub const MONTHS_GEN: [&str; 12] = [
    "января",
    "февраля",
    "марта",
    "апреля",
    "мая",
    "июня",
    "июля",
    "августа",
    "сентября",
    "октября",
    "ноября",
    "декабря",
];

/// «ср, 4 ноября, 19:05»: the time of a line in the prompt (the year is in the system prompt).
pub fn stamp(t: f64) -> String {
    let d = tz().timestamp_opt(t as i64, 0).single().unwrap();
    format!(
        "{}, {} {}, {:02}:{:02}",
        WD[d.weekday().num_days_from_monday() as usize],
        d.day(),
        MONTHS_GEN[d.month0() as usize],
        d.hour(),
        d.minute()
    )
}

pub fn now_s() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

/// `x` to `places` decimal places.
pub fn round_to(x: f64, places: usize) -> f64 {
    if x.is_finite() {
        format!("{x:.places$}").parse().unwrap_or(x)
    } else {
        x
    }
}

/// The start of the home's day `back` days before today.
pub fn day_start(back: u32) -> f64 {
    let today = tz().timestamp_opt(now_s() as i64, 0).single().unwrap().date_naive();
    local(&(today - chrono::Days::new(back as u64)).and_hms_opt(0, 0, 0).unwrap()).timestamp() as f64
}

pub fn year() -> i32 {
    tz().timestamp_opt(now_s() as i64, 0).single().unwrap().year()
}

/// The wake word, or what recognition made of its end, at the start of the command (the station sends 0.4 s from before
/// the detection: the word's tail is in it) — «Мразота, включи…» → «включи…». The forms recognition makes of it:
/// «Мразота», «Мрозота», «Морозота», «Разота», «Розоточка», «Азота», «Азот», «Азат», «Азотом», «Озота», «Зота», and
/// split in two — «А зота». Left in the command, «Азат. Громкость три.» makes the model ask who Азат is. Only these,
/// with an ending of the word's own and then a pause or a space: «Азотная кислота», «А зато…», «Разотри…» stay.
pub fn strip_wake(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        let end = r"(?:[аоуеы]|ой|ом|оч\w*|ин\w*|к\w*)?";
        Regex::new(&format!(r"(?i)^\W*(?:(?:м?[ао]?р[ао]?з[оа]т|[ао]з[оа]т|[ао]\s+зот|зот){end}(?:[\s,.!?-]|$)|маз[оу]т\w*|мараз\w*)[\s,.!?-]*")).unwrap()
    });
    re.replace(s, "").trim().to_string()
}

/// The model's token stream cut into sentences, each given out as soon as it ends.
pub struct Sentences {
    buf: String,
}

impl Sentences {
    pub fn new() -> Sentences {
        Sentences { buf: String::new() }
    }

    pub fn feed(&mut self, s: &str, out: &mut Vec<String>) {
        static END: OnceLock<Regex> = OnceLock::new();
        static MARK: OnceLock<Regex> = OnceLock::new();
        let end = END.get_or_init(|| Regex::new(r#"[.!?…]+[»"')]*\s+|\n+"#).unwrap());
        let mark = MARK.get_or_init(|| Regex::new(r#"[.!?…,;:—»"')-]$"#).unwrap());
        self.buf.push_str(s);
        loop {
            if let Some(m) = end.find(&self.buf) {
                let s = self.buf[..m.end()].trim().to_string();
                if !s.is_empty() {
                    out.push(if mark.is_match(&s) { s } else { s + "." }); // a line without an end mark
                }
                self.buf = self.buf[m.end()..].to_string();
                continue;
            }
            // a very long sentence: do not wait for its end, cut after a comma (else at a space)
            if self.buf.chars().count() > 220 {
                let chars: Vec<(usize, char)> = self.buf.char_indices().collect();
                let at = |i: usize| chars.get(i).map(|c| c.0).unwrap_or(self.buf.len());
                let window = |lo: usize, hi: usize| &self.buf[at(lo)..at(hi)];
                let cut = window(60, 200)
                    .rfind(", ")
                    .map(|k| at(60) + k + 1)
                    .or_else(|| window(60, 180).rfind(' ').map(|k| at(60) + k));
                if let Some(k) = cut {
                    out.push(self.buf[..k].trim().to_string());
                    self.buf = self.buf[k..].trim_start().to_string();
                    continue;
                }
            }
            return;
        }
    }

    pub fn flush(&mut self, out: &mut Vec<String>) {
        let s = self.buf.trim();
        if !s.is_empty() {
            out.push(s.to_string());
        }
        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences_as_they_end() {
        let mut s = Sentences::new();
        let mut out = vec![];
        for piece in ["Привет", "! Как ", "дела? Хорошо", "\nНовая строка"] {
            s.feed(piece, &mut out);
        }
        s.flush(&mut out);
        assert_eq!(out, vec!["Привет!", "Как дела?", "Хорошо.", "Новая строка"]);
        let mut long = vec![];
        let mut s = Sentences::new();
        s.feed(&"слово, ".repeat(40), &mut long);
        assert!(!long.is_empty() && long[0].ends_with(','), "a long one is cut after a comma: {long:?}");
    }

    #[test]
    fn wake_word_off_the_command() {
        assert_eq!(strip_wake("Мразота, включи музыку"), "включи музыку");
        assert_eq!(strip_wake("Разота включи"), "включи");
        assert_eq!(strip_wake("Азот."), "");
        assert_eq!(strip_wake("Азат. Громкость три."), "Громкость три.");
        assert_eq!(strip_wake("Азот, какая сейчас громкость?"), "какая сейчас громкость?");
        assert_eq!(strip_wake("Азот что сейчас играет?"), "что сейчас играет?");
        assert_eq!(strip_wake("Азотом. Как там дела"), "Как там дела");
        assert_eq!(strip_wake("Азот дальше."), "дальше.");
        assert_eq!(strip_wake("азотная кислота"), "азотная кислота", "a word that only starts like it stays");
        assert_eq!(strip_wake("Какая погода?"), "Какая погода?");
        for (heard, command) in [
            ("Мрозота. Включи ты моя мразота.", "Включи ты моя мразота."),
            ("Морозота? Нет, не пойдёт.", "Нет, не пойдёт."),
            ("Розоточка, давай дальше.", "давай дальше."),
            ("А зота что сейчас играет?", "что сейчас играет?"),
            ("Озота. Какая погода?", "Какая погода?"),
            ("Озот, включи музыку.", "включи музыку."),
            ("Зота. Включи музыку.", "Включи музыку."),
            ("О зота, сколько времени?", "сколько времени?"),
            ("Мразотина, ну ты даёшь.", "ну ты даёшь."),
            ("Мразоточка, привет", "привет"),
            ("Азота? Как настрой?", "Как настрой?"),
        ] {
            assert_eq!(strip_wake(heard), command, "{heard}");
        }
        for s in [
            "А зато ты молодец.",
            "Зато я выспался.",
            "Разотри мне спину.",
            "Азотная кислота опасна?",
            "Работа начинается в девять.",
            "Розетка не работает.",
            "А это что?",
            "О чём ты?",
            "Зотов — это кто?",
            "Морозов звонил?",
            "Мразь ты.",
        ] {
            assert_eq!(strip_wake(s), s, "{s}");
        }
    }

    #[test]
    fn rounding() {
        assert_eq!(
            (round_to(0.1 + 0.2, 6), round_to(1.0 / 3.0, 6), round_to(2.5, 0), round_to(f64::INFINITY, 2)),
            (0.3, 0.333333, 2.0, f64::INFINITY)
        );
    }

    #[test]
    fn stamps() {
        // 19:05 in Moscow is 16:05 UTC
        let t =
            chrono::NaiveDate::from_ymd_opt(2026, 11, 4).unwrap().and_hms_opt(16, 5, 0).unwrap().and_utc().timestamp()
                as f64;
        assert_eq!(stamp(t), "ср, 4 ноября, 19:05");
    }
}
