//! Home Assistant's entities as Ranunculus shows them: rooms, the devices in them, and each device's controls. A
//! control is one of five kinds — a switch (bool), a number in a range, a choice of options, a reading, an action —
//! whatever the entity behind it is; the web UI and the voice know nothing else. Here is the only place that knows
//! Home Assistant's domains: what controls an entity makes (`controls`), and what service sets one (`call`).
//!
//! Home Assistant is where the home is described: the names, the rooms, what is hidden or disabled there is so here.
//! A device is a device of its registry (a part of another — a strip's socket — named with it), or an entity of no
//! device (a helper, a group of lights). The words (an entity's name, an option's label) are taken in Russian from
//! Home Assistant's translations where they are its own words; what the user or an integration named stays as named.
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;

/// What was read from Home Assistant, as it gave it.
#[derive(Default)]
pub struct Raw {
    pub areas: Vec<Value>,
    pub devices: Vec<Value>,
    pub entities: Vec<Value>,
    /// entity → its state object
    pub states: HashMap<String, Value>,
    /// translations (`frontend/get_translations`): key → text
    pub words: HashMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Room {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Bool,
    Number,
    Choice,
    Reading,
    Action,
}

/// Where a control stands on a device's page: what it is for, how it is set up, what it reports about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    Main,
    Settings,
    Info,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Opt {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Control {
    /// «entity#aspect»: `light.lamp#brightness`
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub group: Group,
    /// null: not known now (the device is away, or it has not said)
    pub value: Value,
    /// what it measures or does, for its icon: the entity's device class, else the aspect
    pub class: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<Opt>,
    /// the entity answers (false: «unavailable»)
    pub available: bool,
    /// the voice may set it (false: a lock, a garage door — by hand in the web UI only)
    pub voice: bool,
    #[serde(skip)]
    pub entity: String,
    #[serde(skip)]
    pub aspect: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub room: Option<String>,
    /// what it is, by what it is for: light, switch, outlet, water_heater, kettle, climate, humidifier, fan, cover,
    /// valve, media_player, tv, speaker, vacuum, mower, lock, alarm, siren, remote, sensor, other
    pub kind: &'static str,
    pub online: bool,
    pub maker: String,
    pub model: String,
    pub controls: Vec<Control>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Home {
    pub rooms: Vec<Room>,
    pub devices: Vec<Device>,
}

/// Before a key in `Raw::words`: the same word in Home Assistant's own language. An entity's name is known to be its
/// integration's (or its device class') by being that word; only then is the Russian one right for it.
pub const OWN: &str = "own:";

/// The colours a light is asked for by name.
pub const COLORS: [(&str, &str, [u8; 3]); 9] = [
    ("красный", "Красный", [255, 0, 0]),
    ("оранжевый", "Оранжевый", [255, 140, 0]),
    ("жёлтый", "Жёлтый", [255, 220, 0]),
    ("зелёный", "Зелёный", [0, 200, 0]),
    ("голубой", "Голубой", [0, 190, 255]),
    ("синий", "Синий", [0, 0, 255]),
    ("фиолетовый", "Фиолетовый", [150, 0, 255]),
    ("розовый", "Розовый", [255, 90, 170]),
    ("белый", "Белый", [255, 255, 255]),
];

/// Readings the voice is told even when Home Assistant files them under diagnostics: what a socket draws, what is
/// left in a battery.
const TOLD_INFO: [&str; 5] = ["power", "energy", "current", "voltage", "battery"];

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn domain(entity: &str) -> &str {
    entity.split('.').next().unwrap_or("")
}

/// A state's text as a number, if it is one («29.00»).
fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(t) => t.trim().parse::<f64>().ok().filter(|x| x.is_finite()),
        _ => None,
    }
}

/// A whole number is written as one (70, not 70.0): it is read aloud and shown.
fn number(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < 1e15 {
        json!(x as i64)
    } else {
        json!((x * 1000.0).round() / 1000.0)
    }
}

fn opt_num(v: Option<f64>) -> Value {
    v.map(number).unwrap_or(Value::Null)
}

impl Raw {
    fn word(&self, keys: &[String]) -> Option<&str> {
        keys.iter().find_map(|k| self.words.get(k).map(String::as_str)).filter(|w| !w.is_empty())
    }

    /// An entity's own name on its device: None — it has none, it is the device itself (its controls are named by
    /// what they do). The name given in Home Assistant by the user; else Home Assistant's own for it — in Russian
    /// when that name is its integration's or its device class' word (the same key's word in Home Assistant's
    /// language): an integration may call an entity otherwise than its key says, and a word with «{…}» in it is
    /// filled in by Home Assistant only.
    fn label(&self, e: &Value, st: &Value, device: &str) -> Option<String> {
        let entity = s(e, "entity_id");
        let (dom, platform, key) = (domain(entity), s(e, "platform"), s(e, "translation_key"));
        // «* Режим» of one integration, «Чайник Режим» of an older Home Assistant: the name alone
        let clean = |n: &str| {
            let n = n.trim().trim_start_matches('*').trim();
            let rest = n.strip_prefix(device).map(str::trim).filter(|r| !device.is_empty() && !r.is_empty());
            rest.unwrap_or(n).to_string()
        };
        if !s(e, "name").trim().is_empty() {
            return Some(clean(s(e, "name")));
        }
        let own = s(e, "original_name");
        if own.trim().is_empty() {
            return None;
        }
        let class = s(&st["attributes"], "device_class");
        let mut keys = vec![];
        if !key.is_empty() {
            keys.push(format!("component.{platform}.entity.{dom}.{key}.name"));
        }
        if !class.is_empty() {
            keys.push(format!("component.{dom}.entity_component.{class}.name"));
        }
        for k in keys {
            let theirs = self.words.get(&format!("{OWN}{k}")).is_some_and(|w| w == own);
            if let Some(ru) = self.words.get(&k).filter(|w| theirs && !w.is_empty() && !w.contains('{')) {
                return Some(ru.clone());
            }
        }
        Some(clean(own))
    }

    /// A state's (or an option's) word: the entity's own translation, its device class', its domain's.
    fn state_word(&self, e: &Value, class: &str, attr: Option<&str>, v: &str) -> String {
        let entity = s(e, "entity_id");
        let (dom, platform, key) = (domain(entity), s(e, "platform"), s(e, "translation_key"));
        let at = attr.map(|a| format!("state_attributes.{a}.")).unwrap_or_default();
        let mut keys = vec![];
        if !key.is_empty() {
            keys.push(format!("component.{platform}.entity.{dom}.{key}.{at}state.{v}"));
        }
        if !class.is_empty() {
            keys.push(format!("component.{dom}.entity_component.{class}.{at}state.{v}"));
        }
        keys.push(format!("component.{dom}.entity_component._.{at}state.{v}"));
        self.word(&keys).unwrap_or(v).to_string()
    }

    /// What an integration calls an attribute of its entity (a convector's «fan mode» is its heating power).
    fn attr_word(&self, e: &Value, attr: &str) -> Option<&str> {
        let (entity, key) = (s(e, "entity_id"), s(e, "translation_key"));
        let k = format!("component.{}.entity.{}.{key}.state_attributes.{attr}.name", s(e, "platform"), domain(entity));
        (!key.is_empty()).then(|| self.word(&[k])).flatten()
    }
}

/// The nearest named colour, if the light's is close to one.
fn color_name(rgb: &Value) -> Option<&'static str> {
    let c: Vec<f64> = rgb.as_array()?.iter().filter_map(Value::as_f64).collect();
    if c.len() != 3 {
        return None;
    }
    COLORS
        .iter()
        .map(|(n, _, k)| (n, (0..3).map(|i| (c[i] - k[i] as f64).powi(2)).sum::<f64>().sqrt()))
        .filter(|(_, d)| *d < 60.0)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(n, _)| *n)
}

/// One entity being made into controls.
struct Entity<'a> {
    raw: &'a Raw,
    e: &'a Value,
    id: &'a str,
    dom: &'a str,
    state: &'a str,
    a: &'a Value,
    feat: u64,
    class: &'a str,
    group: Group,
    /// it answers (not «unavailable»)
    available: bool,
    /// its own name on the device (None: it is the device)
    label: Option<String>,
    out: Vec<Control>,
}

impl<'a> Entity<'a> {
    fn new(raw: &'a Raw, e: &'a Value, st: &'a Value, label: Option<String>) -> Entity<'a> {
        let id = s(e, "entity_id");
        let a = &st["attributes"];
        Entity {
            raw,
            e,
            id,
            dom: domain(id),
            state: s(st, "state"),
            a,
            feat: a["supported_features"].as_u64().unwrap_or(0),
            class: s(a, "device_class"),
            group: match s(e, "entity_category") {
                "config" => Group::Settings,
                "diagnostic" => Group::Info,
                _ => Group::Main,
            },
            available: s(st, "state") != "unavailable",
            label,
            out: vec![],
        }
    }

    fn has(&self, bits: u64) -> bool {
        self.feat & bits != 0
    }

    fn f(&self, attr: &str) -> Option<f64> {
        self.a[attr].as_f64()
    }

    fn list(&self, attr: &str) -> Vec<&'a str> {
        self.a[attr].as_array().into_iter().flatten().filter_map(Value::as_str).collect()
    }

    /// A value the state itself says: not known while the state is not.
    fn by_state(&self, v: Value) -> Value {
        if self.state == "unknown" {
            Value::Null
        } else {
            v
        }
    }

    /// A control. The entity's first carries its name; the others are «name: what».
    fn push(&mut self, aspect: &'static str, what: &str, kind: Kind, value: Value) -> &mut Control {
        let name = match (&self.label, self.out.is_empty()) {
            (Some(l), true) => l.clone(),
            (Some(l), false) => format!("{l}: {}", what.to_lowercase()),
            (None, _) => what.to_string(),
        };
        self.out.push(Control {
            id: format!("{}#{aspect}", self.id),
            name,
            kind,
            group: self.group,
            value: if self.available { value } else { Value::Null },
            class: if self.class.is_empty() { aspect.to_string() } else { self.class.to_string() },
            unit: None,
            min: None,
            max: None,
            step: None,
            options: vec![],
            available: self.available,
            voice: true,
            entity: self.id.to_string(),
            aspect,
        });
        self.out.last_mut().expect("just pushed")
    }

    fn number(
        &mut self,
        aspect: &'static str,
        what: &str,
        value: Option<f64>,
        range: (Option<f64>, Option<f64>, f64),
        unit: &str,
    ) {
        let c = self.push(aspect, what, Kind::Number, opt_num(value));
        (c.min, c.max, c.step) = (range.0, range.1, Some(if range.2 > 0.0 { range.2 } else { 1.0 }));
        c.unit = (!unit.is_empty()).then(|| unit.to_string());
    }

    /// A choice among the values of an attribute's list, each by its word. `attr`: whose words (None: the state's).
    fn choice(&mut self, aspect: &'static str, what: &str, value: Value, list: &str, attr: Option<&str>) {
        let options: Vec<Opt> = self
            .list(list)
            .into_iter()
            .map(|v| Opt { value: v.to_string(), label: self.raw.state_word(self.e, self.class, attr, v) })
            .collect();
        let what = attr.and_then(|a| self.raw.attr_word(self.e, a)).unwrap_or(what).to_string();
        self.push(aspect, &what, Kind::Choice, value).options = options;
    }

    fn reading(&mut self, aspect: &'static str, what: &str, value: Value, unit: &str, class: &str) {
        let c = self.push(aspect, what, Kind::Reading, value);
        c.unit = (!unit.is_empty()).then(|| unit.to_string());
        if !class.is_empty() {
            c.class = class.to_string();
        }
    }

    fn action(&mut self, aspect: &'static str, what: &str) {
        self.push(aspect, what, Kind::Action, Value::Null);
    }

    /// A temperature's unit, as the entity says it.
    fn degrees(&self) -> &'a str {
        match s(self.a, "temperature_unit") {
            "" => "°C",
            u => u,
        }
    }

    fn controls(mut self) -> Vec<Control> {
        let (state, on) = (self.state, self.by_state(json!(self.state == "on")));
        match self.dom {
            "light" => {
                self.push("power", "Питание", Kind::Bool, on);
                let modes = self.list("supported_color_modes");
                if modes.iter().any(|m| *m != "onoff") {
                    let b = self.f("brightness").map(|b| (b / 2.55).round().max(1.0));
                    self.number("brightness", "Яркость", b, (Some(1.0), Some(100.0), 1.0), "%");
                }
                // white of a warmth: its own mode, or made of the two whites of a five-channel light (which then
                // does not say what warmth it is at)
                if modes.contains(&"color_temp") || modes.contains(&"rgbww") {
                    let (min, max) = (self.f("min_color_temp_kelvin"), self.f("max_color_temp_kelvin"));
                    let range = (min.or(Some(2000.0)), max.or(Some(6500.0)), 100.0);
                    self.number("color_temp", "Теплота света", self.f("color_temp_kelvin"), range, "K");
                }
                if modes.iter().any(|m| ["rgb", "rgbw", "rgbww", "hs", "xy"].contains(m)) {
                    let now =
                        (s(self.a, "color_mode") != "color_temp").then(|| color_name(&self.a["rgb_color"])).flatten();
                    self.push("color", "Цвет", Kind::Choice, json!(now)).options =
                        COLORS.iter().map(|(v, l, _)| Opt { value: v.to_string(), label: l.to_string() }).collect();
                }
                if self.has(4) && !self.list("effect_list").is_empty() {
                    self.choice("effect", "Эффект", self.a["effect"].clone(), "effect_list", Some("effect"));
                }
            }
            "switch" | "input_boolean" | "remote" => {
                self.push("power", "Питание", Kind::Bool, on);
            }
            "siren" if self.feat & 3 == 3 => {
                self.push("power", "Сирена", Kind::Bool, on).voice = false;
            }
            "water_heater" => {
                let modes = self.list("operation_list");
                if self.has(8) || (self.has(2) && modes.contains(&"off") && modes.len() > 1) {
                    let heats = self.by_state(json!(state != "off"));
                    self.push("power", "Нагрев", Kind::Bool, heats);
                }
                if self.has(1) {
                    let range = (self.f("min_temp"), self.f("max_temp"), self.f("target_temp_step").unwrap_or(1.0));
                    self.number("target", "Целевая температура", self.f("temperature"), range, self.degrees());
                }
                if self.has(2) && modes.len() > 2 {
                    let now = self.by_state(json!(state));
                    self.choice("mode", "Режим", now, "operation_list", None);
                }
                if self.has(4) {
                    let away = json!(s(self.a, "away_mode") == "on");
                    self.push("away", "Режим «не дома»", Kind::Bool, away);
                }
                if let Some(t) = self.f("current_temperature") {
                    self.reading("now", "Температура сейчас", number(t), self.degrees(), "temperature");
                }
            }
            "climate" => {
                let now = self.by_state(json!(state));
                self.choice("mode", "Режим", now, "hvac_modes", None);
                let step = self.f("target_temp_step").unwrap_or(0.5);
                let range = (self.f("min_temp"), self.f("max_temp"), step);
                // one temperature to keep, or — where it keeps between two — the two of them
                if self.has(1) && !(self.has(2) && self.a["temperature"].is_null()) {
                    self.number("target", "Целевая температура", self.f("temperature"), range, self.degrees());
                }
                if self.has(2) {
                    self.number("low", "Не холоднее", self.f("target_temp_low"), range, self.degrees());
                    self.number("high", "Не теплее", self.f("target_temp_high"), range, self.degrees());
                }
                if self.has(4) {
                    let range = (self.f("min_humidity"), self.f("max_humidity"), 1.0);
                    self.number("humidity", "Целевая влажность", self.f("humidity"), range, "%");
                }
                if self.has(8) {
                    self.choice("fan", "Обдув", self.a["fan_mode"].clone(), "fan_modes", Some("fan_mode"));
                }
                if self.has(16) {
                    self.choice(
                        "preset",
                        "Программа",
                        self.a["preset_mode"].clone(),
                        "preset_modes",
                        Some("preset_mode"),
                    );
                }
                if self.has(32) {
                    self.choice("swing", "Качание", self.a["swing_mode"].clone(), "swing_modes", Some("swing_mode"));
                }
                if !s(self.a, "hvac_action").is_empty() {
                    let does = self.raw.state_word(self.e, self.class, Some("hvac_action"), s(self.a, "hvac_action"));
                    self.reading("does", "Сейчас", json!(does), "", "");
                }
                if let Some(t) = self.f("current_temperature") {
                    self.reading("now", "Температура сейчас", number(t), self.degrees(), "temperature");
                }
                if let Some(h) = self.f("current_humidity") {
                    self.reading("wet", "Влажность сейчас", number(h), "%", "humidity");
                }
            }
            "humidifier" => {
                self.push("power", "Питание", Kind::Bool, on);
                // a device with no target of its own takes none (its integration drops the call in silence)
                if let Some(h) = self.f("humidity") {
                    let step = self.f("target_humidity_step").unwrap_or(1.0);
                    let range = (self.f("min_humidity"), self.f("max_humidity"), step);
                    self.number("target", "Целевая влажность", Some(h), range, "%");
                }
                if self.has(1) {
                    self.choice("mode", "Режим", self.a["mode"].clone(), "available_modes", Some("mode"));
                }
                if !s(self.a, "action").is_empty() {
                    let does = self.raw.state_word(self.e, self.class, Some("action"), s(self.a, "action"));
                    self.reading("does", "Сейчас", json!(does), "", "");
                }
                if let Some(h) = self.f("current_humidity") {
                    self.reading("now", "Влажность сейчас", number(h), "%", "humidity");
                }
            }
            "fan" => {
                if self.has(16 | 32) {
                    self.push("power", "Питание", Kind::Bool, on);
                }
                if self.has(1) {
                    let step = self.f("percentage_step").filter(|s| *s >= 1.0).unwrap_or(1.0);
                    // one of its few speeds, as a share: Home Assistant says the second of three as «66»
                    let speed = self.f("percentage").map(|p| on_step(p, 0.0, step));
                    self.number("speed", "Скорость", speed, (Some(0.0), Some(100.0), step), "%");
                }
                if self.has(8) {
                    self.choice(
                        "preset",
                        "Программа",
                        self.a["preset_mode"].clone(),
                        "preset_modes",
                        Some("preset_mode"),
                    );
                }
                if self.has(2) {
                    let turns = self.a["oscillating"].clone();
                    self.push("oscillating", "Поворот", Kind::Bool, turns);
                }
                if self.has(4) {
                    let now = self.a["direction"].clone();
                    self.push("direction", "Направление", Kind::Choice, now).options =
                        [("forward", "Вперёд"), ("reverse", "Назад")]
                            .map(|(v, l)| Opt { value: v.to_string(), label: l.to_string() })
                            .to_vec();
                }
            }
            "cover" | "valve" => {
                // by the voice a window blind — not a way into the house, and not the water or the gas
                let voice = self.dom == "cover" && !["garage", "gate", "door"].contains(&self.class);
                let from = self.out.len();
                if self.has(4) {
                    self.number("position", "Открыто", self.f("current_position"), (Some(0.0), Some(100.0), 1.0), "%");
                } else if self.feat & 3 == 3 {
                    let open = self.by_state(json!(state == "open" || state == "opening"));
                    self.push("open", "Открыто", Kind::Bool, open);
                }
                if self.dom == "cover" && self.has(128) {
                    let tilt = self.f("current_tilt_position");
                    self.number("tilt", "Наклон", tilt, (Some(0.0), Some(100.0), 1.0), "%");
                }
                if self.has(8) {
                    self.action("stop", "Остановить");
                }
                self.out[from..].iter_mut().for_each(|c| c.voice = voice);
            }
            "select" | "input_select" => {
                let now = self.by_state(json!(state));
                self.choice("option", "Выбор", now, "options", None);
            }
            "number" | "input_number" => {
                let value = num(&json!(state));
                let range = (self.f("min"), self.f("max"), self.f("step").unwrap_or(1.0));
                self.number("value", "Значение", value, range, s(self.a, "unit_of_measurement"));
            }
            "lock" => {
                let locked = self.by_state(json!(state == "locked"));
                self.push("locked", "Заперто", Kind::Bool, locked);
                if self.has(1) {
                    self.action("unlatch", "Открыть защёлку");
                }
                self.out.iter_mut().for_each(|c| c.voice = false);
            }
            "media_player" => {
                if self.has(128 | 256) {
                    let on = self.by_state(json!(!["off", "standby"].contains(&state)));
                    self.push("power", "Питание", Kind::Bool, on);
                }
                // a television showing its aerial is «on», neither playing nor paused: nothing to pause
                if self.has(1 | 16384) && ["playing", "paused", "buffering"].contains(&state) {
                    self.push("playing", "Воспроизведение", Kind::Bool, json!(state != "paused"));
                }
                if self.has(4) {
                    let v = self.f("volume_level").map(|v| (v * 100.0).round());
                    self.number("volume", "Громкость", v, (Some(0.0), Some(100.0), 1.0), "%");
                } else if self.has(1024) {
                    self.action("louder", "Громче");
                    self.action("quieter", "Тише");
                }
                if self.has(8) {
                    let muted = self.a["is_volume_muted"].clone();
                    self.push("muted", "Без звука", Kind::Bool, muted);
                }
                if self.has(2048) && !self.list("source_list").is_empty() {
                    self.choice("source", "Источник", self.a["source"].clone(), "source_list", Some("source"));
                }
                if self.has(65536) && !self.list("sound_mode_list").is_empty() {
                    self.choice(
                        "sound",
                        "Режим звука",
                        self.a["sound_mode"].clone(),
                        "sound_mode_list",
                        Some("sound_mode"),
                    );
                }
                if self.has(16) {
                    self.action("previous", "Назад");
                }
                if self.has(32) {
                    self.action("next", "Дальше");
                }
                if !s(self.a, "media_title").is_empty() {
                    self.reading("title", "Сейчас играет", self.a["media_title"].clone(), "", "");
                }
            }
            "vacuum" | "lawn_mower" => {
                let word = self.by_state(json!(self.raw.state_word(self.e, self.class, None, state)));
                self.reading("state", "Состояние", word, "", "");
                let does: &[(u64, &'static str, &str)] = if self.dom == "vacuum" {
                    &[
                        (8192, "start", "Начать уборку"),
                        (4, "pause", "Пауза"),
                        (8, "stop", "Остановить"),
                        (16, "dock", "Вернуть на базу"),
                        (512, "locate", "Найти"),
                    ]
                } else {
                    &[(1, "start", "Начать стрижку"), (2, "pause", "Пауза"), (4, "dock", "Вернуть на базу")]
                };
                for (bit, aspect, what) in does {
                    if self.has(*bit) {
                        self.action(aspect, what);
                    }
                }
                if self.dom == "vacuum" && self.has(32) && !self.list("fan_speed_list").is_empty() {
                    self.choice("power", "Мощность", self.a["fan_speed"].clone(), "fan_speed_list", Some("fan_speed"));
                }
            }
            "button" | "input_button" => {
                // «restart», «identify» of a device's own upkeep: by hand
                let upkeep = self.group != Group::Main || ["restart", "identify", "update"].contains(&self.class);
                self.push("press", "Нажать", Kind::Action, Value::Null).voice = !upkeep;
            }
            "alarm_control_panel" => {
                let word = self.by_state(json!(self.raw.state_word(self.e, self.class, None, state)));
                self.reading("state", "Охрана", word, "", "");
            }
            "update" => {
                let word = self.by_state(json!(self.raw.state_word(self.e, self.class, None, state)));
                self.group = Group::Info;
                self.reading("state", "Обновление", word, "", "");
            }
            "sensor" => {
                let unit = s(self.a, "unit_of_measurement");
                let value = match num(&json!(state)) {
                    // as many digits as Home Assistant shows (the entity's setting there), three at most
                    Some(x) => match self.e["options"]["sensor"]["display_precision"].as_u64() {
                        Some(d) => number((x * 10f64.powi(d.min(3) as i32)).round() / 10f64.powi(d.min(3) as i32)),
                        None => number(x),
                    },
                    None => json!(self.raw.state_word(self.e, self.class, None, state)),
                };
                let value = self.by_state(value);
                self.reading("now", "Показание", value, unit, "");
            }
            "binary_sensor" => {
                let word = self.by_state(json!(self.raw.state_word(self.e, self.class, None, state)));
                self.reading("now", "Состояние", word, "", "");
            }
            _ => {}
        }
        self.out
    }
}

/// What a device is, by the entities it is for (not its settings or what it reports about itself).
fn kind_of(main: &[(&str, &str)]) -> &'static str {
    const ORDER: [&str; 16] = [
        "climate",
        "water_heater",
        "humidifier",
        "vacuum",
        "lawn_mower",
        "media_player",
        "cover",
        "valve",
        "fan",
        "lock",
        "alarm_control_panel",
        "light",
        "siren",
        "switch",
        "remote",
        "sensor",
    ];
    let Some(dom) = ORDER.iter().find(|d| main.iter().any(|(m, _)| m == *d)) else {
        return if main.iter().any(|(m, _)| *m == "binary_sensor") { "sensor" } else { "other" };
    };
    let class = main.iter().find(|(m, _)| m == dom).map(|(_, c)| *c).unwrap_or("");
    match (*dom, class) {
        ("media_player", "tv") => "tv",
        ("media_player", "speaker") => "speaker",
        ("switch", "outlet") => "outlet",
        ("lawn_mower", _) => "mower",
        ("alarm_control_panel", _) => "alarm",
        _ => dom,
    }
}

/// The domain a device of a kind is for.
fn domain_of(kind: &str) -> &str {
    match kind {
        "tv" | "speaker" => "media_player",
        "outlet" => "switch",
        "kettle" => "water_heater",
        "mower" => "lawn_mower",
        "alarm" => "alarm_control_panel",
        k => k,
    }
}

/// What a kind is called when the home is listed for the model.
pub fn kind_word(kind: &str) -> &'static str {
    match kind {
        "light" => "свет",
        "switch" => "выключатель",
        "outlet" => "розетка",
        "water_heater" => "нагрев воды",
        "kettle" => "чайник",
        "climate" => "климат",
        "humidifier" => "влажность",
        "fan" => "вентилятор",
        "cover" => "шторы или ворота",
        "valve" => "кран",
        "media_player" => "медиа",
        "tv" => "телевизор",
        "speaker" => "колонка",
        "vacuum" => "пылесос",
        "mower" => "газонокосилка",
        "lock" => "замок",
        "alarm" => "охрана",
        "siren" => "сирена",
        "remote" => "пульт",
        "sensor" => "датчик",
        _ => "устройство",
    }
}

/// An entity that is shown at all: not disabled or hidden in Home Assistant, and with a state.
fn shown<'a>(raw: &'a Raw, e: &Value) -> Option<&'a Value> {
    (e["disabled_by"].is_null() && e["hidden_by"].is_null()).then(|| raw.states.get(s(e, "entity_id"))).flatten()
}

/// A device of its entities' controls. `name`: as it is shown; `of`: what of the device's own is known (its room,
/// maker, model). None: nothing of it to show.
fn assemble(raw: &Raw, id: &str, name: String, of: [&str; 3], entities: &[&Value]) -> Option<Device> {
    let mut controls: Vec<Control> = vec![];
    let mut main: Vec<(&str, &str)> = vec![];
    // whether it answers is for the entities it is used by to say: one that reports on the device itself (its
    // integration's «info» button, its cloud's link, «the tank is attached») stays there when the device is gone
    let (mut answers, mut any) = (None, false);
    let mut room = of[0];
    for e in entities {
        let Some(st) = shown(raw, e) else { continue };
        let entity = s(e, "entity_id");
        let class = s(&st["attributes"], "device_class");
        let made = Entity::new(raw, e, st, raw.label(e, st, &name)).controls();
        if made.is_empty() {
            continue;
        }
        let there = s(st, "state") != "unavailable";
        any |= there;
        if e["entity_category"].is_null() {
            // (a helper that is switched is a switch)
            main.push((if domain(entity) == "input_boolean" { "switch" } else { domain(entity) }, class));
            if !(domain(entity) == "binary_sensor" && class == "connectivity") {
                answers = Some(answers.unwrap_or(false) | there);
            }
            if room.is_empty() {
                room = s(e, "area_id"); // a device in no room is where what it is for is
            }
        }
        controls.extend(made);
    }
    // nothing to use it by — only what it reports about itself (an update that waits, how strong its signal is)
    if controls.iter().all(|c| c.group == Group::Info) {
        return None;
    }
    // a heater's own «temperature now» beside a thermometer of the device that reads the same says it twice
    let same: Vec<(String, f64)> = controls
        .iter()
        .filter(|c| c.group == Group::Main && c.entity.starts_with("sensor."))
        .filter_map(|c| Some((c.class.clone(), c.value.as_f64()?)))
        .collect();
    controls.retain(|c| {
        let own = ["now", "wet"].contains(&c.aspect)
            && !c.entity.starts_with("sensor.")
            && !c.entity.starts_with("binary_sensor.");
        !(own
            && c.value
                .as_f64()
                .is_some_and(|v| same.iter().any(|(class, x)| *class == c.class && (x - v).abs() <= 0.5)))
    });
    let mut kind = kind_of(&main);
    // a water heater called a kettle is one: its own picture, its own word for the model
    let said = format!("{name} {}", of[2]).to_lowercase();
    if kind == "water_heater" && (said.contains("kettle") || said.contains("чайник")) {
        kind = "kettle";
    }
    // what it is for is all among its settings (an appliance of programs and buttons): not «a sensor»
    if kind == "sensor" && controls.iter().any(|c| !matches!(c.kind, Kind::Reading)) {
        kind = "other";
    }
    let of_kind = domain_of(kind);
    // an appliance's «Power» switch beside its own switching on says it twice
    let powered = controls.iter().any(|c| c.aspect == "power" && domain(&c.entity) == of_kind && of_kind != "switch");
    controls.retain(|c| {
        let power = ["power", "питание", "включить", "вкл", "switch", "on off"].contains(&plain(&c.name).as_str());
        !(powered && c.group == Group::Main && c.entity.starts_with("switch.") && power)
    });
    // what guards the house is turned by hand, whatever of it
    if kind == "lock" {
        controls.iter_mut().for_each(|c| c.voice = false);
    }
    // what the device is for comes first (a humidifier's switch before its water level); Home Assistant's order else
    controls.sort_by_key(|c| (c.group, domain(&c.entity) != of_kind));
    Some(Device {
        id: id.to_string(),
        name,
        room: raw.areas.iter().any(|a| s(a, "area_id") == room && !room.is_empty()).then(|| room.to_string()),
        kind,
        online: answers.unwrap_or(any),
        maker: of[1].to_string(),
        model: of[2].to_string(),
        controls,
    })
}

/// A device of Home Assistant's with its controls; None: not a thing of the home (Home Assistant's own services, a
/// disabled one, one with nothing to show). A part of another device (a strip's socket) is named with it and stands
/// where it stands.
fn device(raw: &Raw, d: &Value, entities: &[&Value]) -> Option<Device> {
    if s(d, "entry_type") == "service" || !d["disabled_by"].is_null() {
        return None;
    }
    let own = [s(d, "name_by_user"), s(d, "name")].into_iter().find(|n| !n.is_empty())?;
    let parent =
        raw.devices.iter().find(|p| !s(d, "parent_device_id").is_empty() && s(p, "id") == s(d, "parent_device_id"));
    let whole = parent.and_then(|p| [s(p, "name_by_user"), s(p, "name")].into_iter().find(|n| !n.is_empty()));
    let name = match whole {
        Some(p) if s(d, "name_by_user").is_empty() => format!("{p} {own}"),
        _ => own.to_string(),
    };
    let of =
        |k: &str| [s(d, k), parent.map(|p| s(p, k)).unwrap_or("")].into_iter().find(|v| !v.is_empty()).unwrap_or("");
    assemble(raw, s(d, "id"), name, [of("area_id"), of("manufacturer"), of("model")], entities)
}

/// An entity of no device (a helper, a group of lights, a template) as a device of its own, called as the entity.
/// `e`: its record in Home Assistant's registry; None — it has none (set up in Home Assistant's files, or by an
/// integration that gives its entities no ids): then by its state alone, and nowhere to hide it or give it a room.
fn alone(raw: &Raw, entity: &str, e: Option<&Value>) -> Option<Device> {
    let none = json!({"entity_id": entity});
    let e = e.unwrap_or(&none);
    let st = shown(raw, e)?;
    let name = [s(e, "name"), s(e, "original_name"), s(&st["attributes"], "friendly_name")]
        .into_iter()
        .find(|n| !n.trim().is_empty())?;
    // its controls are the device's: named by what they do
    let nameless = json!({"entity_id": entity, "platform": e["platform"], "translation_key": e["translation_key"],
                          "entity_category": e["entity_category"], "options": e["options"], "area_id": e["area_id"]});
    assemble(raw, entity, name.trim().to_string(), [s(e, "area_id"), "", ""], &[&nameless])
}

/// The device an entity is of (`home` and `one` call it by this): its device's id, or its own when it has none.
pub fn owner(e: &Value) -> Option<(String, String)> {
    let entity = e["entity_id"].as_str()?;
    Some((entity.to_string(), e["device_id"].as_str().filter(|d| !d.is_empty()).unwrap_or(entity).to_string()))
}

/// One device anew (a state of it changed): by its id — a device's, or an entity's of no device.
pub fn one(raw: &Raw, id: &str) -> Option<Device> {
    if id.contains('.') {
        return match raw.entities.iter().find(|e| s(e, "entity_id") == id) {
            Some(e) if !s(e, "device_id").is_empty() => None,
            e => alone(raw, id, e),
        };
    }
    let d = raw.devices.iter().find(|d| s(d, "id") == id)?;
    let entities: Vec<&Value> = raw.entities.iter().filter(|e| s(e, "device_id") == id).collect();
    device(raw, d, &entities)
}

pub fn home(raw: &Raw) -> Home {
    let mut of: HashMap<&str, Vec<&Value>> = HashMap::new();
    for e in &raw.entities {
        of.entry(s(e, "device_id")).or_default().push(e);
    }
    let none = vec![];
    let mut devices: Vec<Device> =
        raw.devices.iter().filter_map(|d| device(raw, d, of.get(s(d, "id")).unwrap_or(&none))).collect();
    devices.extend(of.get("").into_iter().flatten().filter_map(|e| alone(raw, s(e, "entity_id"), Some(e))));
    let recorded: std::collections::HashSet<&str> = raw.entities.iter().map(|e| s(e, "entity_id")).collect();
    devices
        .extend(raw.states.keys().filter(|id| !recorded.contains(id.as_str())).filter_map(|id| alone(raw, id, None)));
    devices.sort_by_cached_key(|d| d.name.to_lowercase());
    let rooms = raw
        .areas
        .iter()
        .map(|a| Room { id: s(a, "area_id").to_string(), name: s(a, "name").to_string() })
        .filter(|r| devices.iter().any(|d| d.room.as_deref() == Some(&r.id)))
        .collect();
    Home { rooms, devices }
}

/// What of a device the voice is told: what it may set, and what there is to read — a lock's state too, though not
/// the voice's to turn; of what a device reports about itself, only what is asked about (what a socket draws).
pub fn told(c: &Control) -> bool {
    match c.group {
        Group::Info => c.kind == Kind::Reading && TOLD_INFO.contains(&c.class.as_str()),
        _ => c.voice || c.kind != Kind::Action,
    }
}

// ---------------------------------------------------------------- setting a control

/// A service of Home Assistant to call.
#[derive(Debug, PartialEq)]
pub struct Call {
    pub domain: String,
    pub service: &'static str,
    pub entity: String,
    pub data: Value,
}

/// Lower case, «ё» as «е», no punctuation: how names and options are compared.
pub fn plain(t: &str) -> String {
    t.to_lowercase()
        .replace('ё', "е")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn truth(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_f64().map(|x| x != 0.0),
        Value::String(t) => match plain(t).as_str() {
            "true" | "on" | "1" | "да" | "вкл" | "включить" | "включено" | "открыть" => {
                Some(true)
            }
            "false" | "off" | "0" | "нет" | "выкл" | "выключить" | "выключено" | "закрыть" => {
                Some(false)
            }
            _ => None,
        },
        _ => None,
    }
}

fn range_text(c: &Control) -> String {
    let unit = c.unit.as_deref().map(|u| format!(" {u}")).unwrap_or_default();
    match (c.min, c.max) {
        (Some(a), Some(b)) => format!("от {} до {}{unit}", number(a), number(b)),
        (Some(a), None) => format!("от {}{unit}", number(a)),
        (None, Some(b)) => format!("до {}{unit}", number(b)),
        (None, None) => "любое число".into(),
    }
}

/// The value of a control's own nearest to `x`: `from` its least, by `step`.
fn on_step(x: f64, from: f64, step: f64) -> f64 {
    let n = from + ((x - from) / step).round() * step;
    (n * 1e6).round() / 1e6
}

/// What a control should read after a call: it is waited for.
#[derive(Debug, PartialEq)]
pub enum Want {
    Is(Value),
    /// a number, give or take: a light's brightness comes back through 255 steps, a fan's speed is one of a few,
    /// a blind stops a little off
    Near(f64, f64),
    /// nothing to read back (an action; a television waking up by the minute), or nothing that says it (a colour
    /// comes back as the lamp could make it)
    Done,
    /// …or the entity on its way there, by its state (a blind «opening», a lock «locking»): what takes a minute is
    /// not waited out
    IsOr(Value, &'static [&'static str]),
    NearOr(f64, f64, &'static [&'static str]),
}

impl Want {
    /// It reads so already: nothing is to be sent. Exactly so — one step up is a change; and not while the entity
    /// is on its way somewhere (a blind closing that passes the half it is now asked for).
    pub fn already(&self, now: &Value, state: &str) -> bool {
        let at = |x: f64| now.as_f64().is_some_and(|n| (n - x).abs() < SAME);
        match self {
            Want::Is(v) => now == v,
            Want::Near(x, _) => at(*x),
            Want::IsOr(v, on_the_way) => now == v && !on_the_way.contains(&state),
            Want::NearOr(x, _, on_the_way) => at(*x) && !on_the_way.contains(&state),
            Want::Done => false,
        }
    }

    /// The control reads what was asked: `now` its value, `state` its entity's state, `before` what it read when
    /// asked — a number near enough counts only once it has moved (21 is not «22, give or take»). What takes its time
    /// (a blind, a louvre) is done when it is on its way: by its state, or by its number nearing what was asked.
    pub fn met(&self, now: &Value, state: &str, before: &Value) -> bool {
        let off = |v: &Value, x: f64| v.as_f64().map(|n| (n - x).abs());
        let near = |x: f64, by: f64| off(now, x).is_some_and(|d| d < SAME || (now != before && d <= by));
        let nearing = |x: f64| matches!((off(now, x), off(before, x)), (Some(d), Some(was)) if d < was);
        match self {
            Want::Is(v) => now == v,
            Want::IsOr(v, on_the_way) => now == v || on_the_way.contains(&state),
            Want::Near(x, by) => near(*x, *by),
            Want::NearOr(x, by, on_the_way) => near(*x, *by) || on_the_way.contains(&state) || nearing(*x),
            Want::Done => true,
        }
    }
}

const MOVING: &[&str] = &["opening", "closing"];
/// two readings of a number are one (they are said to three digits)
const SAME: f64 = 1e-3;

/// The service that sets `c` to `v`, and what the control reads then. Err: why not, as the user is told — the value
/// is not one the device takes (Home Assistant itself lets 130° through to a kettle of 100), or the device has no
/// such thing to do.
pub fn call(c: &Control, st: &Value, v: &Value) -> Result<(Call, Want), String> {
    if !c.available {
        return Err("устройство не в сети".into());
    }
    let dom = domain(&c.entity).to_string();
    let a = &st["attributes"];
    let feat = a["supported_features"].as_u64().unwrap_or(0);
    let go = |service: &'static str, data: Value, want: Want| {
        Ok((Call { domain: dom.clone(), service, entity: c.entity.clone(), data }, want))
    };
    match c.kind {
        Kind::Reading => Err("это показание, его нельзя менять".into()),
        Kind::Action => {
            if truth(v) == Some(false) {
                return Err("это действие: его можно только выполнить".into());
            }
            let service = match (dom.as_str(), c.aspect) {
                ("vacuum", "start") => "start",
                ("vacuum", "dock") => "return_to_base",
                ("vacuum" | "lawn_mower", "pause") => "pause",
                ("vacuum", "stop") => "stop",
                ("vacuum", "locate") => "locate",
                ("lawn_mower", "start") => "start_mowing",
                ("lawn_mower", "dock") => "dock",
                ("cover", "stop") => "stop_cover",
                ("valve", "stop") => "stop_valve",
                ("lock", "unlatch") => "open",
                ("media_player", "louder") => "volume_up",
                ("media_player", "quieter") => "volume_down",
                ("media_player", "next") => "media_next_track",
                ("media_player", "previous") => "media_previous_track",
                (_, "press") => "press",
                _ => return Err("этим управлять нельзя".into()),
            };
            go(service, json!({}), Want::Done)
        }
        Kind::Bool => {
            let on = truth(v).ok_or("нужно «включить» или «выключить» (true или false)")?;
            let want = Want::Is(json!(on));
            let cannot = |what: &str| Err(format!("устройство не умеет {what} по команде из Home Assistant"));
            match (dom.as_str(), c.aspect) {
                ("water_heater", "power") if feat & 8 == 0 => {
                    let first = a["operation_list"].as_array().into_iter().flatten().find(|m| *m != "off").cloned();
                    let mode = if on {
                        first.ok_or("у устройства нет режима нагрева")?
                    } else {
                        json!("off")
                    };
                    go("set_operation_mode", json!({"operation_mode": mode}), want)
                }
                ("water_heater", "away") => go("set_away_mode", json!({"away_mode": on}), want),
                ("lock", "locked") if on => go("lock", json!({}), Want::IsOr(json!(true), &["locking"])),
                ("lock", "locked") => {
                    go("unlock", json!({}), Want::IsOr(json!(false), &["unlocking", "open", "opening"]))
                }
                ("cover", "open") => go(if on { "open_cover" } else { "close_cover" }, json!({}), want),
                ("valve", "open") => go(if on { "open_valve" } else { "close_valve" }, json!({}), want),
                ("media_player", "playing") => go(if on { "media_play" } else { "media_pause" }, json!({}), want),
                ("media_player", "muted") => go("volume_mute", json!({"is_volume_muted": on}), want),
                ("media_player", "power") if on && feat & 128 == 0 => cannot("включаться"),
                ("media_player", "power") if !on && feat & 256 == 0 => cannot("выключаться"),
                // a television wakes over the network for longer than anyone waits
                ("media_player", "power") if on => go("turn_on", json!({}), Want::Done),
                ("fan", "power") if on && feat & 32 == 0 => cannot("включаться"),
                ("fan", "power") if !on && feat & 16 == 0 => cannot("выключаться"),
                ("fan", "oscillating") => go("oscillate", json!({"oscillating": on}), want),
                (_, "power") => go(if on { "turn_on" } else { "turn_off" }, json!({}), want),
                _ => Err("этим управлять нельзя".into()),
            }
        }
        Kind::Number => {
            let x = num(v).ok_or("нужно число")?;
            if c.min.is_some_and(|m| x < m) || c.max.is_some_and(|m| x > m) {
                return Err(format!("вне диапазона: можно {}", range_text(c)));
            }
            let step = c.step.unwrap_or(1.0);
            // on the control's own step (21.3° to a thermostat of halves is 21.5°) — but a fan's few speeds are
            // Home Assistant's to choose among
            let x = on_step(x, c.min.unwrap_or(0.0), step);
            let exact = Want::Near(x, step / 2.0);
            match (dom.as_str(), c.aspect) {
                ("light", "brightness") => go("turn_on", json!({"brightness_pct": x}), Want::Near(x, 1.0)),
                // the two whites of a five-channel light do not say what warmth they make
                ("light", "color_temp")
                    if a["color_temp_kelvin"].is_null()
                        && a["supported_color_modes"]
                            .as_array()
                            .is_some_and(|m| !m.iter().any(|x| x == "color_temp")) =>
                {
                    go("turn_on", json!({"color_temp_kelvin": x}), Want::Done)
                }
                ("light", "color_temp") => go("turn_on", json!({"color_temp_kelvin": x}), Want::Near(x, 150.0)),
                ("water_heater" | "climate", "target") => go("set_temperature", json!({"temperature": x}), exact),
                // the two are set together: the one not asked for stays as it is
                ("climate", "low" | "high") => {
                    let other = if c.aspect == "low" { "target_temp_high" } else { "target_temp_low" };
                    let kept = a[other].as_f64().ok_or("устройство не сказало вторую границу температуры")?;
                    let (low, high) = if c.aspect == "low" { (x, kept) } else { (kept, x) };
                    if low > high {
                        return Err("нижняя граница выше верхней".into());
                    }
                    go("set_temperature", json!({"target_temp_low": low, "target_temp_high": high}), exact)
                }
                // a humidifier keeps to its own steps (tens, as a rule), whatever it says of them
                ("humidifier", "target") | ("climate", "humidity") => {
                    go("set_humidity", json!({"humidity": x.round()}), Want::Near(x, step.max(5.0)))
                }
                // a fan has a few speeds: the one asked for becomes the nearest above
                // (Home Assistant takes a fan's percentage up to the next speed: «67» of three would be the third)
                ("fan", "speed") => go("set_percentage", json!({"percentage": (x + 1e-6).floor()}), Want::Near(x, 1.0)),
                ("cover", "position") => {
                    go("set_cover_position", json!({"position": x.round()}), Want::NearOr(x, 3.0, MOVING))
                }
                ("cover", "tilt") => {
                    go("set_cover_tilt_position", json!({"tilt_position": x.round()}), Want::NearOr(x, 3.0, MOVING))
                }
                ("valve", "position") => {
                    go("set_valve_position", json!({"position": x.round()}), Want::NearOr(x, 3.0, MOVING))
                }
                ("media_player", "volume") => go("volume_set", json!({"volume_level": x / 100.0}), Want::Near(x, 1.0)),
                (_, "value") => go("set_value", json!({"value": x}), exact),
                _ => Err("этим управлять нельзя".into()),
            }
        }
        Kind::Choice => {
            let asked = plain(&match v {
                Value::String(t) => t.clone(),
                other => other.to_string(),
            });
            let Some(o) = c.options.iter().find(|o| plain(&o.value) == asked || plain(&o.label) == asked) else {
                let all: Vec<&str> = c.options.iter().take(OPTIONS_SAID).map(|o| o.label.as_str()).collect();
                let more = if c.options.len() > all.len() { " и другие" } else { "" };
                return Err(format!("нет такого варианта; есть: {}{more}", all.join(", ")));
            };
            let want = Want::Is(json!(o.value));
            match (dom.as_str(), c.aspect) {
                ("light", "color") => {
                    let rgb = COLORS.iter().find(|(n, ..)| *n == o.value).map(|(.., c)| c).ok_or("нет такого цвета")?;
                    go("turn_on", json!({"rgb_color": rgb}), Want::Done)
                }
                ("light", "effect") => go("turn_on", json!({"effect": o.value}), want),
                ("water_heater", "mode") => go("set_operation_mode", json!({"operation_mode": o.value}), want),
                ("climate", "mode") => go("set_hvac_mode", json!({"hvac_mode": o.value}), want),
                ("climate", "fan") => go("set_fan_mode", json!({"fan_mode": o.value}), want),
                ("climate" | "fan", "preset") => go("set_preset_mode", json!({"preset_mode": o.value}), want),
                ("climate", "swing") => go("set_swing_mode", json!({"swing_mode": o.value}), want),
                ("humidifier", "mode") => go("set_mode", json!({"mode": o.value}), want),
                ("fan", "direction") => go("set_direction", json!({"direction": o.value}), want),
                ("media_player", "source") => go("select_source", json!({"source": o.value}), want),
                ("media_player", "sound") => go("select_sound_mode", json!({"sound_mode": o.value}), want),
                ("vacuum", "power") => go("set_fan_speed", json!({"fan_speed": o.value}), want),
                (_, "option") => go("select_option", json!({"option": o.value}), want),
                _ => Err("этим управлять нельзя".into()),
            }
        }
    }
}

/// How many of a choice's options are said at once (a light's effects come by the hundred).
pub const OPTIONS_SAID: usize = 24;

#[cfg(test)]
pub mod tests {
    use super::*;

    fn entity(id: &str, dev: Value, platform: &str, cat: Value, key: &str, original: &str) -> Value {
        json!({"entity_id": id, "device_id": dev, "platform": platform, "translation_key": key, "name": null, "area_id": null,
               "original_name": original, "entity_category": cat, "disabled_by": null, "hidden_by": null, "options": {}})
    }

    fn state(id: &str, state: &str, attrs: Value) -> (String, Value) {
        (id.to_string(), json!({"entity_id": id, "state": state, "attributes": attrs}))
    }

    fn words(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// A kitchen with a kettle as an integration gives one (a heater, a switch beside it that does the same,
    /// settings, diagnostics, a word that it is connected), a lamp that is away, and Home Assistant's own «devices».
    /// Home Assistant itself speaks English here: its words for the entities are the `OWN` ones.
    pub fn raw() -> Raw {
        let ent = |id: &str, dev: &str, cat: Value, key: &str, original: &str| {
            entity(id, json!(dev), "acme", cat, key, original)
        };
        Raw {
            areas: vec![
                json!({"area_id": "kitchen", "name": "Кухня"}),
                json!({"area_id": "hall", "name": "Гостиная"}),
                json!({"area_id": "attic", "name": "Чердак"}),
            ],
            devices: vec![
                json!({"id": "k", "name": "kettle K-100", "name_by_user": "Чайник", "area_id": "kitchen", "manufacturer": "Acme",
                       "model": "K-100", "entry_type": null, "disabled_by": null}),
                json!({"id": "l", "name": "Лампа", "name_by_user": null, "area_id": "hall", "manufacturer": "Acme", "model": "L-10",
                       "entry_type": null, "disabled_by": null}),
                json!({"id": "sun", "name": "Sun", "name_by_user": null, "area_id": null, "entry_type": "service", "disabled_by": null}),
                json!({"id": "gone", "name": "Розетка", "name_by_user": null, "area_id": "attic", "entry_type": null, "disabled_by": "user"}),
            ],
            entities: vec![
                ent("water_heater.kettle", "k", Value::Null, "water_heater", "Water heater"),
                ent("switch.kettle_power", "k", Value::Null, "power_switch", "Power"),
                ent("sensor.kettle_temperature", "k", Value::Null, "", "Temperature"),
                ent("select.kettle_mode", "k", json!("config"), "select_mode_kettle", "Preset"),
                ent("switch.kettle_backlight", "k", json!("config"), "backlight_switch", "Backlight"),
                ent("sensor.kettle_rssi", "k", json!("diagnostic"), "rssi", "RSSI"),
                ent("binary_sensor.kettle_available", "k", Value::Null, "available_binary_sensor", "LAN"),
                ent("light.lamp", "l", Value::Null, "", ""),
                ent("sensor.sun_next", "sun", Value::Null, "", "Next dawn"),
                ent("switch.gone", "gone", Value::Null, "", ""),
            ],
            states: HashMap::from([
                state(
                    "water_heater.kettle",
                    "off",
                    json!({"min_temp": 30, "max_temp": 100, "operation_list": ["off", "performance", "electric"],
                       "current_temperature": 29.0, "temperature": 30, "supported_features": 11}),
                ),
                state("switch.kettle_power", "off", json!({"device_class": "switch"})),
                state(
                    "sensor.kettle_temperature",
                    "29.00",
                    json!({"device_class": "temperature", "unit_of_measurement": "°C"}),
                ),
                state("select.kettle_mode", "not_selected", json!({"options": ["not_selected", "green", "black"]})),
                state("switch.kettle_backlight", "on", json!({"device_class": "switch"})),
                state(
                    "sensor.kettle_rssi",
                    "-45",
                    json!({"device_class": "signal_strength", "unit_of_measurement": "dB"}),
                ),
                state("binary_sensor.kettle_available", "on", json!({"device_class": "connectivity"})),
                state(
                    "light.lamp",
                    "unavailable",
                    json!({"supported_color_modes": ["color_temp", "rgb"], "friendly_name": "Лампа"}),
                ),
                state("sensor.sun_next", "2026-11-22", json!({})),
                state("switch.gone", "on", json!({})),
            ]),
            words: words(&[
                ("component.acme.entity.switch.backlight_switch.name", "Подсветка"),
                ("own:component.acme.entity.switch.backlight_switch.name", "Backlight"),
                ("component.acme.entity.select.select_mode_kettle.name", "Программа"),
                ("own:component.acme.entity.select.select_mode_kettle.name", "Preset"),
                ("component.acme.entity.select.select_mode_kettle.state.green", "Зелёный чай"),
                ("component.acme.entity.select.select_mode_kettle.state.not_selected", "Не выбрано"),
                ("component.acme.entity.water_heater.water_heater.state.performance", "Кипячение"),
                ("component.acme.entity.water_heater.water_heater.state.electric", "Разогрев"),
                ("component.sensor.entity_component.temperature.name", "Температура"),
                ("own:component.sensor.entity_component.temperature.name", "Temperature"),
                ("component.binary_sensor.entity_component.connectivity.state.on", "Подключено"),
            ]),
        }
    }

    fn control<'a>(d: &'a Device, id: &str) -> &'a Control {
        let all: Vec<&String> = d.controls.iter().map(|c| &c.id).collect();
        d.controls.iter().find(|c| c.id == id).unwrap_or_else(|| panic!("no control {id}: {all:?}"))
    }

    /// One entity of a device «Вещь», as its controls.
    fn made(id: &str, st: &str, attrs: Value) -> Vec<Control> {
        let e = entity(id, json!("d"), "demo", Value::Null, "", "");
        let st = json!({"entity_id": id, "state": st, "attributes": attrs});
        Entity::new(&Raw::default(), &e, &st, None).controls()
    }

    fn aspects(controls: &[Control]) -> Vec<&str> {
        controls.iter().map(|c| c.aspect).collect()
    }

    #[test]
    fn a_home_of_what_is_there() {
        let h = home(&raw());
        // Home Assistant's own services and what is disabled there are not things of the home; a room with nothing
        // in it is not listed
        assert_eq!(h.devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["Лампа", "Чайник"]);
        assert_eq!(h.rooms.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["Кухня", "Гостиная"]);
        let k = &h.devices[1];
        assert_eq!((k.kind, k.online, k.room.as_deref()), ("kettle", true, Some("kitchen")));
        // what it is for first (the heater before the thermometer), then how it is set up, then what it reports
        assert_eq!(k.controls[0].id, "water_heater.kettle#power");
        let groups: Vec<Group> = k.controls.iter().map(|c| c.group).collect();
        assert!(groups.is_sorted(), "{groups:?}");
        // the words are Russian where the entity's name is its integration's or its class' own; else as they are
        assert_eq!(control(k, "switch.kettle_backlight#power").name, "Подсветка");
        assert_eq!(control(k, "sensor.kettle_temperature#now").name, "Температура");
        assert_eq!(control(k, "sensor.kettle_rssi#now").name, "RSSI");
        let t = control(k, "water_heater.kettle#target");
        assert_eq!(
            (t.name.as_str(), t.kind, &t.value, t.min, t.max, t.unit.as_deref()),
            ("Water heater: целевая температура", Kind::Number, &json!(30), Some(30.0), Some(100.0), Some("°C"))
        );
        assert_eq!(control(k, "water_heater.kettle#power").value, json!(false));
        let modes = control(k, "water_heater.kettle#mode");
        assert_eq!(
            modes.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(),
            ["off", "Кипячение", "Разогрев"]
        );
        assert_eq!(control(k, "sensor.kettle_temperature#now").value, json!(29));
        // the heater's own «temperature now» is not said beside a thermometer that reads the same, nor is the
        // appliance's «Power» switch beside its own switching on
        assert!(!k.controls.iter().any(|c| c.id == "water_heater.kettle#now" || c.id == "switch.kettle_power#power"));
        let m = control(k, "select.kettle_mode#option");
        assert_eq!((m.name.as_str(), m.group, m.value.as_str()), ("Программа", Group::Settings, Some("not_selected")));
        assert_eq!(m.options[1], Opt { value: "green".into(), label: "Зелёный чай".into() });
        assert_eq!(control(k, "binary_sensor.kettle_available#now").value, json!("Подключено"));
        // a lamp that does not answer: there, away, its controls without values
        let l = &h.devices[0];
        assert_eq!((l.kind, l.online), ("light", false));
        let p = control(l, "light.lamp#power");
        assert_eq!((p.name.as_str(), &p.value, p.available), ("Питание", &Value::Null, false));
        assert_eq!(one(&raw(), "k").as_ref(), Some(k));
        assert_eq!(one(&raw(), "sun"), None);
    }

    #[test]
    fn there_or_away_is_for_what_the_device_is_used_by_to_say() {
        // its link to a cloud, «the tank is attached», an «info» button that is always there: not the device's word
        let mut r = raw();
        r.states.get_mut("binary_sensor.kettle_available").unwrap()["state"] = json!("off");
        assert!(home(&r).devices[1].online, "a connectivity sensor does not take a device away");
        for id in ["water_heater.kettle", "switch.kettle_power", "sensor.kettle_temperature"] {
            r.states.get_mut(id).unwrap()["state"] = json!("unavailable");
        }
        r.states.get_mut("binary_sensor.kettle_available").unwrap()["state"] = json!("on");
        assert!(
            !home(&r).devices[1].online,
            "…nor keeps it there, with a diagnostic sensor and the settings still answering"
        );
    }

    #[test]
    fn names() {
        let name = |e: Value, own: &[(&str, &str)], device: &str| {
            let r = Raw { words: words(own), ..Raw::default() };
            r.label(&e, &json!({"attributes": {"device_class": "temperature"}}), device)
        };
        let e = |key: &str, original: &str| entity("sensor.x", json!("d"), "tuya", Value::Null, key, original);
        let key = "component.tuya.entity.sensor.indexed.name";
        // «Розетка {x}» is filled in by Home Assistant only: its own word stays
        assert_eq!(
            name(
                e("indexed", "Socket 2"),
                &[(key, "Розетка {x}"), ("own:component.tuya.entity.sensor.indexed.name", "Socket {x}")],
                ""
            )
            .as_deref(),
            Some("Socket 2")
        );
        // the integration named it otherwise than its key says: not the key's word
        assert_eq!(
            name(
                e("indexed", "Boiler flow"),
                &[(key, "Подача"), ("own:component.tuya.entity.sensor.indexed.name", "Flow")],
                ""
            )
            .as_deref(),
            Some("Boiler flow")
        );
        // no name of its own: the device itself
        assert_eq!(name(e("", ""), &[], "Вещь"), None);
        // named by the user in Home Assistant: that, whatever the words say
        let mut mine = e("indexed", "Socket 2");
        mine["name"] = json!(" Мой датчик ");
        assert_eq!(name(mine, &[(key, "Розетка")], "").as_deref(), Some("Мой датчик"));
        // an older Home Assistant's «Device Name», one integration's «* Name»
        assert_eq!(name(e("", "Белая розетка напряжение"), &[], "Белая розетка").as_deref(), Some("напряжение"));
        assert_eq!(name(e("", "* Режим"), &[], "Вещь").as_deref(), Some("Режим"));
        assert_eq!(name(e("", "Вещь"), &[], "Вещь").as_deref(), Some("Вещь"));
    }

    #[test]
    fn what_stands_alone_or_is_a_part() {
        let mut r = raw();
        // a helper of no device is a device of its own, where the helper is
        let mut helper = entity("input_boolean.guests", Value::Null, "input_boolean", Value::Null, "", "Гости");
        helper["area_id"] = json!("hall");
        r.entities.push(helper);
        r.states.extend([state("input_boolean.guests", "on", json!({"friendly_name": "Гости"}))]);
        // a socket of a strip: called with the strip, standing where it stands
        r.devices.push(json!({"id": "strip", "name": "Удлинитель", "name_by_user": null, "area_id": "attic", "manufacturer": "Tuya", "model": "S4",
                              "entry_type": null, "disabled_by": null}));
        r.devices.push(
            json!({"id": "ch1", "name": "Channel 1", "name_by_user": null, "area_id": null, "parent_device_id": "strip",
                              "entry_type": null, "disabled_by": null}),
        );
        r.entities.push(entity("switch.strip_1", json!("ch1"), "tuya", Value::Null, "", ""));
        r.states.extend([state("switch.strip_1", "off", json!({"device_class": "outlet"}))]);
        // a device in no room is where what it is for is; a room Home Assistant does not list is none
        r.devices.push(json!({"id": "fan", "name": "Вентилятор", "name_by_user": null, "area_id": "nowhere", "entry_type": null, "disabled_by": null}));
        let mut blades = entity("fan.blades", json!("fan"), "demo", Value::Null, "", "");
        blades["area_id"] = json!("kitchen");
        r.entities.push(blades);
        r.states.extend([state(
            "fan.blades",
            "on",
            json!({"supported_features": 49, "percentage": 33, "percentage_step": 33.333}),
        )]);
        let h = home(&r);
        let by = |name: &str| h.devices.iter().find(|d| d.name == name).unwrap_or_else(|| panic!("no {name}"));
        let g = by("Гости");
        assert_eq!(
            (g.id.as_str(), g.kind, g.room.as_deref(), g.controls[0].name.as_str()),
            ("input_boolean.guests", "switch", Some("hall"), "Питание")
        );
        assert_eq!(one(&r, "input_boolean.guests").as_ref(), Some(g));
        let s1 = by("Удлинитель Channel 1");
        assert_eq!(
            (s1.kind, s1.room.as_deref(), s1.maker.as_str(), s1.model.as_str()),
            ("outlet", Some("attic"), "Tuya", "S4")
        );
        assert_eq!(by("Вентилятор").room, None);
        // an entity Home Assistant keeps no record of: by its state alone
        r.states.extend([state(
            "media_player.old",
            "playing",
            json!({"friendly_name": "Проигрыватель", "supported_features": 4, "volume_level": 0.4}),
        )]);
        let h = home(&r);
        let old = h.devices.iter().find(|d| d.name == "Проигрыватель").unwrap();
        assert_eq!((old.id.as_str(), old.kind, old.room.as_deref()), ("media_player.old", "media_player", None));
        assert_eq!(one(&r, "media_player.old").as_ref(), Some(old));
        assert_eq!(one(&r, "fan.blades"), None, "an entity of a device is not a device");
        assert_eq!(owner(&r.entities.last().unwrap().clone()), Some(("fan.blades".into(), "fan".into())));
        assert_eq!(
            owner(&entity("light.x", Value::Null, "group", Value::Null, "", "")),
            Some(("light.x".into(), "light.x".into()))
        );
    }

    #[test]
    fn exactly_what_was_asked() {
        let r = raw();
        let h = home(&r);
        let k = &h.devices[1];
        let st = |c: &Control| r.states[&c.entity].clone();
        let set = |id: &str, v: Value| {
            let c = control(k, id);
            call(c, &st(c), &v)
        };
        let (c, want) = set("water_heater.kettle#target", json!(70)).unwrap();
        assert_eq!(
            (c.domain.as_str(), c.service, c.entity.as_str(), &c.data),
            ("water_heater", "set_temperature", "water_heater.kettle", &json!({"temperature": 70.0}))
        );
        // one degree up is a change: not «70, give or take», and not read as done before the kettle says so
        assert!(want.already(&json!(70), "on") && !want.already(&json!(69), "on"));
        assert!(
            want.met(&json!(70), "off", &json!(30))
                && !want.met(&json!(69), "off", &json!(69))
                && !want.met(&Value::Null, "off", &json!(30))
        );
        // said as a word by a model, a number is a number; off its step, the nearest of its own
        assert!(set("water_heater.kettle#target", json!("85")).is_ok());
        assert_eq!(set("water_heater.kettle#target", json!(70.4)).unwrap().0.data, json!({"temperature": 70.0}));
        assert_eq!((on_step(21.3, 7.0, 0.5), on_step(0.42, 0.0, 0.01), on_step(33.0, 30.0, 5.0)), (21.5, 0.42, 35.0));
        // Home Assistant would pass 130° on to the kettle
        assert_eq!(set("water_heater.kettle#target", json!(130)).unwrap_err(), "вне диапазона: можно от 30 до 100 °C");
        // this heater is switched on (feature 8); one without the feature is put into its first mode
        assert_eq!(set("water_heater.kettle#power", json!(true)).unwrap().0.service, "turn_on");
        let mut modes = st(control(k, "water_heater.kettle#power"));
        modes["attributes"]["supported_features"] = json!(3);
        let (c, _) = call(control(k, "water_heater.kettle#power"), &modes, &json!("включить")).unwrap();
        assert_eq!((c.service, &c.data), ("set_operation_mode", &json!({"operation_mode": "performance"})));
        // an option by its label or its value, whatever the case
        let (c, want) = set("select.kettle_mode#option", json!("зеленый чай")).unwrap();
        assert_eq!((&c.data, want), (&json!({"option": "green"}), Want::Is(json!("green"))));
        assert_eq!(
            set("select.kettle_mode#option", json!("кофе")).unwrap_err(),
            "нет такого варианта; есть: Не выбрано, Зелёный чай, black"
        );
        assert_eq!(set("sensor.kettle_temperature#now", json!(50)).unwrap_err(), "это показание, его нельзя менять");
        assert_eq!(
            set("switch.kettle_backlight#power", json!("может быть")).unwrap_err(),
            "нужно «включить» или «выключить» (true или false)"
        );
        // a device that is away is not sent anything (Home Assistant answers «done» to that)
        let l = &h.devices[0];
        let p = control(l, "light.lamp#power");
        assert_eq!(call(p, &st(p), &json!(true)).unwrap_err(), "устройство не в сети");
    }

    #[test]
    fn near_enough_once_it_has_moved() {
        // a fan of three speeds asked for a half goes to two thirds: done; left where it was: not
        let fan = made("fan.x", "on", json!({"supported_features": 49, "percentage": 33, "percentage_step": 33.333}));
        let (c, want) = call(&fan[1], &json!({"attributes": {}}), &json!(50)).unwrap();
        assert_eq!((c.service, &c.data), ("set_percentage", &json!({"percentage": 66.0})));
        assert!(
            !want.already(&json!(33), "on")
                && want.met(&json!(67), "on", &json!(33))
                && !want.met(&json!(33), "on", &json!(33))
        );
        // a blind takes a minute: on its way is enough, where it was is not
        let blind = made(
            "cover.x",
            "closed",
            json!({"device_class": "blind", "supported_features": 15 | 128, "current_position": 0, "current_tilt_position": 50}),
        );
        assert_eq!(aspects(&blind), ["position", "tilt", "stop"]);
        let (c, want) = call(&blind[0], &json!({"attributes": {}}), &json!(60)).unwrap();
        assert_eq!((c.service, &c.data), ("set_cover_position", &json!({"position": 60.0})));
        assert!(
            want.met(&json!(5), "opening", &json!(0))
                && !want.met(&json!(0), "closed", &json!(0))
                && want.met(&json!(58), "open", &json!(0))
        );
        // closing, it passes the 60 it is asked for: not there «already» — told to stop there
        assert!(want.already(&json!(60), "open") && !want.already(&json!(60), "closing"));
        // …or nearing it, where its state does not say it moves (a louvre turning)
        assert!(want.met(&json!(10), "open", &json!(0)) && !want.met(&json!(0), "open", &json!(10)));
        assert_eq!(call(&blind[2], &json!({}), &json!(true)).unwrap().0.service, "stop_cover");
        assert_eq!(call(&blind[2], &json!({}), &json!(false)).unwrap_err(), "это действие: его можно только выполнить");
        // a lock on its way is locked soon; jammed, it never reads so
        let lock = made("lock.x", "unlocked", json!({"supported_features": 1}));
        assert_eq!((aspects(&lock), lock.iter().all(|c| !c.voice)), (vec!["locked", "unlatch"], true));
        let (c, want) = call(&lock[0], &json!({}), &json!(true)).unwrap();
        assert!(
            c.service == "lock"
                && want.met(&json!(false), "locking", &json!(false))
                && !want.met(&json!(false), "jammed", &json!(false))
        );
        // a colour comes back as the lamp could make it: not waited for; brightness through 255 steps is
        let lamp = made(
            "light.x",
            "on",
            json!({"supported_color_modes": ["rgbww"], "brightness": 128, "color_mode": "rgbww", "rgb_color": [250, 10, 5]}),
        );
        assert_eq!(
            lamp.iter().map(|c| (c.aspect, c.value.clone())).collect::<Vec<_>>(),
            [
                ("power", json!(true)),
                ("brightness", json!(50)),
                ("color_temp", Value::Null),
                ("color", json!("красный"))
            ]
        );
        let st = json!({"attributes": {"supported_color_modes": ["rgbww"]}});
        assert_eq!(
            call(&lamp[3], &st, &json!("Синий")).unwrap(),
            (
                Call {
                    domain: "light".into(),
                    service: "turn_on",
                    entity: "light.x".into(),
                    data: json!({"rgb_color": [0, 0, 255]})
                },
                Want::Done
            )
        );
        assert_eq!(call(&lamp[2], &st, &json!(3000)).unwrap().1, Want::Done);
        let (_, want) = call(&lamp[1], &st, &json!(51)).unwrap();
        assert!(
            !want.already(&json!(50), "on")
                && want.met(&json!(51), "on", &json!(50))
                && !want.met(&json!(50), "on", &json!(50))
        );
    }

    #[test]
    fn only_what_the_device_can_do() {
        assert_eq!(made("light.plain", "off", json!({"supported_color_modes": ["onoff"]})).len(), 1);
        // a fan with no way to switch it, a cover that only tilts, a humidifier with no target of its own
        assert_eq!(aspects(&made("fan.x", "on", json!({"supported_features": 1, "percentage": 50}))), ["speed"]);
        assert_eq!(
            aspects(&made("cover.x", "open", json!({"supported_features": 128, "current_tilt_position": 10}))),
            ["tilt"]
        );
        assert_eq!(
            aspects(&made(
                "humidifier.x",
                "on",
                json!({"supported_features": 1, "available_modes": ["auto"], "mode": "auto"})
            )),
            ["power", "mode"]
        );
        assert_eq!(
            aspects(&made(
                "humidifier.x",
                "on",
                json!({"humidity": 50, "min_humidity": 30, "max_humidity": 80, "target_humidity_step": 10, "current_humidity": 41})
            )),
            ["power", "target", "now"]
        );
        // a television: on, but neither playing nor paused — nothing to pause; with no way to wake it, told so; sound
        // through a soundbar is a step up or down
        let tv = made(
            "media_player.tv",
            "on",
            json!({"device_class": "tv", "supported_features": 256 | 1 | 16384 | 1024 | 8 | 2048, "is_volume_muted": false,
                                                    "source_list": ["HDMI 1", "YouTube"], "source": "YouTube"}),
        );
        assert_eq!(aspects(&tv), ["power", "louder", "quieter", "muted", "source"]);
        let st = json!({"attributes": {"supported_features": 256}});
        assert_eq!(
            call(&tv[0], &st, &json!(true)).unwrap_err(),
            "устройство не умеет включаться по команде из Home Assistant"
        );
        assert_eq!(call(&tv[0], &st, &json!(false)).unwrap().0.service, "turn_off");
        assert_eq!(call(&tv[1], &st, &json!(true)).unwrap().0.service, "volume_up");
        let player = made(
            "media_player.x",
            "paused",
            json!({"supported_features": 128 | 256 | 1 | 16384 | 4 | 16 | 32, "volume_level": 0.3, "media_title": "Песня"}),
        );
        assert_eq!(aspects(&player), ["power", "playing", "volume", "previous", "next", "title"]);
        assert_eq!((&player[1].value, &player[2].value), (&json!(false), &json!(30)));
        assert_eq!(
            call(&player[0], &json!({"attributes": {"supported_features": 384}}), &json!(true)).unwrap().1,
            Want::Done
        );
        // a thermostat that keeps between two: both are sent, the one not asked for as it is
        let st = json!({"attributes": {"supported_features": 3, "target_temp_low": 20.0, "target_temp_high": 24.0}});
        let keeps = made(
            "climate.x",
            "heat_cool",
            json!({"supported_features": 3, "hvac_modes": ["off", "heat_cool"], "temperature": null, "target_temp_low": 20.0,
                                                        "target_temp_high": 24.0, "min_temp": 7, "max_temp": 35, "hvac_action": "idle", "current_temperature": 21.5}),
        );
        assert_eq!(aspects(&keeps), ["mode", "low", "high", "does", "now"]);
        assert_eq!(
            call(&keeps[1], &st, &json!(21)).unwrap().0.data,
            json!({"target_temp_low": 21.0, "target_temp_high": 24.0})
        );
        assert_eq!(call(&keeps[1], &st, &json!(25)).unwrap_err(), "нижняя граница выше верхней");
        // a heater whose state says nothing yet still has its target
        let heater = made(
            "water_heater.x",
            "unknown",
            json!({"supported_features": 1 | 8, "temperature": 55, "min_temp": 30, "max_temp": 75, "target_temp_step": 5, "temperature_unit": "°F"}),
        );
        assert_eq!(
            (heater[0].value.clone(), heater[1].value.clone(), heater[1].step, heater[1].unit.as_deref()),
            (Value::Null, json!(55), Some(5.0), Some("°F"))
        );
        // a vacuum does what it says it can
        let vac = made(
            "vacuum.x",
            "docked",
            json!({"supported_features": 8192 | 8 | 16 | 512 | 32, "fan_speed_list": ["quiet", "max"], "fan_speed": "quiet"}),
        );
        assert_eq!(aspects(&vac), ["state", "start", "stop", "dock", "locate", "power"]);
        assert_eq!(call(&vac[2], &json!({}), &json!(true)).unwrap().0.service, "stop");
        // something this server has no control for is passed over, not guessed at
        assert!(made("camera.yard", "idle", json!({})).is_empty());
        assert!(made("siren.x", "off", json!({"supported_features": 1})).is_empty());
    }

    #[test]
    fn kinds_and_what_the_voice_may() {
        // a lock and a garage door, the water and a siren: by hand in the web UI, not by a voice from the street
        assert!(made("lock.door", "locked", json!({})).iter().all(|c| !c.voice));
        assert!(!made("cover.garage", "closed", json!({"device_class": "garage", "supported_features": 3}))[0].voice);
        assert!(
            made(
                "cover.blind",
                "open",
                json!({"device_class": "blind", "supported_features": 15, "current_position": 40})
            )[0]
            .voice
        );
        assert!(!made("valve.water", "open", json!({"supported_features": 3}))[0].voice);
        assert!(!made("siren.x", "off", json!({"supported_features": 3}))[0].voice);
        // a button that restarts the device, or stands among its settings: not the voice's to press
        assert!(made("button.go", "unknown", json!({}))[0].voice);
        assert!(!made("button.reboot", "unknown", json!({"device_class": "restart"}))[0].voice);
        let reset = entity("button.reset", json!("d"), "demo", json!("config"), "", "Reset");
        assert!(
            !Entity::new(&Raw::default(), &reset, &json!({"state": "unknown", "attributes": {}}), None).controls()[0]
                .voice
        );
        // of what the voice may not set it is still told the state; of what a device reports about itself, what is
        // asked about
        let lock = &made("lock.door", "locked", json!({"supported_features": 1}));
        assert!(told(&lock[0]) && !told(&lock[1]));
        let mut watts =
            made("sensor.w", "12.5", json!({"device_class": "power", "unit_of_measurement": "W"})).remove(0);
        watts.group = Group::Info;
        let mut rssi = made("sensor.r", "-40", json!({"device_class": "signal_strength"})).remove(0);
        rssi.group = Group::Info;
        assert!(told(&watts) && !told(&rssi));
        assert_eq!(kind_of(&[("switch", "outlet")]), "outlet");
        assert_eq!(kind_of(&[("sensor", "temperature"), ("media_player", "tv")]), "tv");
        assert_eq!(kind_of(&[("binary_sensor", "door")]), "sensor");
        assert_eq!(kind_of(&[("lawn_mower", "")]), "mower");
        assert_eq!(kind_of(&[]), "other");
        // an appliance whose buttons and programs are all among its settings is not «a sensor»; a device with a lock
        // is turned by hand, whatever of it
        let mut r = Raw::default();
        r.devices.push(json!({"id": "d", "name": "Мультиварка", "entry_type": null, "disabled_by": null}));
        r.entities.push(entity("sensor.pot_temperature", json!("d"), "x", Value::Null, "", "Температура"));
        r.entities.push(entity("select.pot_program", json!("d"), "x", json!("config"), "", "Программа"));
        r.states.extend([
            state("sensor.pot_temperature", "25", json!({})),
            state("select.pot_program", "soup", json!({"options": ["soup", "rice"]})),
        ]);
        assert_eq!(home(&r).devices[0].kind, "other");
        // of nothing but what it reports about itself (an update that waits): not a device of the home
        r.devices.push(json!({"id": "u", "name": "Прошивка", "entry_type": null, "disabled_by": null}));
        r.entities.push(entity("update.fw", json!("u"), "x", json!("diagnostic"), "", "Обновление"));
        r.states.extend([state("update.fw", "on", json!({}))]);
        assert_eq!(home(&r).devices.len(), 1);
        r.entities.push(entity("lock.pot_lid", json!("d"), "x", Value::Null, "", "Крышка"));
        r.states.extend([state("lock.pot_lid", "locked", json!({}))]);
        let pot = &home(&r).devices[0];
        assert!(pot.kind == "lock" && pot.controls.iter().all(|c| !c.voice));
    }
}
