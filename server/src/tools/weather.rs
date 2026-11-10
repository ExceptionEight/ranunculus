//! The weather: Open-Meteo, no key. A city is found by its name through Open-Meteo's geocoder; the home is the
//! settings' city.
use super::web::get_json;
use crate::text::round_to;
use serde_json::{json, Value};

/// A place the weather is told for.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    /// IANA name ("": the geocoder said none)
    pub timezone: String,
}

/// A city by its name (Open-Meteo's geocoder) → None: it knows no such place.
pub async fn locate(http: &reqwest::Client, city: &str) -> Result<Option<Place>, String> {
    let g = get_json(
        http,
        "https://geocoding-api.open-meteo.com/v1/search",
        &[("name", city.to_string()), ("count", "1".into()), ("language", "ru".into())],
        15,
    )
    .await?;
    let Some(r) = g["results"].as_array().and_then(|a| a.first()) else { return Ok(None) };
    let (Some(lat), Some(lon)) = (r["latitude"].as_f64(), r["longitude"].as_f64()) else { return Ok(None) };
    let country = r["country"].as_str().unwrap_or("");
    let name = format!(
        "{}{}",
        r["name"].as_str().unwrap_or(city),
        if !country.is_empty() && country != "Россия" { format!(", {country}") } else { String::new() }
    );
    Ok(Some(Place { name, lat, lon, timezone: r["timezone"].as_str().unwrap_or("").to_string() }))
}

fn wmo(code: &Value) -> &'static str {
    match code.as_i64().unwrap_or(-1) {
        0 => "ясно",
        1 => "преимущественно ясно",
        2 => "переменная облачность",
        3 => "пасмурно",
        45 => "туман",
        48 => "туман с изморозью",
        51 => "слабая морось",
        53 => "морось",
        55 => "сильная морось",
        56 | 57 => "ледяная морось",
        61 => "небольшой дождь",
        63 => "дождь",
        65 => "сильный дождь",
        66 | 67 => "ледяной дождь",
        71 => "небольшой снег",
        73 => "снег",
        75 => "сильный снег",
        77 => "снежная крупа",
        80 => "небольшой ливень",
        81 => "ливень",
        82 => "сильный ливень",
        85 => "снегопад",
        86 => "сильный снегопад",
        95 => "гроза",
        96 => "гроза с градом",
        99 => "гроза с сильным градом",
        _ => "",
    }
}

fn round0(v: &Value) -> Value {
    v.as_f64().map(|f| json!(f.round_ties_even() as i64)).unwrap_or(Value::Null)
}

/// The weather in `city`, or at home when none is named (or it is «дома»): now and the day's minimum and maximum,
/// precipitation, wind.
pub async fn weather(
    http: &reqwest::Client,
    city: Option<String>,
    day: Option<String>,
    home: Option<&Place>,
) -> Result<Value, String> {
    let at_home = |c: &str| ["дома", "дом"].contains(&c) || home.is_some_and(|h| h.name.to_lowercase() == c);
    let place = match city.filter(|c| !at_home(&c.trim().to_lowercase())) {
        Some(c) => match locate(http, &c).await? {
            Some(p) => p,
            None => return Ok(json!({"ok": false, "error": format!("не нашла город «{c}»")})),
        },
        None => match home {
            Some(h) => h.clone(),
            None => {
                return Ok(
                    json!({"ok": false, "error": "домашний город в настройках не указан: спроси, для какого города нужен прогноз"}),
                )
            }
        },
    };
    let (name, lat, lon) = (place.name, place.lat, place.lon);
    let w = get_json(http, "https://api.open-meteo.com/v1/forecast", &[
        ("latitude", lat.to_string()), ("longitude", lon.to_string()), ("timezone", "auto".into()), ("forecast_days", "3".into()),
        ("wind_speed_unit", "ms".into()),
        ("current", "temperature_2m,apparent_temperature,weather_code,wind_speed_10m,precipitation".into()),
        ("daily", "weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,precipitation_probability_max,wind_speed_10m_max".into()),
    ], 15).await?;
    let i = match day.as_deref().unwrap_or("today") {
        "tomorrow" => 1,
        "after_tomorrow" => 2,
        _ => 0,
    };
    let dl = &w["daily"];
    let day_name = ["сегодня", "завтра", "послезавтра"][i];
    let mut out = json!({"ok": true, "place": name, "day": day_name, "date": dl["time"][i],
                         "sky": wmo(&dl["weather_code"][i]), "temp_min": round0(&dl["temperature_2m_min"][i]), "temp_max": round0(&dl["temperature_2m_max"][i]),
                         "precip_mm": dl["precipitation_sum"][i].as_f64().map(|f| json!(round_to(f, 1))).unwrap_or(Value::Null),
                         "precip_chance_pct": dl["precipitation_probability_max"][i], "wind_max_ms": round0(&dl["wind_speed_10m_max"][i])});
    if i == 0 {
        let c = &w["current"];
        out["now"] = json!({"temp": round0(&c["temperature_2m"]), "feels_like": round0(&c["apparent_temperature"]), "sky": wmo(&c["weather_code"]),
                            "wind_ms": round0(&c["wind_speed_10m"])});
    }
    Ok(out)
}
