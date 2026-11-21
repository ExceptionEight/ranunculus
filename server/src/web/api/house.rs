//! The home's devices (house/): the connection to Home Assistant set and given up, the home as it is, a control set.
use super::*;

/// The connection and the home: {"status": off | connecting | on | failed, "error", "url", "token_tail", "name",
/// "version", "rooms": [{"id", "name"}], "devices": [{"id", "name", "room", "kind", "online", "controls": […]}]}. Of
/// the token only its last four characters are among it.
pub async fn house(State(app): State<App>) -> R {
    ok(app.shared.house.json())
}

/// {"url", "token"}: tried first — kept only if Home Assistant takes them. No "token": the one kept, for the same
/// address.
pub async fn connect(State(app): State<App>, h: HeaderMap, body: Bytes) -> R {
    let v = json_body(&h, &body)?;
    let url =
        v["url"].as_str().map(str::trim).filter(|u| !u.is_empty()).ok_or_else(|| bad("Нужен адрес Home Assistant"))?;
    let token = v["token"].as_str().unwrap_or("");
    if url.len() > 300 || token.len() > 2000 {
        return Err(bad("Слишком длинно"));
    }
    app.shared.house.connect(url, token).await.map_err(|e| Fail::new(StatusCode::UNPROCESSABLE_ENTITY, e))?;
    log("house", "   (web: connected to Home Assistant)");
    ok(app.shared.house.json())
}

pub async fn forget(State(app): State<App>) -> R {
    app.shared.house.forget().map_err(|e| Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    log("house", "   (web: Home Assistant forgotten)");
    ok(app.shared.house.json())
}

/// {"control": "light.lamp#brightness", "value": 40}: set and waited for. → {"value": what it reads now; null:
/// nothing that says}; 409 with why not (away, out of its range, not confirmed by the device), 404: no such control.
pub async fn set(State(app): State<App>, h: HeaderMap, body: Bytes) -> R {
    let v = json_body(&h, &body)?;
    let control = v["control"].as_str().ok_or_else(|| bad("нужен control"))?;
    let value = v.get("value").ok_or_else(|| bad("нужно value"))?;
    match app.shared.house.set(control, value, false).await {
        Ok(now) => ok(json!({"value": now})),
        Err(e) => {
            let code = if e == crate::house::NO_CONTROL { StatusCode::NOT_FOUND } else { StatusCode::CONFLICT };
            let mut text = e;
            if let Some(first) = text.chars().next() {
                text = first.to_uppercase().collect::<String>() + &text[first.len_utf8()..];
            }
            Err(Fail::new(code, text))
        }
    }
}
