//! Signing in to the web UI. Who may: the users in server.db — with a password (`[web] password`),
//! or by an OIDC provider (`[web.oidc]`, oidc.rs: its users are made at their first sign-in). While there is no user,
//! the page offers to make one (/api/auth/register: the first to come, as Home Assistant and Jellyfin do; made in one
//! statement, two at once make one); then a signed-in user adds others, changes their own password (/api/users,
//! /api/auth/password); `ranunculus-server password NAME` on the server is for a forgotten one.
//! Every address of the API wants a session but /api/auth (who is signed in), its /login, /logout, /register, /oidc
//! and /oidc/callback. The app's files are not private: the page asks /api/auth and shows the form.
//!
//! - Passwords: Argon2id with OWASP's parameters (the crate's defaults), a salt each; a hash made with weaker ones is
//!   made again at the next sign-in. At least 8 characters, no «a digit and a capital» rules.
//! - A wrong name and a wrong password answer the same and take as long (a name not there is checked against a dummy).
//! - Tries: 5 free per address and per name, then a wait: 30 s, doubling up to 15 min; a success clears it. Never a
//!   lock for good (anyone could lock the users out). At most 4 checked at once (19 MiB of memory each).
//! - Signing in only over HTTPS, or on this machine (web/mod.rs: `Client::secure`).
//! - A session: 32 random bytes in a cookie (`__Host-`, Secure, HttpOnly, SameSite=Lax: Strict would leave it behind
//!   on a link from elsewhere); server.db keeps their SHA-256 only. 7 days unused, 30 days at most. A new one at every
//!   sign-in, the one the browser came with ended (no fixation); «выйти» ends it, «выйти везде» all of the user's; a
//!   new password ends them all. The event stream looks at its session every 30 s: one ended closes it (api.rs).
use super::{json_body, App, Client, Fail};
use crate::session::log;
use crate::store::{sql, Store};
use crate::text::now_s;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use rusqlite::{params, OptionalExtension};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub const COOKIE: &str = "__Host-ranunculus";
/// a session unused this long ends
pub const IDLE: f64 = 7.0 * 86400.0;
/// and any, this long after the sign-in
pub const LONGEST: f64 = 30.0 * 86400.0;
/// how seldom a session's «last used» is written (not on every request)
const SEEN_EVERY: f64 = 300.0;
pub const PASSWORD_MIN: usize = 8;
const PASSWORD_MAX: usize = 1024;
pub const NAME_MAX: usize = 64;
const FREE_TRIES: u32 = 5;
const FIRST_WAIT: f64 = 30.0;
const LONGEST_WAIT: f64 = 900.0;
const CHECKED_AT_ONCE: usize = 4;

/// Who a request is from: put into it by the guard (web/mod.rs) for the API's addresses.
#[derive(Clone, Debug, PartialEq)]
pub struct Authed {
    pub user: i64,
    pub name: String,
    /// the session, as server.db keeps it (the token's hash)
    pub session: String,
    /// when they proved who they are (the password given, the provider's sign-in)
    pub auth_t: f64,
    /// signed in by the OIDC provider (not a password)
    pub provider: bool,
}

/// The sign-in's state in memory: the failed tries, the checks going on.
pub struct Auth {
    tries: Mutex<HashMap<String, (u32, f64)>>,
    checking: tokio::sync::Semaphore,
}

impl Default for Auth {
    fn default() -> Auth {
        Auth { tries: Default::default(), checking: tokio::sync::Semaphore::new(CHECKED_AT_ONCE) }
    }
}

/// The addresses open without a session.
pub fn open(path: &str) -> bool {
    matches!(
        path,
        "/api/auth"
            | "/api/auth/login"
            | "/api/auth/logout"
            | "/api/auth/register"
            | super::oidc::START
            | super::oidc::CALLBACK
    )
}

// ---------------------------------------------------------------- passwords

pub fn password_ok(pw: &str) -> Result<(), String> {
    let n = pw.chars().count();
    if n < PASSWORD_MIN {
        Err(format!("пароль короче {PASSWORD_MIN} символов"))
    } else if n > PASSWORD_MAX {
        Err(format!("пароль длиннее {PASSWORD_MAX} символов"))
    } else {
        Ok(())
    }
}

/// A user's name as kept: trimmed, 1…64 characters, nothing unprintable.
pub fn name_ok(name: &str) -> Result<String, String> {
    let n = name.trim();
    if n.is_empty() {
        Err("пустое имя".into())
    } else if n.chars().count() > NAME_MAX {
        Err(format!("имя длиннее {NAME_MAX} символов"))
    } else if n.chars().any(char::is_control) {
        Err("в имени служебные символы".into())
    } else {
        Ok(n.to_string())
    }
}

pub fn hash_password(pw: &str) -> Result<String, String> {
    use argon2::PasswordHasher;
    // a random salt of the recommended length, drawn by the crate
    argon2::Argon2::default().hash_password(pw.as_bytes()).map(|h| h.to_string()).map_err(|e| format!("пароль: {e}"))
}

/// (it matches, its hash is weaker than one made now — to be made again)
fn verify(pw: &str, phc: &str) -> (bool, bool) {
    use argon2::{PasswordHash, PasswordVerifier};
    let Ok(h) = PasswordHash::new(phc) else { return (false, false) };
    // the parameters come from the hash itself: an older one still checks
    let ok = argon2::Argon2::default().verify_password(pw.as_bytes(), &h).is_ok();
    let now = argon2::Params::default();
    let weaker = h.algorithm != argon2::Algorithm::Argon2id.ident()
        || argon2::Params::try_from(&h).map_or(true, |p| p.m_cost() < now.m_cost() || p.t_cost() < now.t_cost());
    (ok, ok && weaker)
}

/// What a name that is not there is checked against: as long as a real one.
fn dummy() -> &'static str {
    static D: OnceLock<String> = OnceLock::new();
    D.get_or_init(|| hash_password(&token()).unwrap_or_default())
}

// ---------------------------------------------------------------- tokens and the cookie

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(super) fn token() -> String {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 32];
    ring::rand::SystemRandom::new().fill(&mut b).expect("the system's random numbers");
    hex(&b)
}

/// A session as server.db knows it: its token's SHA-256 (a copy of the base gives no one a way in).
pub fn token_hash(t: &str) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, t.as_bytes()).as_ref())
}

/// The session token the request brings, if it looks like one.
pub fn token_of(h: &HeaderMap) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().strip_prefix(COOKIE)?.strip_prefix('='))
        .find(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|t| t.to_ascii_lowercase())
}

pub(super) fn cookie(token: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; Path=/; Max-Age={}; Secure; HttpOnly; SameSite=Lax",
        LONGEST as u64
    ))
    .expect("a cookie")
}

fn no_cookie() -> HeaderValue {
    HeaderValue::from_str(&format!("{COOKIE}=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Lax")).expect("a cookie")
}

/// The request's session, if it is one (and still is).
pub fn authed(store: &Store, h: &HeaderMap) -> Option<Authed> {
    let t = token_of(h)?;
    match store.session(&token_hash(&t), now_s()) {
        Ok(a) => a,
        Err(e) => {
            log("web", format!("session: {e}"));
            None
        }
    }
}

// ---------------------------------------------------------------- the tries

/// How long to wait after `fails` failed tries in a row.
fn wait(fails: u32) -> f64 {
    if fails < FREE_TRIES {
        0.0
    } else {
        (FIRST_WAIT * 2f64.powi((fails - FREE_TRIES).min(16) as i32)).min(LONGEST_WAIT)
    }
}

impl Auth {
    /// Seconds before another try may be made (0: now): the longest of the keys'.
    fn wait_left(&self, keys: &[String], now: f64) -> f64 {
        let m = self.tries.lock().unwrap();
        keys.iter().filter_map(|k| m.get(k)).map(|&(n, last)| (last + wait(n) - now).max(0.0)).fold(0.0, f64::max)
    }

    fn failed(&self, keys: &[String], now: f64) {
        let mut m = self.tries.lock().unwrap();
        m.retain(|_, &mut (_, last)| now - last < 86400.0);
        for k in keys {
            let e = m.entry(k.clone()).or_insert((0, now));
            e.0 += 1;
            e.1 = now;
        }
    }

    fn passed(&self, keys: &[String]) {
        let mut m = self.tries.lock().unwrap();
        keys.iter().for_each(|k| {
            m.remove(k);
        });
    }
}

// ---------------------------------------------------------------- server.db

/// A name as it is looked up: in any case («Аня» is «аня»: SQLite's NOCASE folds ASCII only).
fn key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// A user as `server users` and /api/users list them.
#[derive(Debug, Clone, PartialEq)]
pub struct UserRow {
    pub id: i64,
    pub name: String,
    pub created: f64,
    pub changed: f64,
    pub sessions: i64,
    /// the OIDC provider they sign in by (None: a password)
    pub via: Option<String>,
}

impl Store {
    /// Makes the user, or gives them a new password and ends their sessions. → made (was not there)
    pub fn set_password(&self, name: &str, phc: &str) -> Result<bool, String> {
        let db = self.db.lock().unwrap();
        let tx = db.unchecked_transaction().map_err(sql)?;
        let now = now_s();
        let id: Option<i64> =
            tx.query_row("SELECT id FROM users WHERE key = ?1", [key(name)], |r| r.get(0)).optional().map_err(sql)?;
        match id {
            Some(id) => {
                tx.execute("UPDATE users SET password = ?1, changed = ?2 WHERE id = ?3", params![phc, now, id])
                    .map_err(sql)?;
                tx.execute("DELETE FROM sessions WHERE user = ?1", [id]).map_err(sql)?;
            }
            None => {
                tx.execute(
                    "INSERT INTO users (name, key, password, created, changed) VALUES (?1, ?2, ?3, ?4, ?4)",
                    params![name, key(name), phc, now],
                )
                .map_err(sql)?;
            }
        }
        tx.commit().map_err(sql)?;
        Ok(id.is_none())
    }

    /// A password user by name, or else a provider's one (their names are not unique: the first). → the name as it was
    /// kept (None: no such user)
    pub fn remove_user(&self, name: &str) -> Result<Option<String>, String> {
        let db = self.db.lock().unwrap();
        let tx = db.unchecked_transaction().map_err(sql)?;
        let mut found: Option<(i64, String)> = tx
            .query_row("SELECT id, name FROM users WHERE key = ?1", [key(name)], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(sql)?;
        if found.is_none() {
            let mut q = tx.prepare("SELECT id, name FROM users WHERE key IS NULL ORDER BY id").map_err(sql)?;
            let all = q
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
                .map_err(sql)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql)?;
            found = all.into_iter().find(|(_, n)| key(n) == key(name));
        }
        if let Some((id, _)) = &found {
            tx.execute("DELETE FROM sessions WHERE user = ?1", [id]).map_err(sql)?;
            tx.execute("DELETE FROM users WHERE id = ?1", [id]).map_err(sql)?;
        }
        tx.commit().map_err(sql)?;
        Ok(found.map(|(_, n)| n))
    }

    pub fn users(&self) -> Result<Vec<UserRow>, String> {
        let db = self.db.lock().unwrap();
        let mut q = db
            .prepare("SELECT id, name, created, changed, (SELECT count(*) FROM sessions s WHERE s.user = u.id), issuer FROM users u ORDER BY created, id")
            .map_err(sql)?;
        let rows = q
            .query_map([], |r| {
                Ok(UserRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    created: r.get(2)?,
                    changed: r.get(3)?,
                    sessions: r.get(4)?,
                    via: r.get(5)?,
                })
            })
            .map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }

    /// The first user — only while there is none (one statement: two at once make one). → its id; None: there is one
    pub fn first_user(&self, name: &str, phc: &str) -> Result<Option<i64>, String> {
        let db = self.db.lock().unwrap();
        let n = db
            .execute("INSERT INTO users (name, key, password, created, changed) SELECT ?1, ?2, ?3, ?4, ?4 WHERE NOT EXISTS (SELECT 1 FROM users)",
                     params![name, key(name), phc, now_s()])
            .map_err(sql)?;
        Ok((n == 1).then(|| db.last_insert_rowid()))
    }

    /// Another password user (by a signed-in one). → its id; None: the name is taken (in any case)
    pub fn add_user(&self, name: &str, phc: &str) -> Result<Option<i64>, String> {
        let db = self.db.lock().unwrap();
        let n = db
            .execute("INSERT INTO users (name, key, password, created, changed) VALUES (?1, ?2, ?3, ?4, ?4) ON CONFLICT (key) DO NOTHING",
                     params![name, key(name), phc, now_s()])
            .map_err(sql)?;
        Ok((n == 1).then(|| db.last_insert_rowid()))
    }

    /// The user and their sessions gone. → the name it had (None: no such)
    pub fn remove_user_id(&self, id: i64) -> Result<Option<String>, String> {
        let db = self.db.lock().unwrap();
        let tx = db.unchecked_transaction().map_err(sql)?;
        let name: Option<String> =
            tx.query_row("SELECT name FROM users WHERE id = ?1", [id], |r| r.get(0)).optional().map_err(sql)?;
        if name.is_some() {
            tx.execute("DELETE FROM sessions WHERE user = ?1", [id]).map_err(sql)?;
            tx.execute("DELETE FROM users WHERE id = ?1", [id]).map_err(sql)?;
        }
        tx.commit().map_err(sql)?;
        Ok(name)
    }

    /// A user's password hash (None: none — a provider's).
    fn password_hash(&self, id: i64) -> Result<Option<String>, String> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT password FROM users WHERE id = ?1", [id], |r| r.get(0))
            .optional()
            .map(Option::flatten)
            .map_err(sql)
    }

    /// A new password: every session of the user ended.
    fn new_password(&self, id: i64, phc: &str) -> Result<(), String> {
        let db = self.db.lock().unwrap();
        let tx = db.unchecked_transaction().map_err(sql)?;
        tx.execute("UPDATE users SET password = ?1, changed = ?2 WHERE id = ?3", params![phc, now_s(), id])
            .map_err(sql)?;
        tx.execute("DELETE FROM sessions WHERE user = ?1", [id]).map_err(sql)?;
        tx.commit().map_err(sql)
    }

    pub fn has_users(&self) -> bool {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT EXISTS (SELECT 1 FROM users)", [], |r| r.get::<_, bool>(0)).unwrap_or(false)
    }

    /// (id, the name as kept, the password's hash)
    fn login_of(&self, name: &str) -> Result<Option<(i64, String, Option<String>)>, String> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT id, name, password FROM users WHERE key = ?1", [key(name)], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()
        .map_err(sql)
    }

    /// The same password, hashed anew (stronger parameters): the sessions stay.
    fn rehash(&self, user: i64, phc: &str) -> Result<(), String> {
        let db = self.db.lock().unwrap();
        db.execute("UPDATE users SET password = ?1 WHERE id = ?2", params![phc, user]).map_err(sql)?;
        Ok(())
    }

    /// A new session (and the ones over their time gone); `auth_t`: when they proved who they are.
    pub(super) fn open_session(
        &self,
        hash: &str,
        user: i64,
        now: f64,
        auth_t: f64,
        ip: &str,
        agent: &str,
    ) -> Result<(), String> {
        let db = self.db.lock().unwrap();
        db.execute("DELETE FROM sessions WHERE seen < ?1 OR t < ?2", params![now - IDLE, now - LONGEST])
            .map_err(sql)?;
        db.execute(
            "INSERT INTO sessions (hash, user, t, seen, auth_t, ip, agent) VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6)",
            params![hash, user, now, auth_t, ip, agent],
        )
        .map_err(sql)?;
        Ok(())
    }

    /// The session, if it is one and not over its time (then it is ended here); its «last used» kept up.
    pub fn session(&self, hash: &str, now: f64) -> Result<Option<Authed>, String> {
        let db = self.db.lock().unwrap();
        let row: Option<(i64, String, f64, f64, f64, bool)> = db
            .query_row("SELECT s.user, u.name, s.t, s.seen, s.auth_t, u.issuer IS NOT NULL FROM sessions s JOIN users u ON u.id = s.user WHERE s.hash = ?1",
                       [hash], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
            .optional()
            .map_err(sql)?;
        let Some((user, name, t, seen, auth_t, provider)) = row else { return Ok(None) };
        if now - seen > IDLE || now - t > LONGEST {
            db.execute("DELETE FROM sessions WHERE hash = ?1", [hash]).map_err(sql)?;
            return Ok(None);
        }
        if now - seen > SEEN_EVERY {
            db.execute("UPDATE sessions SET seen = ?1 WHERE hash = ?2", params![now, hash]).map_err(sql)?;
        }
        Ok(Some(Authed { user, name, session: hash.to_string(), auth_t, provider }))
    }

    pub fn end_session(&self, hash: &str) -> Result<(), String> {
        let db = self.db.lock().unwrap();
        db.execute("DELETE FROM sessions WHERE hash = ?1", [hash]).map_err(sql)?;
        Ok(())
    }

    /// → how many
    pub fn end_sessions(&self, user: i64) -> Result<usize, String> {
        let db = self.db.lock().unwrap();
        db.execute("DELETE FROM sessions WHERE user = ?1", [user]).map_err(sql)
    }
}

// ---------------------------------------------------------------- the API

fn internal(e: String) -> Fail {
    log("web", format!("sign-in: {e}"));
    Fail::new(StatusCode::INTERNAL_SERVER_ERROR, e)
}

/// GET /api/auth: who is signed in (null: no one); how one may: `password`, `oidc` ({name, start}: where the button
/// goes); `setup`: no user yet — the page offers to make the first; `secure`: this connection may take a password.
pub async fn state(State(app): State<App>, Extension(c): Extension<Client>, h: HeaderMap) -> Response {
    let user = authed(&app.store, &h);
    let oidc = app.oidc.as_ref().map(|o| json!({"name": o.name, "start": o.start()}));
    let via = |a: &Authed| a.provider.then(|| app.oidc.as_ref().map(|o| o.name.clone()).unwrap_or_default());
    Json(json!({"user": user.map(|a| json!({"name": a.name, "via": via(&a)})), "password": app.password, "oidc": oidc,
                "setup": app.password && !app.store.has_users(), "secure": c.secure}))
    .into_response()
}

fn too_many(left: f64) -> Response {
    let min = (left / 60.0).ceil().max(1.0) as u64;
    let mut r = Fail::new(StatusCode::TOO_MANY_REQUESTS, format!("Слишком много попыток. Попробуйте через {min} мин."))
        .into_response();
    r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(left.ceil() as u64));
    r
}

/// POST /api/auth/login {name, password} → {user: {name}} and the session's cookie.
pub async fn login(
    State(app): State<App>,
    Extension(c): Extension<Client>,
    h: HeaderMap,
    body: Bytes,
) -> Result<Response, Fail> {
    if !app.password {
        return Err(Fail::new(StatusCode::FORBIDDEN, "Вход по паролю выключен"));
    }
    if !c.secure {
        return Err(Fail::new(
            StatusCode::FORBIDDEN,
            "Вход только по защищённому соединению: откройте адрес с https://",
        ));
    }
    let v = json_body(&h, &body)?;
    let name = v["name"].as_str().unwrap_or("").trim().to_string();
    let pw = v["password"].as_str().unwrap_or("").to_string();
    if name.is_empty() || pw.is_empty() {
        return Err(Fail::new(StatusCode::BAD_REQUEST, "Введите имя и пароль"));
    }
    let keys = [format!("ip {}", c.ip), format!("name {}", key(&name))];
    let auth = &app.auth;
    let left = auth.wait_left(&keys, now_s());
    if left > 0.0 {
        return Ok(too_many(left));
    }
    let permit = auth
        .checking
        .acquire()
        .await
        .map_err(|_| Fail::new(StatusCode::SERVICE_UNAVAILABLE, "сервер останавливается"))?;
    // others may have failed while this one waited its turn
    let left = auth.wait_left(&keys, now_s());
    if left > 0.0 {
        return Ok(too_many(left));
    }
    let found = if name.chars().count() > NAME_MAX || pw.chars().count() > PASSWORD_MAX {
        None
    } else {
        app.store.login_of(&name).map_err(internal)?
    };
    let phc = found.as_ref().and_then(|f| f.2.clone());
    let (ok, rehash) = tokio::task::spawn_blocking(move || {
        let Some(p) = phc else {
            let _ = verify(&pw, dummy());
            return (false, None);
        };
        let (ok, weaker) = verify(&pw, &p);
        (ok, if weaker { hash_password(&pw).ok() } else { None })
    })
    .await
    .unwrap_or((false, None));
    drop(permit);
    let now = now_s();
    let Some((id, real, _)) = found.filter(|_| ok) else {
        auth.failed(&keys, now);
        log(
            "web",
            format!(
                "{}: sign-in as «{}»: a wrong name or password",
                c.ip,
                name.chars().take(NAME_MAX).collect::<String>()
            ),
        );
        return Err(Fail::new(StatusCode::UNAUTHORIZED, "Неверное имя или пароль"));
    };
    auth.passed(&keys);
    if let Some(p) = rehash {
        app.store.rehash(id, &p).map_err(internal)?;
    }
    log("web", format!("{}: «{}» signed in", c.ip, real.escape_debug()));
    let mut r = Json(json!({"user": {"name": real, "via": null}})).into_response();
    sign_in(&app, &c, &h, id, &mut r)?;
    Ok(r)
}

/// POST /api/auth/logout {all?}: this session ended (all: every one of the user's), the cookie gone.
pub async fn logout(
    State(app): State<App>,
    Extension(c): Extension<Client>,
    h: HeaderMap,
    body: Bytes,
) -> Result<Response, Fail> {
    let all = !body.is_empty() && json_body(&h, &body)?["all"].as_bool().unwrap_or(false);
    if let Some(a) = authed(&app.store, &h) {
        if all {
            let n = app.store.end_sessions(a.user).map_err(internal)?;
            log("web", format!("{}: «{}» signed out everywhere ({n})", c.ip, a.name));
        } else {
            app.store.end_session(&a.session).map_err(internal)?;
            log("web", format!("{}: «{}» signed out", c.ip, a.name));
        }
    }
    let mut r = Json(json!({"ok": true})).into_response();
    r.headers_mut().insert(header::SET_COOKIE, no_cookie());
    Ok(r)
}

/// A new session for this browser: its cookie on `r` (the one it came with ended).
fn sign_in(app: &App, c: &Client, h: &HeaderMap, user: i64, r: &mut Response) -> Result<(), Fail> {
    if let Some(old) = token_of(h) {
        app.store.end_session(&token_hash(&old)).map_err(internal)?;
    }
    let t = token();
    let agent: String =
        h.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").chars().take(200).collect();
    let now = now_s();
    app.store.open_session(&token_hash(&t), user, now, now, &c.ip.to_string(), &agent).map_err(internal)?;
    r.headers_mut().insert(header::SET_COOKIE, cookie(&t));
    Ok(())
}

/// A name and a password from a request, checked as a new user's would be.
fn new_user_of(h: &HeaderMap, body: &[u8]) -> Result<(String, String), Fail> {
    let v = json_body(h, body)?;
    let name =
        name_ok(v["name"].as_str().unwrap_or("")).map_err(|e| Fail::new(StatusCode::BAD_REQUEST, capital(&e)))?;
    let pw = v["password"].as_str().unwrap_or("").to_string();
    password_ok(&pw).map_err(|e| Fail::new(StatusCode::BAD_REQUEST, capital(&e)))?;
    Ok((name, pw))
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// The hash made off the async threads, at most CHECKED_AT_ONCE at a time (19 MiB each).
async fn hashed(app: &App, pw: String) -> Result<String, Fail> {
    let _permit = app
        .auth
        .checking
        .acquire()
        .await
        .map_err(|_| Fail::new(StatusCode::SERVICE_UNAVAILABLE, "сервер останавливается"))?;
    tokio::task::spawn_blocking(move || hash_password(&pw))
        .await
        .map_err(|e| internal(e.to_string()))?
        .map_err(internal)
}

/// POST /api/auth/register {name, password}: the first user, while there is none → signed in as them.
pub async fn register(
    State(app): State<App>,
    Extension(c): Extension<Client>,
    h: HeaderMap,
    body: Bytes,
) -> Result<Response, Fail> {
    if !app.password {
        return Err(Fail::new(StatusCode::FORBIDDEN, "Вход по паролю выключен"));
    }
    if !c.secure {
        return Err(Fail::new(StatusCode::FORBIDDEN, "Только по защищённому соединению: откройте адрес с https://"));
    }
    if body.len() > 4096 {
        return Err(Fail::new(StatusCode::PAYLOAD_TOO_LARGE, "Слишком длинно"));
    }
    if app.store.has_users() {
        return Err(Fail::new(StatusCode::CONFLICT, "Пользователь уже есть. Войдите."));
    }
    let (name, pw) = new_user_of(&h, &body)?;
    let phc = hashed(&app, pw).await?;
    let Some(id) = app.store.first_user(&name, &phc).map_err(internal)? else {
        return Err(Fail::new(StatusCode::CONFLICT, "Пользователь уже есть. Войдите."));
    };
    log("web", format!("{}: the first user is made: «{}»", c.ip, name.escape_debug()));
    let mut r = (StatusCode::CREATED, Json(json!({"user": {"name": name, "via": null}}))).into_response();
    sign_in(&app, &c, &h, id, &mut r)?;
    Ok(r)
}

/// POST /api/auth/password {current, password}: a password user's own, the current one given; every other session
/// of theirs ended, this browser signed in anew.
pub async fn change_password(
    State(app): State<App>,
    Extension(c): Extension<Client>,
    Extension(a): Extension<Authed>,
    h: HeaderMap,
    body: Bytes,
) -> Result<Response, Fail> {
    if a.provider {
        return Err(Fail::new(StatusCode::BAD_REQUEST, "Вход выполнен через провайдера: пароль меняется там"));
    }
    if body.len() > 4096 {
        return Err(Fail::new(StatusCode::PAYLOAD_TOO_LARGE, "Слишком длинно"));
    }
    let v = json_body(&h, &body)?;
    let current = v["current"].as_str().unwrap_or("").to_string();
    let pw = v["password"].as_str().unwrap_or("").to_string();
    password_ok(&pw).map_err(|e| Fail::new(StatusCode::BAD_REQUEST, capital(&e)))?;
    let keys = [format!("ip {}", c.ip), format!("name {}", key(&a.name))];
    let left = app.auth.wait_left(&keys, now_s());
    if left > 0.0 {
        return Ok(too_many(left));
    }
    let Some(phc) = app.store.password_hash(a.user).map_err(internal)? else {
        return Err(Fail::new(StatusCode::BAD_REQUEST, "У этого пользователя нет пароля"));
    };
    let ok = {
        let _permit = app
            .auth
            .checking
            .acquire()
            .await
            .map_err(|_| Fail::new(StatusCode::SERVICE_UNAVAILABLE, "сервер останавливается"))?;
        tokio::task::spawn_blocking(move || verify(&current, &phc).0).await.unwrap_or(false)
    };
    if !ok {
        app.auth.failed(&keys, now_s());
        log("web", format!("{}: «{}»: a password change with a wrong current one", c.ip, a.name.escape_debug()));
        return Err(Fail::new(StatusCode::FORBIDDEN, "Текущий пароль неверный"));
    }
    app.auth.passed(&keys);
    let phc = hashed(&app, pw).await?;
    app.store.new_password(a.user, &phc).map_err(internal)?;
    log("web", format!("{}: «{}» changed the password: signed out on the other devices", c.ip, a.name.escape_debug()));
    let mut r = Json(json!({"ok": true})).into_response();
    sign_in(&app, &c, &h, a.user, &mut r)?;
    Ok(r)
}

/// GET /api/users: everyone who may sign in (`me`: who asks; `via`: the provider's name, for its users).
pub async fn users(State(app): State<App>, Extension(a): Extension<Authed>) -> Result<Response, Fail> {
    let provider = app.oidc.as_ref().map(|o| o.name.clone());
    let items: Vec<_> = app
        .store
        .users()
        .map_err(internal)?
        .into_iter()
        .map(|u| {
            let via = u.via.as_ref().map(|iss| provider.clone().unwrap_or_else(|| iss.clone()));
            json!({"id": u.id, "name": u.name, "via": via, "created": u.created, "sessions": u.sessions, "me": u.id == a.user})
        })
        .collect();
    Ok(Json(json!({"items": items})).into_response())
}

/// POST /api/users {name, password}: another user with a password (anyone signed in may: one role).
pub async fn add_user(
    State(app): State<App>,
    Extension(c): Extension<Client>,
    Extension(a): Extension<Authed>,
    h: HeaderMap,
    body: Bytes,
) -> Result<Response, Fail> {
    if body.len() > 4096 {
        return Err(Fail::new(StatusCode::PAYLOAD_TOO_LARGE, "Слишком длинно"));
    }
    let (name, pw) = new_user_of(&h, &body)?;
    let phc = hashed(&app, pw).await?;
    let Some(id) = app.store.add_user(&name, &phc).map_err(internal)? else {
        return Err(Fail::new(StatusCode::CONFLICT, format!("«{name}» уже есть")));
    };
    log("web", format!("{}: «{}» added the user «{}»", c.ip, a.name.escape_debug(), name.escape_debug()));
    Ok((StatusCode::CREATED, Json(json!({"id": id, "name": name}))).into_response())
}

/// DELETE /api/users/{id}: a user and their sessions (not oneself: no one would be left to sign in, maybe).
pub async fn remove_user(
    State(app): State<App>,
    Extension(c): Extension<Client>,
    Extension(a): Extension<Authed>,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Response, Fail> {
    if id == a.user {
        return Err(Fail::new(StatusCode::BAD_REQUEST, "Себя удалить нельзя"));
    }
    let Some(name) = app.store.remove_user_id(id).map_err(internal)? else {
        return Err(Fail::new(StatusCode::NOT_FOUND, "Нет такого пользователя"));
    };
    log("web", format!("{}: «{}» removed the user «{}»", c.ip, a.name.escape_debug(), name.escape_debug()));
    Ok(Json(json!({"deleted": name})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> (std::path::PathBuf, std::sync::Arc<Store>) {
        let d = std::env::temp_dir().join(format!("server-auth-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let s = Store::open(&d.join("server.db").to_string_lossy()).unwrap();
        (d, s)
    }

    #[test]
    fn passwords() {
        assert!(password_ok("1234567").is_err() && password_ok("12345678").is_ok() && password_ok("пароль12").is_ok());
        assert!(password_ok(&"x".repeat(PASSWORD_MAX + 1)).is_err());
        assert_eq!(name_ok("  Аня "), Ok("Аня".to_string()));
        assert!(name_ok("").is_err() && name_ok("a\nb").is_err() && name_ok(&"я".repeat(NAME_MAX + 1)).is_err());
        let h = hash_password("correct horse").unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{h}");
        assert_eq!(verify("correct horse", &h), (true, false));
        assert_eq!(verify("correct hors", &h), (false, false));
        assert_ne!(hash_password("correct horse").unwrap(), h, "a salt each");
        // a hash with weaker parameters: still checks, and is to be made again
        use argon2::PasswordHasher;
        let weak = argon2::Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(8192, 1, 1, None).unwrap(),
        )
        .hash_password_with_salt(b"correct horse", b"saltsaltsalt")
        .unwrap()
        .to_string();
        assert_eq!(verify("correct horse", &weak), (true, true));
        assert_eq!(verify("wrong", &weak), (false, false), "nothing is hashed anew for a wrong one");
        assert_eq!(verify("x", "not a hash"), (false, false));
        assert!(!dummy().is_empty() && !verify("", dummy()).0);
    }

    #[test]
    fn tokens_and_cookies() {
        let t = token();
        assert!(t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()) && token() != t);
        assert_eq!(token_hash("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let mut h = HeaderMap::new();
        h.append(
            header::COOKIE,
            HeaderValue::from_str(&format!("theme=dark; {COOKIE}x=1; {COOKIE}={}", t.to_uppercase())).unwrap(),
        );
        assert_eq!(token_of(&h), Some(t.clone()), "among others, in any case");
        let mut h2 = HeaderMap::new();
        h2.append(header::COOKIE, HeaderValue::from_static("a=b"));
        h2.append(header::COOKIE, HeaderValue::from_str(&format!("{COOKIE}={t}")).unwrap());
        assert_eq!(token_of(&h2), Some(t.clone()), "in a second Cookie header");
        for bad in [format!("{COOKIE}=short"), format!("{COOKIE}={}z", &t[..63]), format!("x{COOKIE}={t}")] {
            let mut h = HeaderMap::new();
            h.insert(header::COOKIE, HeaderValue::from_str(&bad).unwrap());
            assert_eq!(token_of(&h), None, "{bad}");
        }
        let c = cookie(&t);
        let c = c.to_str().unwrap();
        for part in ["__Host-ranunculus=", "Path=/", "Secure", "HttpOnly", "SameSite=Lax", "Max-Age=2592000"] {
            assert!(c.contains(part), "{c}");
        }
        assert!(!c.contains("Domain"), "__Host-: no Domain");
        assert!(no_cookie().to_str().unwrap().contains("Max-Age=0"));
    }

    #[test]
    fn tries() {
        let a = Auth::default();
        let keys = ["ip 192.168.1.10".to_string(), "name admin".to_string()];
        let other = ["ip 192.168.1.11".to_string(), "name аня".to_string()];
        for i in 0..FREE_TRIES {
            assert_eq!(a.wait_left(&keys, 100.0), 0.0, "try {i}");
            a.failed(&keys, 100.0);
        }
        assert_eq!(a.wait_left(&keys, 100.0), 30.0);
        assert_eq!(a.wait_left(&keys, 130.0), 0.0);
        a.failed(&keys, 130.0);
        assert_eq!(a.wait_left(&keys, 130.0), 60.0, "doubles");
        // the same name from another address waits too (the name is a key), another name from another address does not
        assert_eq!(a.wait_left(&["ip 192.168.1.12".into(), "name admin".into()], 130.0), 60.0);
        assert_eq!(a.wait_left(&other, 130.0), 0.0);
        for _ in 0..20 {
            a.failed(&keys, 200.0);
        }
        assert_eq!(a.wait_left(&keys, 200.0), LONGEST_WAIT, "15 min at most");
        a.passed(&keys);
        assert_eq!(a.wait_left(&keys, 200.0), 0.0, "a success clears it");
        assert_eq!(wait(FREE_TRIES - 1), 0.0);
    }

    #[test]
    fn users_and_sessions() {
        let (d, s) = store("sessions");
        assert!(!s.has_users());
        assert_eq!(s.set_password("Admin", "h1"), Ok(true));
        assert!(s.has_users());
        assert_eq!(
            s.login_of("admin").unwrap().map(|u| (u.1, u.2)),
            Some(("Admin".into(), Some("h1".into()))),
            "names in any case"
        );
        let id = s.login_of("ADMIN").unwrap().unwrap().0;
        assert_eq!(s.set_password("Аня", "h"), Ok(true));
        assert_eq!(s.login_of("АНЯ").unwrap().map(|u| u.1), Some("Аня".into()), "not ASCII only");
        assert_eq!(s.set_password("аня", "h0"), Ok(false), "the same user");
        assert_eq!(s.remove_user("аНя"), Ok(Some("Аня".into())));
        let now = 1_000_000.0;
        s.open_session("s1", id, now, now, "192.168.1.10", "Firefox").unwrap();
        s.open_session("s2", id, now, now, "192.168.1.11", "Safari").unwrap();
        let a = s.session("s1", now + 10.0).unwrap().unwrap();
        assert_eq!((a.user, a.name.as_str(), a.auth_t), (id, "Admin", now));
        assert_eq!(s.session("nope", now).unwrap(), None);
        // used: kept up (not on every request); unused for 7 days: over
        assert!(s.session("s1", now + IDLE - 1.0).unwrap().is_some());
        assert!(s.session("s1", now + 2.0 * IDLE - 2.0).unwrap().is_some(), "its «last used» moved on");
        assert!(s.session("s2", now + IDLE + 1.0).unwrap().is_none(), "unused for a week");
        assert!(s.session("s2", now).unwrap().is_none(), "ended for good");
        // 30 days at most, used or not
        let mut t = now;
        while t < now + LONGEST - IDLE / 2.0 {
            t += IDLE / 2.0;
            assert!(s.session("s1", t).unwrap().is_some());
        }
        assert!(s.session("s1", now + LONGEST + 1.0).unwrap().is_none());
        // a new password ends them all
        s.open_session("s3", id, now, now, "x", "").unwrap();
        s.open_session("s4", id, now, now, "x", "").unwrap();
        assert_eq!(s.users().unwrap()[0].sessions, 2);
        assert_eq!(s.set_password("admin", "h2"), Ok(false));
        assert!(s.session("s3", now).unwrap().is_none() && s.session("s4", now).unwrap().is_none());
        assert_eq!(s.login_of("admin").unwrap().unwrap().2.as_deref(), Some("h2"));
        // ended one by one or all at once; a user removed takes theirs
        s.open_session("s5", id, now, now, "x", "").unwrap();
        s.open_session("s6", id, now, now, "x", "").unwrap();
        s.end_session("s5").unwrap();
        assert!(s.session("s5", now).unwrap().is_none() && s.session("s6", now).unwrap().is_some());
        assert_eq!(s.end_sessions(id), Ok(1));
        s.open_session("s7", id, now, now, "x", "").unwrap();
        assert_eq!(s.remove_user("ADMIN"), Ok(Some("Admin".into())));
        assert!(s.session("s7", now).unwrap().is_none() && !s.has_users());
        assert_eq!(s.remove_user("admin"), Ok(None));
        drop(s);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_first_and_the_others() {
        let (d, s) = store("first");
        let first = s.first_user("Аня", "h1").unwrap().expect("no one yet: made");
        assert_eq!(s.first_user("Боря", "h2"), Ok(None), "the first only while there is no one");
        assert_eq!(s.users().unwrap().iter().map(|u| u.name.as_str()).collect::<Vec<_>>(), vec!["Аня"]);
        let other = s.add_user("Боря", "h2").unwrap().expect("added");
        assert_eq!(s.add_user("БОРЯ", "h3"), Ok(None), "the name is taken in any case");
        assert_eq!(s.password_hash(other), Ok(Some("h2".into())));
        // a new password ends the user's sessions, not others'
        s.open_session("a1", first, 100.0, 100.0, "x", "").unwrap();
        s.open_session("b1", other, 100.0, 100.0, "x", "").unwrap();
        s.new_password(other, "h4").unwrap();
        assert!(s.session("b1", 101.0).unwrap().is_none() && s.session("a1", 101.0).unwrap().is_some());
        assert_eq!(s.password_hash(other), Ok(Some("h4".into())));
        // removed by id, with their sessions
        s.open_session("b2", other, 100.0, 100.0, "x", "").unwrap();
        assert_eq!(s.remove_user_id(other), Ok(Some("Боря".into())));
        assert!(s.session("b2", 101.0).unwrap().is_none());
        assert_eq!(s.remove_user_id(other), Ok(None));
        // a provider's user has no password
        let p = s.provider_user("https://auth.example.org", "sub-1", "Вика").unwrap();
        assert_eq!(s.password_hash(p), Ok(None));
        assert_eq!(capital("пароль короче 8 символов"), "Пароль короче 8 символов");
        drop(s);
        let _ = std::fs::remove_dir_all(&d);
    }
}
