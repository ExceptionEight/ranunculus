//! Web search through SearXNG (services/search).
use serde_json::{json, Value};
use std::time::Duration;

/// The part between «//» and the path (no punycode: «кто.рф» stays).
fn netloc(url: &str) -> String {
    let Some(i) = url.find("//") else { return String::new() };
    let rest = &url[i + 2..];
    rest.split(['/', '?', '#']).next().unwrap_or("").to_string()
}

pub(super) async fn get_json(
    http: &reqwest::Client,
    url: &str,
    q: &[(&str, String)],
    timeout: u64,
) -> Result<Value, String> {
    let r = http.get(url).query(q).timeout(Duration::from_secs(timeout)).send().await.map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("HTTP {}", r.status().as_u16()));
    }
    r.json().await.map_err(|e| e.to_string())
}

/// Top results from SearXNG: title, snippet, site, url (for read_page), date. The answer box / infobox goes first.
pub async fn search(http: &reqwest::Client, searxng: &str, query: &str) -> Result<Value, String> {
    let d =
        get_json(http, searxng, &[("q", query.to_string()), ("format", "json".into()), ("language", "ru".into())], 20)
            .await?;
    let mut out: Vec<Value> = d["results"].as_array().into_iter().flatten().take(6).map(|r| {
        let url = r["url"].as_str().unwrap_or("");
        let date: String = r["publishedDate"].as_str().unwrap_or("").chars().take(10).collect();
        json!({"title": r["title"].as_str().unwrap_or(""), "snippet": r["content"].as_str().unwrap_or("").chars().take(400).collect::<String>(),
               "site": netloc(url), "url": url, "date": if date.is_empty() { Value::Null } else { json!(date) }})
    }).collect();
    if let Some(ib) = d["infoboxes"].as_array().and_then(|a| a.first()) {
        out.insert(0, json!({"title": ib.get("infobox").cloned().unwrap_or(Value::Null),
                             "snippet": ib["content"].as_str().unwrap_or("").chars().take(400).collect::<String>(), "site": "справка"}));
    }
    Ok(if out.is_empty() {
        json!({"results": [], "note": "ничего не нашлось"})
    } else {
        json!({"results": out})
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netlocs() {
        assert_eq!(netloc("https://ru.wikipedia.org/wiki/Монеточка"), "ru.wikipedia.org");
        assert_eq!(netloc("https://кто.рф:8080?x=1"), "кто.рф:8080");
        assert_eq!(netloc("nonsense"), "");
    }
}
