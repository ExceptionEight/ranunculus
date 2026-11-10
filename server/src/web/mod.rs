//! The web UI's server (`[web] listen`, default 0.0.0.0:52702; HTTPS with the server's certificate, or plain HTTP behind
//! a proxy that has it: tls.rs): the single-page app from `[web] ui` (a folder: what `web/` builds) and its API under
//! /api/ (api/). What guards it:
//! - the API wants a session (auth.rs): signed in by a password, over HTTPS (or on this machine) only;
//! - the client's address is the connection's, or — from a proxy of `[web] trusted_proxies` — the nearest one in
//!   X-Forwarded-For that is not a trusted proxy's (X-Forwarded-Proto says whether it came by HTTPS); nobody else's
//!   X-Forwarded-* is read: a client could say anything there;
//! - from where, and by what name, is not looked at (a VPS, a VPN, proxies: that is the network's and the reverse
//!   proxy's business, not the server's); the sign-in guards it;
//! - a browser's change (POST, PUT, PATCH, DELETE) must come from the UI's own origin (the Host's, or
//!   `public_url`'s: a proxy that does not pass the Host on), and the API takes nothing from
//!   another site (Sec-Fetch-Site: cross-site): no cross-site requests in the user's name. The app's own files do open
//!   from another site's link (a Home Assistant dashboard, a start page): they hold nothing private, and the page
//!   refuses to be framed (CSP frame-ancestors 'none', X-Frame-Options: DENY);
//! - JSON bodies only as application/json (a plain HTML form cannot send one).
//!
//! The app's files: one that exists is served (Vite's hashed `assets/` cached for good, the rest revalidated); any
//! other path without an extension is the client's route — index.html; a missing file with an extension is a 404.
mod api;
mod auth;
mod oidc;
mod tls;

pub use api::device;
pub use auth::{hash_password, name_ok, password_ok, PASSWORD_MIN};
pub use tls::Peer;

use crate::config;
use crate::session::{log, Shared};
use crate::store::Store;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A sound file's (modified, bytes) and its length in seconds, or why it does not play.
type Length = (std::time::SystemTime, u64, Result<f64, String>);

#[derive(Clone)]
pub struct App {
    pub shared: Arc<Shared>,
    pub store: Arc<Store>,
    ui: Arc<PathBuf>,
    /// `[memory] prewarm`: the prompt back into the model's cache after the preferences change (off in tests: the test
    /// server shares the model with the live one)
    pub prewarm: bool,
    /// each sound file's length, by (modified, bytes): decoding a long one takes a while
    lengths: Arc<Mutex<HashMap<String, Length>>>,
    auth: Arc<auth::Auth>,
    net: Arc<Net>,
    /// `[web] public_url`'s origin («https://home.example.org»)
    origin: Option<Arc<str>>,
    /// `[web] password`: signing in by password
    password: bool,
    /// `[web.oidc]`: signing in by a provider (None: off, or not set right — said at start)
    oidc: Option<Arc<oidc::Oidc>>,
}

pub async fn serve(cfg: config::Web, shared: Arc<Shared>, store: Arc<Store>, prewarm: bool) {
    // HTTPS that does not start is no web UI at all: never plain HTTP instead
    let acceptor = match cfg.tls.then(|| crate::listener::acceptor(&cfg.cert, &cfg.key)).transpose() {
        Ok(a) => a,
        Err(e) => {
            log("server", format!("the web UI is not started: {e} ([web] tls)"));
            return;
        }
    };
    let net = match Net::new(&cfg.trusted_proxies) {
        Ok(n) => n,
        Err(e) => {
            log("server", format!("the web UI is not started: [web] trusted_proxies: {e}"));
            return;
        }
    };
    let public = (!cfg.public_url.is_empty()).then(|| public_origin(&cfg.public_url)).transpose();
    let Ok(public) = public else {
        log(
            "server",
            format!(
                "the web UI is not started: [web] public_url «{}» is not an address like https://name",
                cfg.public_url
            ),
        );
        return;
    };
    let l = match tokio::net::TcpListener::bind(&cfg.listen).await {
        Ok(l) => l,
        Err(e) => {
            log("server", format!("web {}: {e}", cfg.listen));
            return;
        }
    };
    let ui = std::fs::canonicalize(&cfg.ui).unwrap_or_else(|_| PathBuf::from(&cfg.ui));
    if !ui.join("index.html").is_file() {
        log("server", format!("   (web: no index.html in {}: the UI is not built, only the API works)", ui.display()));
    }
    log(
        "server",
        format!(
            "the web UI on {}://{}/ (its files: {})",
            if acceptor.is_some() { "https" } else { "http" },
            cfg.listen,
            ui.display()
        ),
    );
    if acceptor.is_none() {
        log("server", "   (web without HTTPS: signing in works only through an HTTPS proxy of [web] trusted_proxies, or on this machine)");
    }
    if !store.has_users() && cfg.oidc.issuer.is_empty() {
        log("server", "   (web: no users yet: the first to open the page makes themselves one, so open it now)");
    }
    let oidc = match cfg.oidc.issuer.is_empty() {
        true => None,
        false => match oidc::Oidc::new(&cfg.oidc, public.as_ref()) {
            Ok(o) => {
                log(
                    "server",
                    format!("   (web: sign-in by {}: {}, groups {:?})", o.name, cfg.oidc.issuer, cfg.oidc.groups),
                );
                Some(Arc::new(o))
            }
            Err(e) => {
                log("server", format!("   (web: sign-in by a provider is off: [web.oidc]: {e})"));
                None
            }
        },
    };
    if !cfg.password && oidc.is_none() {
        log("server", "   (web: there is no way to sign in: [web] password = false, and no provider is set up)");
    }
    let app = App {
        shared: shared.clone(),
        store,
        ui: Arc::new(ui),
        prewarm,
        lengths: Default::default(),
        auth: Default::default(),
        net: Arc::new(net),
        origin: public.map(|(o, _)| o.into()),
        password: cfg.password,
        oidc,
    };
    tokio::spawn(api::watch_music(shared));
    let router =
        api::routes().fallback(files).layer(middleware::from_fn_with_state(app.clone(), guard)).with_state(app);
    if let Err(e) = axum::serve(tls::incoming(l, acceptor), router.into_make_service_with_connect_info::<Peer>()).await
    {
        log("server", format!("web: {e}"));
    }
}

// ---------------------------------------------------------------- errors

/// A refusal as the API gives it: {"error": "…"} with its status.
pub struct Fail(pub StatusCode, pub String);

impl Fail {
    pub fn new(code: StatusCode, msg: impl Into<String>) -> Fail {
        Fail(code, msg.into())
    }
}

impl IntoResponse for Fail {
    fn into_response(self) -> Response {
        (self.0, axum::Json(json!({"error": self.1}))).into_response()
    }
}

// ---------------------------------------------------------------- who is asking

/// The client as the guard settled it (for every request: the handlers take it as an Extension).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Client {
    pub ip: IpAddr,
    /// it came by HTTPS (or from this machine): a password may be given
    pub secure: bool,
}

/// `[web] trusted_proxies`.
#[derive(Debug, Default)]
pub struct Net {
    trusted: Vec<(IpAddr, u8)>,
}

/// ::ffff:a.b.c.d is a.b.c.d
fn canon(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(a) => a.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

fn within(ip: IpAddr, net: IpAddr, n: u8) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let m = if n == 0 { 0 } else { u32::MAX << (32 - n as u32) };
            u32::from(a) & m == u32::from(b) & m
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let m = if n == 0 { 0 } else { u128::MAX << (128 - n as u32) };
            u128::from(a) & m == u128::from(b) & m
        }
        _ => false,
    }
}

/// An address in X-Forwarded-For: «a.b.c.d», «a.b.c.d:port», «[v6]:port», «v6».
fn hop(s: &str) -> Option<IpAddr> {
    let s = s.trim().trim_matches('"');
    if let Ok(ip) = s.parse() {
        return Some(canon(ip));
    }
    let a = match s.strip_prefix('[') {
        Some(rest) => rest.split(']').next()?,
        None => s.rsplit_once(':').filter(|(a, _)| !a.contains(':'))?.0,
    };
    a.parse().ok().map(canon)
}

impl Net {
    /// «a.b.c.d», «a.b.c.d/n», a v6 one likewise.
    pub fn parse(s: &str) -> Result<(IpAddr, u8), String> {
        let (a, n) = match s.trim().split_once('/') {
            Some((a, n)) => (a, Some(n)),
            None => (s.trim(), None),
        };
        let ip: IpAddr = a.parse().map_err(|_| format!("«{s}» is not an address"))?;
        let ip = canon(ip);
        let max = if ip.is_ipv4() { 32 } else { 128 };
        let n = match n {
            Some(n) => n.parse::<u8>().ok().filter(|&n| n <= max).ok_or_else(|| format!("«{s}» is not a network"))?,
            None => max,
        };
        Ok((ip, n))
    }

    pub fn new(trusted: &[String]) -> Result<Net, String> {
        Ok(Net { trusted: trusted.iter().map(|s| Net::parse(s)).collect::<Result<_, _>>()? })
    }

    fn trusts(&self, ip: IpAddr) -> bool {
        self.trusted.iter().any(|&(net, n)| within(ip, net, n))
    }

    /// Who is asking: the connection, or — through a proxy trusted — whom it says it passes on.
    pub fn client(&self, peer: Peer, h: &HeaderMap) -> Client {
        let p = canon(peer.addr.ip());
        if !self.trusts(p) {
            return Client { ip: p, secure: peer.tls || p.is_loopback() };
        }
        // right to left (each proxy adds the one it got it from): the first that is not a trusted proxy's
        let mut ip = p;
        let hops = h
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .collect::<Vec<_>>();
        for s in hops.iter().rev() {
            match hop(s) {
                Some(a) => {
                    ip = a;
                    if !self.trusts(a) {
                        break;
                    }
                }
                None => break, // not an address: what is left of it is anyone's
            }
        }
        let proto = h
            .get_all("x-forwarded-proto")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .map(str::trim)
            .next_back();
        let secure = match proto {
            Some(p) => p.eq_ignore_ascii_case("https"),
            None => peer.tls || ip.is_loopback(),
        };
        Client { ip, secure }
    }
}

/// `[web] public_url` → (its origin, its host): «https://Home.Example.org:8443/» → («https://home.example.org:8443»,
/// «home.example.org»).
pub fn public_origin(url: &str) -> Result<(String, String), String> {
    let u = url.trim().to_ascii_lowercase();
    let rest = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .ok_or_else(|| format!("«{url}»: http:// or https:// first"))?;
    let hostport = rest.strip_suffix('/').unwrap_or(rest);
    let host = host_part(hostport);
    if hostport.contains(['/', '?', '#', '@']) || host.is_empty() {
        return Err(format!("«{url}»: an address of a site, nothing after it"));
    }
    let scheme = &u[..u.len() - rest.len()];
    // as a browser says it: no default port
    let hostport = hostport.strip_suffix(if scheme == "https://" { ":443" } else { ":80" }).unwrap_or(hostport);
    Ok((format!("{scheme}{hostport}"), host))
}

// ---------------------------------------------------------------- the guard

/// «host[:port]» → host, lower-cased (IPv6 in brackets kept as an address).
fn host_part(h: &str) -> String {
    let h = h.trim().to_ascii_lowercase();
    if let Some(rest) = h.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("").to_string();
    }
    match h.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) && !host.contains(':') => host.to_string(),
        _ => h,
    }
}

/// A browser's change must come from the page's own origin (or, on this machine, from a dev server on localhost).
fn origin_ok(origin: &str, host: &str, peer: IpAddr, public: Option<&str>) -> bool {
    if public.is_some_and(|p| origin.trim_end_matches('/').eq_ignore_ascii_case(p)) {
        return true;
    }
    let Some(o) = origin.strip_prefix("http://").or_else(|| origin.strip_prefix("https://")) else { return false };
    let o = o.trim_end_matches('/');
    if o.eq_ignore_ascii_case(host.trim()) {
        return true;
    }
    let oh = host_part(o);
    peer.is_loopback() && (oh == "localhost" || oh.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()))
}

fn is_api(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// What the guard goes by besides the request: `[web] public_url`'s origin.
#[derive(Default)]
struct Rules<'a> {
    origin: Option<&'a str>,
}

/// Why a request is refused (None: let in).
fn refusal(ip: IpAddr, method: &Method, path: &str, h: &HeaderMap, rules: &Rules) -> Option<&'static str> {
    let get = |k: header::HeaderName| h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("");
    let host = get(header::HOST);
    if is_api(path) && path != oidc::CALLBACK && h.get("sec-fetch-site").is_some_and(|v| v == "cross-site") {
        // the API (and its events) only: a link to the UI from another site must open it, not answer 403 on index.html
        // (the provider's way back is from its site: its state and cookie guard it, oidc.rs)
        Some("a request from another site")
    } else if !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
        && h.get(header::ORIGIN).is_some()
        && !origin_ok(get(header::ORIGIN), host, ip, rules.origin)
    {
        Some("a change not from the UI's own page")
    } else {
        None
    }
}

async fn guard(State(app): State<App>, ConnectInfo(peer): ConnectInfo<Peer>, mut req: Request, next: Next) -> Response {
    let client = app.net.client(peer, req.headers());
    let ip = client.ip;
    let rules = Rules { origin: app.origin.as_deref() };
    let api = is_api(req.uri().path());
    let mut resp = if let Some(why) = refusal(ip, req.method(), req.uri().path(), req.headers(), &rules) {
        let h = req.headers();
        let get = |k: header::HeaderName| h.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        log(
            "web",
            format!(
                "{ip}: {} {} refused: {why} (Host «{}», Origin «{}»)",
                req.method(),
                req.uri().path(),
                get(header::HOST),
                get(header::ORIGIN)
            ),
        );
        Fail::new(StatusCode::FORBIDDEN, why).into_response()
    } else if api && !auth::open(req.uri().path()) && !insert_authed(&app, &mut req) {
        Fail::new(StatusCode::UNAUTHORIZED, "Нужно войти").into_response()
    } else {
        req.extensions_mut().insert(client);
        next.run(req).await
    };
    let hs = resp.headers_mut();
    hs.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    hs.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    hs.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    if api {
        hs.entry(header::CACHE_CONTROL).or_insert(HeaderValue::from_static("no-store"));
    }
    resp
}

/// The request's session into it (→ there is one).
fn insert_authed(app: &App, req: &mut Request) -> bool {
    match auth::authed(&app.store, req.headers()) {
        Some(a) => {
            req.extensions_mut().insert(a);
            true
        }
        None => false,
    }
}

/// JSON from a request body: application/json only (a plain form cannot send it), an object.
pub fn json_body(h: &HeaderMap, body: &[u8]) -> Result<Value, Fail> {
    let ct = h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !ct.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json") {
        return Err(Fail::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "нужен application/json"));
    }
    let v: Value =
        serde_json::from_slice(body).map_err(|e| Fail::new(StatusCode::BAD_REQUEST, format!("не JSON: {e}")))?;
    if !v.is_object() {
        return Err(Fail::new(StatusCode::BAD_REQUEST, "нужен объект"));
    }
    Ok(v)
}

// ---------------------------------------------------------------- the app's files

fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain; charset=utf-8",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// A URL path → its segments, %-decoded; None: not a plain path inside the folder (`..`, hidden, a backslash…).
fn segments(path: &str) -> Option<Vec<String>> {
    let mut out = vec![];
    for raw in path.split('/').filter(|s| !s.is_empty()) {
        let b = raw.as_bytes();
        let mut v = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' {
                let h = b
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok())?;
                v.push(h);
                i += 3;
            } else {
                v.push(b[i]);
                i += 1;
            }
        }
        let s = String::from_utf8(v).ok()?;
        if s.starts_with('.') || s.contains(['/', '\\', '\0']) {
            return None;
        }
        out.push(s);
    }
    Some(out)
}

/// Which file answers `path`: the file itself, or — a client route (no extension) — the app's index.html. None: 404.
fn resolve(ui: &Path, path: &str) -> Option<PathBuf> {
    let segs = segments(path)?;
    let mut p = ui.to_path_buf();
    segs.iter().for_each(|s| p.push(s));
    let inside = |f: &Path| std::fs::canonicalize(f).ok().is_some_and(|c| c.starts_with(ui));
    if p.is_file() {
        return inside(&p).then_some(p);
    }
    if segs.last().is_some_and(|s| s.contains('.')) {
        return None; // a file that is not there: not the app instead
    }
    let i = ui.join("index.html");
    (i.is_file() && inside(&i)).then_some(i)
}

async fn files(State(app): State<App>, method: Method, uri: Uri) -> Response {
    if uri.path().starts_with("/api/") || uri.path() == "/api" {
        return Fail::new(StatusCode::NOT_FOUND, "нет такого адреса API").into_response();
    }
    if !matches!(method, Method::GET | Method::HEAD) {
        return Fail::new(StatusCode::METHOD_NOT_ALLOWED, "только GET").into_response();
    }
    let Some(f) = resolve(&app.ui, uri.path()) else {
        // the folder's name only: where it is is for the log (said at the start), not for anyone in the house
        let folder = app.ui.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        return if app.ui.join("index.html").is_file() {
            (StatusCode::NOT_FOUND, "нет такого файла").into_response()
        } else {
            (StatusCode::SERVICE_UNAVAILABLE, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
             format!("Интерфейс не собран: в папке «{folder}» нет index.html (web/: npm run build). API работает: /api/info")).into_response()
        };
    };
    let body = match tokio::fs::read(&f).await {
        Ok(b) => b,
        Err(e) => {
            log("web", format!("{}: {e}", f.display()));
            let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            return Fail::new(StatusCode::INTERNAL_SERVER_ERROR, format!("{name}: {e}")).into_response();
        }
    };
    let rel = f.strip_prefix(app.ui.as_path()).unwrap_or(&f);
    let cache = if rel.components().any(|c| c.as_os_str() == "assets") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let mut r = Response::new(if method == Method::HEAD { Body::empty() } else { Body::from(body) });
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime(&f)));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if mime(&f).starts_with("text/html") {
        h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; \
             font-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'; object-src 'none'"));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_parts() {
        assert_eq!(host_part("[fe80::1]:80"), "fe80::1");
        assert_eq!(host_part("fe80::1"), "fe80::1");
        assert_eq!(host_part("Home.Example.org:8443"), "home.example.org");
    }

    #[test]
    fn whose_changes() {
        let lan: IpAddr = "192.168.1.10".parse().unwrap();
        let me: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(origin_ok("http://192.168.1.65:52702", "192.168.1.65:52702", lan, None));
        assert!(origin_ok("http://PC.local:52702/", "pc.local:52702", lan, None));
        assert!(!origin_ok("http://evil.com", "192.168.1.65:52702", lan, None));
        assert!(
            !origin_ok("http://192.168.1.65:5173", "192.168.1.65:52702", lan, None),
            "another port is another origin"
        );
        assert!(!origin_ok("null", "192.168.1.65:52702", lan, None));
        // Vite's dev server on this machine proxies to the server
        assert!(origin_ok("http://localhost:5173", "127.0.0.1:52702", me, None));
        assert!(!origin_ok("http://localhost:5173", "192.168.1.65:52702", lan, None));
    }

    #[test]
    fn who_is_asking() {
        let peer = |a: &str, tls: bool| Peer { addr: a.parse().unwrap(), tls };
        let hs = |pairs: &[(&'static str, &str)]| {
            let mut h = HeaderMap::new();
            for (k, v) in pairs {
                h.append(*k, HeaderValue::from_str(v).unwrap());
            }
            h
        };
        let c = |ip: &str, secure: bool| Client { ip: ip.parse().unwrap(), secure };
        let none = Net::default();
        // no proxy trusted: the connection is the client, its X-Forwarded-* nobody's word
        let lies = hs(&[("x-forwarded-for", "127.0.0.1"), ("x-forwarded-proto", "https")]);
        assert_eq!(none.client(peer("192.168.1.10:5000", false), &lies), c("192.168.1.10", false));
        assert_eq!(none.client(peer("192.168.1.10:5000", true), &lies), c("192.168.1.10", true));
        assert_eq!(none.client(peer("127.0.0.1:5000", false), &HeaderMap::new()), c("127.0.0.1", true), "this machine");
        assert_eq!(none.client(peer("[::ffff:192.168.1.10]:5000", false), &HeaderMap::new()), c("192.168.1.10", false));
        // through a trusted proxy: the nearest address not of a trusted proxy; its proto
        let net = Net::new(&["192.168.1.33".into(), "10.42.0.0/16".into()]).unwrap();
        let px = peer("192.168.1.33:40000", false);
        assert_eq!(
            net.client(px, &hs(&[("x-forwarded-for", "192.168.1.10"), ("x-forwarded-proto", "https")])),
            c("192.168.1.10", true)
        );
        // what the client said itself stays left of what the proxy added
        assert_eq!(
            net.client(px, &hs(&[("x-forwarded-for", "127.0.0.1, 203.0.113.7"), ("x-forwarded-proto", "https")])),
            c("203.0.113.7", true)
        );
        assert_eq!(
            net.client(
                px,
                &hs(&[("x-forwarded-for", "203.0.113.7, 10.42.1.5"), ("x-forwarded-for", "10.42.0.9:3000")])
            ),
            c("203.0.113.7", false),
            "two proxies of ours, two headers; http"
        );
        assert_eq!(net.client(px, &hs(&[("x-forwarded-for", "garbage, 192.168.1.10")])), c("192.168.1.10", false));
        assert_eq!(
            net.client(px, &hs(&[("x-forwarded-for", "192.168.1.10, garbage")])),
            c("192.168.1.33", false),
            "not an address: stop there"
        );
        assert_eq!(
            net.client(px, &hs(&[("x-forwarded-for", "[2001:db8::1]:443"), ("x-forwarded-proto", "http, https")])),
            c("2001:db8::1", true),
            "the nearest proxy's proto"
        );
        assert_eq!(
            net.client(px, &HeaderMap::new()),
            c("192.168.1.33", false),
            "a trusted proxy that says nothing: itself, its own connection"
        );
        assert_eq!(net.client(peer("192.168.1.33:1", true), &HeaderMap::new()), c("192.168.1.33", true));
        // the networks
        assert_eq!(Net::parse("10.42.0.0/16"), Ok(("10.42.0.0".parse().unwrap(), 16)));
        assert_eq!(Net::parse("fd00::/8"), Ok(("fd00::".parse().unwrap(), 8)));
        assert_eq!(Net::parse("::ffff:10.0.0.1"), Ok(("10.0.0.1".parse().unwrap(), 32)));
        assert!(within("10.42.255.1".parse().unwrap(), "10.42.0.0".parse().unwrap(), 16));
        assert!(!within("10.43.0.1".parse().unwrap(), "10.42.0.0".parse().unwrap(), 16));
        assert!(within("8.8.8.8".parse().unwrap(), "0.0.0.0".parse().unwrap(), 0));
        assert!(!within("::1".parse().unwrap(), "0.0.0.0".parse().unwrap(), 0), "v6 is not in a v4 network");
        for bad in ["10.0.0.0/33", "x", "10.0.0/8", "fd00::/129", ""] {
            assert!(Net::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            public_origin("https://Home.Example.org/"),
            Ok(("https://home.example.org".into(), "home.example.org".into()))
        );
        assert_eq!(
            public_origin("https://home.example.org:443"),
            Ok(("https://home.example.org".into(), "home.example.org".into()))
        );
        assert_eq!(public_origin("http://pc.lan:52702"), Ok(("http://pc.lan:52702".into(), "pc.lan".into())));
        for bad in ["home.example.org", "https://", "https://x/y", "https://u@x", "ftp://x"] {
            assert!(public_origin(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn who_is_let_in() {
        let lan: IpAddr = "192.168.1.10".parse().unwrap();
        let hs = |pairs: &[(&'static str, &str)]| {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, HeaderValue::from_static("192.168.1.65:52702"));
            for (k, v) in pairs {
                h.insert(*k, HeaderValue::from_str(v).unwrap());
            }
            h
        };
        let why = |m: Method, path: &str, h: &HeaderMap| refusal(lan, &m, path, h, &Rules::default());
        let cross = hs(&[("sec-fetch-site", "cross-site")]);
        // a link from another site (a Home Assistant dashboard): the app opens, its files load
        for path in ["/", "/devices/station-1/talk", "/settings/sounds", "/assets/app-1a2b.js", "/favicon.svg"] {
            assert_eq!(why(Method::GET, path, &cross), None, "{path}");
        }
        assert_eq!(why(Method::HEAD, "/", &cross), None);
        // the API and its events: nothing from another site, reading included
        for path in ["/api/devices", "/api/events", "/api/sounds/x.wav/audio", "/api", "/api/"] {
            assert_eq!(why(Method::GET, path, &cross), Some("a request from another site"), "{path}");
        }
        assert!(why(
            Method::POST,
            "/api/alarms",
            &hs(&[("sec-fetch-site", "cross-site"), ("origin", "http://192.168.1.65:52702")])
        )
        .is_some());
        // a path that only starts like the API's is the app's
        assert_eq!(why(Method::GET, "/apix", &cross), None);
        // the page's own requests
        assert_eq!(why(Method::GET, "/api/devices", &hs(&[("sec-fetch-site", "same-origin")])), None);
        assert_eq!(
            why(
                Method::PATCH,
                "/api/settings",
                &hs(&[("sec-fetch-site", "same-origin"), ("origin", "http://192.168.1.65:52702")])
            ),
            None
        );
        // a change from another origin: refused, wherever it goes
        for path in ["/api/settings", "/"] {
            assert_eq!(
                why(Method::POST, path, &hs(&[("origin", "http://evil.com")])),
                Some("a change not from the UI's own page"),
                "{path}"
            );
        }
        // from anywhere, by any name (the network's and the proxy's business; the sign-in guards it)
        let mut far = HeaderMap::new();
        far.insert(header::HOST, HeaderValue::from_static("home.example.org"));
        assert_eq!(
            refusal("203.0.113.7".parse().unwrap(), &Method::GET, "/api/devices", &far, &Rules::default()),
            None
        );
        // behind a proxy that does not pass the Host on: public_url's origin is the page's
        let behind = Rules { origin: Some("https://home.example.org") };
        let mut h = hs(&[("origin", "https://home.example.org")]);
        h.insert(header::HOST, HeaderValue::from_static("192.168.1.65:52702"));
        assert_eq!(refusal(lan, &Method::POST, "/api/auth/login", &h, &behind), None);
        assert!(refusal(lan, &Method::POST, "/api/auth/login", &h, &Rules::default()).is_some());
    }

    #[test]
    fn the_apps_files() {
        let d = std::env::temp_dir().join(format!("server-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for f in [
            "index.html",
            "assets/app-1a2b.js",
            "a/index.html",
            "a/assets/x.css",
            "b/deep/index.html",
            "favicon.svg",
            ".env",
        ] {
            let p = d.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, f).unwrap();
        }
        std::fs::write(d.parent().unwrap().join(format!("server-ui-secret-{}", std::process::id())), "secret").unwrap();
        let ui = std::fs::canonicalize(&d).unwrap();
        let got = |p: &str| resolve(&ui, p).map(|f| f.strip_prefix(&ui).unwrap().to_string_lossy().to_string());
        assert_eq!(got("/"), Some("index.html".into()));
        assert_eq!(got("/assets/app-1a2b.js"), Some("assets/app-1a2b.js".into()));
        assert_eq!(got("/chats/station-1"), Some("index.html".into()), "a client route");
        for route in [
            "/devices/station-1/talk",
            "/devices/a%20b/settings",
            "/settings/sounds",
            "/analytics",
            "/devices/station-1/talk/",
        ] {
            assert_eq!(got(route), Some("index.html".into()), "{route}");
        }
        // one app: a folder (with an index.html of its own or not) is a client route too
        for route in ["/a/", "/a/settings/timer", "/b/deep/x", "/assets", "/assets/"] {
            assert_eq!(got(route), Some("index.html".into()), "{route}");
        }
        assert_eq!(got("/a/index.html"), Some("a/index.html".into()), "a file that is there is that file");
        assert_eq!(got("/%61/assets/x.css"), Some("a/assets/x.css".into()), "%-decoded");
        assert_eq!(got("/assets/missing.js"), None, "a missing file is not the app");
        for bad in [
            "/../server-ui-secret",
            "/%2e%2e/x",
            "/a/%2e%2e/%2e%2e/etc/passwd",
            "/.env",
            "/a%2fb",
            "/a%5cb",
            "/%00",
            "/%zz",
            "/%e2%28%a1",
        ] {
            assert_eq!(got(bad), None, "{bad}");
        }
        // a link out of the folder is not followed
        std::os::unix::fs::symlink(
            d.parent().unwrap().join(format!("server-ui-secret-{}", std::process::id())),
            d.join("link.txt"),
        )
        .unwrap();
        assert_eq!(got("/link.txt"), None);
        assert_eq!(mime(Path::new("x.JS")), "text/javascript; charset=utf-8");
        let _ = std::fs::remove_file(d.parent().unwrap().join(format!("server-ui-secret-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&d);
    }
}
