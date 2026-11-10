//! Alarms (crate::alarms): listed, added, changed, deleted.
use super::*;
use crate::ring;

pub(super) const LABEL_MAX: usize = 60;

pub(super) async fn list_alarms(State(app): State<App>) -> R {
    ok(crate::alarms::list(&app.shared))
}

/// The fields of `v` into `a` (a new alarm: device and time required). → whether when it rings changed (then it is armed
/// anew: from now on).
pub(super) async fn alarm_fields(
    app: &App,
    v: &Value,
    a: &mut crate::store::AlarmRow,
    new: bool,
) -> Result<bool, Fail> {
    let o = v.as_object().ok_or_else(|| bad("нужен объект"))?;
    let mut rearm = new;
    for (k, x) in o {
        match k.as_str() {
            "device" => {
                let d = x.as_str().ok_or_else(|| bad("device: id колонки"))?;
                devices::known(app, d)?;
                a.device = d.to_string();
            }
            "time" => {
                let t = x.as_str().unwrap_or("");
                let m = t
                    .split_once(':')
                    .and_then(|(h, m)| Some((h.parse::<u32>().ok()?, m.parse::<u32>().ok()?)))
                    .filter(|(h, m)| *h < 24 && *m < 60 && t.len() == 5)
                    .ok_or_else(|| bad("time: «ЧЧ:ММ», например «07:30»"))?;
                rearm |= a.minute != m.0 * 60 + m.1;
                a.minute = m.0 * 60 + m.1;
            }
            "days" => {
                let arr = x.as_array().ok_or_else(|| bad("days: список дней 1 (пн) … 7 (вс), пустой — один раз"))?;
                let mut mask = 0u8;
                for d in arr {
                    let d = d
                        .as_u64()
                        .filter(|d| (1..=7).contains(d))
                        .ok_or_else(|| bad("days: числа от 1 (пн) до 7 (вс)"))?;
                    mask |= 1 << (d - 1);
                }
                rearm |= a.days != mask;
                a.days = mask;
            }
            "label" => {
                let l = x.as_str().ok_or_else(|| bad("label: текст"))?.trim().to_string();
                if l.chars().count() > LABEL_MAX || l.chars().any(|c| c.is_control()) {
                    return Err(bad(format!("label: до {LABEL_MAX} знаков")));
                }
                a.label = l;
            }
            "enabled" => {
                let e = x.as_bool().ok_or_else(|| bad("enabled: true или false"))?;
                rearm |= e && !a.enabled;
                a.enabled = e;
            }
            "sound" => {
                a.sound = match x {
                    Value::Null => None,
                    Value::String(n) => {
                        crate::sounds::load(&app.shared.sounds_dir, n)
                            .await
                            .map_err(|e| Fail::new(StatusCode::UNPROCESSABLE_ENTITY, e))?;
                        Some(n.clone())
                    }
                    _ => return Err(bad("sound: имя звука или null (звук будильников из настроек)")),
                }
            }
            "ring" => {
                a.ring = opt_num(
                    x,
                    |r| (ring::MIN..=ring::MAX).contains(&r),
                    &format!("ring: от {} до {} с или null", ring::MIN, ring::MAX),
                )?
            }
            "volume" => a.volume = opt_num(x, crate::settings::level_ok, "volume: от 0 до 10 с шагом 0,5 или null")?,
            "ramp" => {
                a.ramp = opt_num(x, |r| (0.0..=crate::settings::RAMP_MAX).contains(&r), "ramp: от 0 до 300 с или null")?
            }
            other => return Err(bad(format!("{other}: такого поля нет"))),
        }
    }
    if new && (a.device.is_empty() || !o.contains_key("time")) {
        return Err(bad("нужны device и time"));
    }
    Ok(rearm)
}

pub(super) fn alarm_by_id(app: &App, id: i64) -> Result<crate::store::AlarmRow, Fail> {
    app.store
        .alarms()
        .map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))?
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| Fail::new(StatusCode::NOT_FOUND, "нет такого будильника"))
}

pub(super) async fn add_alarm(State(app): State<App>, h: HeaderMap, body: Bytes) -> R {
    let v = json_body(&h, &body)?;
    let mut a = crate::store::AlarmRow {
        id: 0,
        device: String::new(),
        minute: 0,
        days: 0,
        label: String::new(),
        enabled: true,
        sound: None,
        ring: None,
        volume: None,
        ramp: None,
        armed: now_s(),
        rang: None,
    };
    alarm_fields(&app, &v, &mut a, true).await?;
    a.id = app.store.save_alarm(&a).map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    log(&a.device, format!("   (web: an alarm for {} added)", crate::alarms::time(a.minute)));
    crate::alarms::changed(&app.shared);
    Ok((StatusCode::CREATED, Json(crate::alarms::json(&a))).into_response())
}

pub(super) async fn change_alarm(State(app): State<App>, Path(id): Path<i64>, h: HeaderMap, body: Bytes) -> R {
    let v = json_body(&h, &body)?;
    let mut a = alarm_by_id(&app, id)?;
    if alarm_fields(&app, &v, &mut a, false).await? {
        a.armed = now_s();
    }
    app.store.save_alarm(&a).map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    crate::alarms::changed(&app.shared);
    ok(crate::alarms::json(&a))
}

pub(super) async fn delete_alarm(State(app): State<App>, Path(id): Path<i64>) -> R {
    let a = alarm_by_id(&app, id)?;
    app.store.delete_alarm(id).map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    log(&a.device, format!("   (web: the alarm for {} deleted)", crate::alarms::time(a.minute)));
    crate::alarms::changed(&app.shared);
    ok(json!({"deleted": id}))
}
