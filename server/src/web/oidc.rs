//! Signing in by an OIDC provider (`[web.oidc]`: Authelia, Authentik, Keycloak, Zitadel…): the
//! authorization code flow with PKCE (S256), `state` and `nonce`. The openidconnect crate does the protocol and checks
//! the ID token: its signature by the provider's keys (only the algorithms it announces, never «none»), the issuer,
//! the audience, the time, the nonce, the access token's hash.
//! - The way back is `[web] public_url` + /api/auth/oidc/callback, never the request's Host (a forged Host would send
//!   the code elsewhere). The start is moved onto that address first: the state's cookie must be that host's.
//! - `state` is this browser's: kept in memory for 10 min (256 at most) and in a cookie (`__Host-ranunculus-oidc`,
//!   SameSite=Lax: it comes back with the provider's redirect); the callback takes it once, and only with that
//!   cookie — no one can sign another's browser in as themselves (login CSRF).
//! - Who: the provider's (issuer, sub), never the e-mail (Grafana's CVE-2023-3128: one provider's e-mail took another's
//!   account); let in if in one of `groups` (the ID token's or the userinfo's: Authelia 4.39 gives them in the
//!   userinfo only; ["*"] — anyone the provider lets in, said so). A user is made at the first sign-in, the name kept
//!   up from `preferred_username`. The provider's tokens are not kept: only who it is.
//! - The provider's TLS is always checked (`ca_file`: a home CA); its metadata and keys are fetched anew after 5 min.
//! - A refusal comes back as /?login=<one of a few words> (the page says what it means); the details go to the log.
use super::{auth, App, Client};
use crate::config;
use crate::session::log;
use crate::store::{sql, Store};
use crate::text::now_s;
use crate::why;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use openidconnect::core::{
    CoreAuthDisplay, CoreAuthPrompt, CoreAuthenticationFlow, CoreErrorResponseType, CoreGenderClaim, CoreJsonWebKey,
    CoreJweContentEncryptionAlgorithm, CoreJwsSigningAlgorithm, CoreProviderMetadata, CoreRevocableToken,
    CoreRevocationErrorResponse, CoreTokenIntrospectionResponse, CoreTokenType,
};
use openidconnect::{
    AccessTokenHash, AdditionalClaims, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EmptyExtraTokenFields,
    EndpointMaybeSet, EndpointNotSet, EndpointSet, IdTokenFields, IssuerUrl, Nonce, OAuth2TokenResponse,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, StandardErrorResponse, StandardTokenResponse,
    TokenResponse, UserInfoClaims,
};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const START: &str = "/api/auth/oidc";
pub const CALLBACK: &str = "/api/auth/oidc/callback";
const STATE_COOKIE: &str = "__Host-ranunculus-oidc";
const PENDING_FOR: f64 = 600.0;
const PENDING_MAX: usize = 256;
const METADATA_FOR: Duration = Duration::from_secs(300);

/// What the ID token and the userinfo may carry besides the standard claims: the groups (an array, or one string).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Extra {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    groups: Option<Value>,
}
impl AdditionalClaims for Extra {}

type Fields = IdTokenFields<
    Extra,
    EmptyExtraTokenFields,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJwsSigningAlgorithm,
>;
type Tokens = StandardTokenResponse<Fields, CoreTokenType>;
type Provider = openidconnect::Client<
    Extra,
    CoreAuthDisplay,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJsonWebKey,
    CoreAuthPrompt,
    StandardErrorResponse<CoreErrorResponseType>,
    Tokens,
    CoreTokenIntrospectionResponse,
    CoreRevocableToken,
    CoreRevocationErrorResponse,
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

/// A sign-in on its way through the provider.
struct Pending {
    nonce: String,
    verifier: String,
    /// where to go after (a path of the app)
    to: String,
    t: f64,
}

pub struct Oidc {
    /// the button's «Войти через …»
    pub name: String,
    issuer: IssuerUrl,
    id: ClientId,
    secret: ClientSecret,
    redirect: RedirectUrl,
    /// `[web] public_url`'s origin and its host[:port]
    origin: String,
    hostport: String,
    groups: Vec<String>,
    scopes: Vec<String>,
    http: Http,
    provider: Mutex<Option<(Instant, Arc<Provider>)>>,
    pending: Mutex<HashMap<String, Pending>>,
}

/// An error with what caused it, for the log.
/// The provider's requests, through the server's own reqwest (openidconnect's built-in client is another reqwest
/// version): the request as it is, the answer's status, headers and body.
struct Http(reqwest::Client);

impl<'c> openidconnect::AsyncHttpClient<'c> for Http {
    type Error = openidconnect::HttpClientError<reqwest::Error>;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<openidconnect::HttpResponse, Self::Error>> + Send + Sync + 'c>,
    >;

    fn call(&'c self, request: openidconnect::HttpRequest) -> Self::Future {
        Box::pin(async move {
            let request = reqwest::Request::try_from(request).map_err(Box::new)?;
            let answer = self.0.execute(request).await.map_err(Box::new)?;
            let mut b = openidconnect::http::Response::builder().status(answer.status()).version(answer.version());
            for (name, value) in answer.headers() {
                b = b.header(name, value);
            }
            b.body(answer.bytes().await.map_err(Box::new)?.to_vec()).map_err(openidconnect::HttpClientError::Http)
        })
    }
}

impl Oidc {
    /// `[web.oidc]` checked; `public`: `[web] public_url`'s (origin, host).
    pub fn new(c: &config::Oidc, public: Option<&(String, String)>) -> Result<Oidc, String> {
        let (origin, _) =
            public.ok_or("[web] public_url is needed: the address the provider sends the user back to")?;
        let hostport = origin.strip_prefix("https://").ok_or("[web] public_url must be https://")?.to_string();
        if c.client_id.is_empty() {
            return Err("no client_id".into());
        }
        if c.groups.is_empty() {
            return Err(
                "groups is empty: name the groups that may sign in (or [\"*\"]: everyone the provider lets in)".into(),
            );
        }
        let secret = std::fs::read_to_string(&c.client_secret_file)
            .map_err(|e| format!("client_secret_file {}: {e}", c.client_secret_file))?;
        let secret = secret.trim().to_string();
        if secret.is_empty() {
            return Err(format!("client_secret_file {}: empty", c.client_secret_file));
        }
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(&c.client_secret_file).is_ok_and(|m| m.permissions().mode() & 0o077 != 0) {
                log("server", format!("   (web: {} can be read by others: chmod 600)", c.client_secret_file));
            }
        }
        let issuer = IssuerUrl::new(c.issuer.clone()).map_err(|e| format!("issuer «{}»: {e}", c.issuer))?;
        let redirect = RedirectUrl::new(format!("{origin}{CALLBACK}")).map_err(|e| e.to_string())?;
        // redirects not followed: a provider's answer cannot send the server anywhere else (SSRF)
        let mut b = crate::http().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(10));
        if !c.ca_file.is_empty() {
            let pem = std::fs::read(&c.ca_file).map_err(|e| format!("ca_file {}: {e}", c.ca_file))?;
            b = b.add_root_certificate(
                reqwest::Certificate::from_pem(&pem).map_err(|e| format!("ca_file {}: {e}", c.ca_file))?,
            );
        }
        let http = b.build().map_err(|e| e.to_string())?;
        let name = if c.name.trim().is_empty() {
            issuer.url().host_str().unwrap_or("провайдер").to_string()
        } else {
            c.name.trim().to_string()
        };
        Ok(Oidc {
            name,
            issuer,
            id: ClientId::new(c.client_id.clone()),
            secret: ClientSecret::new(secret),
            redirect,
            origin: origin.clone(),
            hostport,
            groups: c.groups.clone(),
            scopes: c.scopes.clone(),
            http: Http(http),
            provider: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
        })
    }

    /// Where the sign-in button goes.
    pub fn start(&self) -> String {
        format!("{}{START}", self.origin)
    }

    /// The provider as it says it is (its endpoints, its keys): fetched anew after 5 min.
    async fn provider(&self) -> Result<Arc<Provider>, String> {
        if let Some((t, p)) = self.provider.lock().unwrap().as_ref() {
            if t.elapsed() < METADATA_FOR {
                return Ok(p.clone());
            }
        }
        let meta = CoreProviderMetadata::discover_async(self.issuer.clone(), &self.http).await.map_err(|e| why(&e))?;
        let p = Arc::new(
            Provider::from_provider_metadata(meta, self.id.clone(), Some(self.secret.clone()))
                .set_redirect_uri(self.redirect.clone()),
        );
        *self.provider.lock().unwrap() = Some((Instant::now(), p.clone()));
        Ok(p)
    }

    fn remember(&self, state: String, p: Pending) {
        let mut m = self.pending.lock().unwrap();
        let now = p.t;
        m.retain(|_, x| now - x.t < PENDING_FOR);
        while m.len() >= PENDING_MAX {
            let Some(oldest) = m.iter().min_by(|a, b| a.1.t.total_cmp(&b.1.t)).map(|(k, _)| k.clone()) else { break };
            m.remove(&oldest);
        }
        m.insert(state, p);
    }

    /// The sign-in of this state, once, if not over its time.
    fn take(&self, state: &str, now: f64) -> Option<Pending> {
        let p = self.pending.lock().unwrap().remove(state)?;
        (now - p.t < PENDING_FOR).then_some(p)
    }

    fn allowed(&self, have: &[String]) -> bool {
        self.groups.iter().any(|g| g == "*" || have.contains(g))
    }
}

/// The groups as a provider gives them: an array of names, or one.
fn groups_of(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => vec![],
    }
}

/// A path of the app to come back to (anything else — the start page): no other site, no API.
fn local_path(to: &str) -> &str {
    let ok = to.starts_with('/')
        && !to.starts_with("//")
        && !to.starts_with("/api/")
        && to != "/api"
        && to.len() <= 512
        && to.bytes().all(|b| b.is_ascii_graphic() && b != b'\\');
    if ok {
        to
    } else {
        "/"
    }
}

fn go(to: &str) -> Response {
    let mut r = StatusCode::SEE_OTHER.into_response();
    r.headers_mut().insert(header::LOCATION, HeaderValue::from_str(to).unwrap_or(HeaderValue::from_static("/")));
    r
}

fn state_cookie(state: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{STATE_COOKIE}={state}; Path=/; Max-Age={}; Secure; HttpOnly; SameSite=Lax",
        PENDING_FOR as u64
    ))
    .unwrap_or(HeaderValue::from_static("__Host-ranunculus-oidc=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Lax"))
}

fn state_of(h: &HeaderMap) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix(STATE_COOKIE)?.strip_prefix('=').map(String::from))
        .filter(|s| !s.is_empty())
}

/// Back to the app with a word for the page (`login`); the state's cookie gone.
fn refused(word: &str) -> Response {
    let mut r = go(&format!("/?login={word}"));
    r.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static("__Host-ranunculus-oidc=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Lax"),
    );
    r
}

/// GET /api/auth/oidc?to=/path: off to the provider.
pub async fn start(
    State(app): State<App>,
    Extension(c): Extension<Client>,
    h: HeaderMap,
    uri: Uri,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(o) = app.oidc.clone() else { return refused("off") };
    // onto the address the provider comes back to: the state's cookie must be that host's
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    if !host.eq_ignore_ascii_case(&o.hostport) {
        return go(&format!("{}{}", o.origin, uri.path_and_query().map(|p| p.as_str()).unwrap_or(START)));
    }
    if !c.secure {
        return refused("insecure");
    }
    let p = match o.provider().await {
        Ok(p) => p,
        Err(e) => {
            log("web", format!("{}: sign-in by {}: the provider does not answer: {e}", c.ip, o.name));
            return refused("provider");
        }
    };
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let mut req = p
        .authorize_url(CoreAuthenticationFlow::AuthorizationCode, CsrfToken::new_random, Nonce::new_random)
        .set_pkce_challenge(challenge);
    for s in &o.scopes {
        req = req.add_scope(Scope::new(s.clone()));
    }
    let (url, state, nonce) = req.url();
    let to = local_path(q.get("to").map(String::as_str).unwrap_or("/")).to_string();
    o.remember(
        state.secret().clone(),
        Pending { nonce: nonce.secret().clone(), verifier: verifier.secret().clone(), to, t: now_s() },
    );
    let mut r = go(url.as_str());
    r.headers_mut().append(header::SET_COOKIE, state_cookie(state.secret()));
    r
}

/// Who the provider says it is: (issuer, subject, name, when they proved it).
struct Who {
    issuer: String,
    subject: String,
    name: String,
    auth_t: Option<f64>,
    groups: Vec<String>,
}

/// The code for tokens, the ID token checked, the userinfo asked (the same subject): who it is.
async fn identify(o: &Oidc, code: String, p: &Pending) -> Result<Who, String> {
    let provider = o.provider().await?;
    let tokens = provider
        .exchange_code(AuthorizationCode::new(code))
        .map_err(|e| why(&e))?
        .set_pkce_verifier(PkceCodeVerifier::new(p.verifier.clone()))
        .request_async(&o.http)
        .await
        .map_err(|e| format!("the code was not exchanged: {}", why(&e)))?;
    let id_token = tokens.id_token().ok_or("the provider gave no ID token")?;
    let verifier = provider.id_token_verifier();
    let claims = id_token
        .claims(&verifier, &Nonce::new(p.nonce.clone()))
        .map_err(|e| format!("the ID token did not pass the check: {}", why(&e)))?;
    // the access token is the one issued with it (not another's put in its place)
    if let Some(want) = claims.access_token_hash() {
        let alg = id_token.signing_alg().map_err(|e| why(&e))?;
        let key = id_token.signing_key(&verifier).map_err(|e| why(&e))?;
        let got = AccessTokenHash::from_token(tokens.access_token(), alg, key).map_err(|e| why(&e))?;
        if got != *want {
            return Err("at_hash does not match the access token".into());
        }
    }
    let mut who = Who {
        issuer: claims.issuer().to_string(),
        subject: claims.subject().to_string(),
        name: claims
            .preferred_username()
            .map(|u| u.to_string())
            .or_else(|| claims.name().and_then(|n| n.get(None)).map(|n| n.to_string()))
            .unwrap_or_default(),
        auth_t: claims.auth_time().map(|t| t.timestamp() as f64),
        groups: groups_of(claims.additional_claims().groups.as_ref()),
    };
    // the userinfo: where Authelia 4.39 keeps the groups and the names; its subject must be the token's
    if let Ok(req) = provider.user_info(tokens.access_token().clone(), Some(claims.subject().clone())) {
        match req.request_async::<Extra, _, CoreGenderClaim>(&o.http).await {
            Ok(info) => {
                let info: UserInfoClaims<Extra, CoreGenderClaim> = info;
                who.groups.extend(groups_of(info.additional_claims().groups.as_ref()));
                if let Some(n) = info
                    .preferred_username()
                    .map(|u| u.to_string())
                    .or_else(|| info.name().and_then(|n| n.get(None)).map(|n| n.to_string()))
                {
                    who.name = n;
                }
            }
            Err(e) => log("web", format!("sign-in by {}: userinfo: {}", o.name, why(&e))),
        }
    }
    if who.name.trim().is_empty() {
        who.name = who.subject.clone();
    }
    who.name = who.name.trim().chars().filter(|c| !c.is_control()).take(auth::NAME_MAX).collect();
    Ok(who)
}

/// GET /api/auth/oidc/callback?code&state (or ?error&state): back from the provider.
pub async fn callback(
    State(app): State<App>,
    Extension(c): Extension<Client>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(o) = app.oidc.clone() else { return refused("off") };
    let now = now_s();
    // this browser's sign-in, once, in time
    let state = q.get("state").cloned().unwrap_or_default();
    let mine = state_of(&h).is_some_and(|s| !state.is_empty() && s == state);
    let Some(p) = mine.then(|| o.take(&state, now)).flatten() else {
        log("web", format!("{}: sign-in by {}: a state that is not ours, or too old", c.ip, o.name));
        return refused("expired");
    };
    if let Some(e) = q.get("error") {
        let d: String = q.get("error_description").map(|d| d.chars().take(200).collect()).unwrap_or_default();
        log(
            "web",
            format!(
                "{}: sign-in by {}: the provider refused: {} {d}",
                c.ip,
                o.name,
                e.chars().take(64).collect::<String>()
            ),
        );
        return refused(if e == "access_denied" { "denied" } else { "failed" });
    }
    let Some(code) = q.get("code").cloned() else { return refused("failed") };
    let who = match identify(&o, code, &p).await {
        Ok(w) => w,
        Err(e) => {
            log("web", format!("{}: sign-in by {}: {e}", c.ip, o.name));
            return refused("failed");
        }
    };
    if !o.allowed(&who.groups) {
        log(
            "web",
            format!(
                "{}: sign-in by {}: «{}» ({}) is in none of the groups {:?} (theirs: {:?})",
                c.ip, o.name, who.name, who.subject, o.groups, who.groups
            ),
        );
        return refused("group");
    }
    let id = match app.store.provider_user(&who.issuer, &who.subject, &who.name) {
        Ok(id) => id,
        Err(e) => {
            log("web", format!("sign-in by {}: {e}", o.name));
            return refused("failed");
        }
    };
    if let Some(old) = auth::token_of(&h) {
        let _ = app.store.end_session(&auth::token_hash(&old));
    }
    let t = auth::token();
    let agent: String =
        h.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").chars().take(200).collect();
    let auth_t = who.auth_t.filter(|a| *a <= now + 60.0).unwrap_or(now);
    if let Err(e) = app.store.open_session(&auth::token_hash(&t), id, now, auth_t, &c.ip.to_string(), &agent) {
        log("web", format!("sign-in by {}: {e}", o.name));
        return refused("failed");
    }
    log("web", format!("{}: «{}» signed in by {}", c.ip, who.name, o.name));
    let mut r = go(&p.to);
    r.headers_mut().append(header::SET_COOKIE, auth::cookie(&t));
    r.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static("__Host-ranunculus-oidc=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Lax"),
    );
    r
}

impl Store {
    /// The provider's user: made at the first sign-in, the name kept up. → its id
    pub fn provider_user(&self, issuer: &str, subject: &str, name: &str) -> Result<i64, String> {
        let db = self.db.lock().unwrap();
        let now = now_s();
        let id: Option<i64> = db
            .query_row("SELECT id FROM users WHERE issuer = ?1 AND subject = ?2", [issuer, subject], |r| r.get(0))
            .optional()
            .map_err(sql)?;
        match id {
            Some(id) => {
                db.execute("UPDATE users SET name = ?1 WHERE id = ?2", params![name, id]).map_err(sql)?;
                Ok(id)
            }
            None => {
                db.execute("INSERT INTO users (name, key, password, issuer, subject, created, changed) VALUES (?1, NULL, NULL, ?2, ?3, ?4, ?4)",
                           params![name, issuer, subject, now])
                    .map_err(sql)?;
                Ok(db.last_insert_rowid())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oidc(groups: &[&str]) -> Oidc {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let d = std::env::temp_dir().join(format!("server-oidc-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("secret");
        std::fs::write(&f, "s3cret\n").unwrap();
        let c = config::Oidc {
            issuer: "https://auth.example.org".into(),
            client_id: "ranunculus".into(),
            client_secret_file: f.to_string_lossy().into(),
            groups: groups.iter().map(|g| g.to_string()).collect(),
            ..Default::default()
        };
        let o = Oidc::new(&c, Some(&("https://home.example.org".into(), "home.example.org".into()))).unwrap();
        let _ = std::fs::remove_dir_all(&d);
        o
    }

    #[test]
    fn what_it_takes() {
        let o = oidc(&["ranunculus"]);
        assert_eq!(
            (o.name.as_str(), o.start().as_str()),
            ("auth.example.org", "https://home.example.org/api/auth/oidc")
        );
        assert_eq!(
            o.redirect.as_str(),
            "https://home.example.org/api/auth/oidc/callback",
            "public_url's, never the Host's"
        );
        let public = ("https://home.example.org".to_string(), "home.example.org".to_string());
        let base = config::Oidc {
            issuer: "https://auth.example.org".into(),
            client_id: "x".into(),
            client_secret_file: "/nonexistent".into(),
            groups: vec!["g".into()],
            ..Default::default()
        };
        assert!(Oidc::new(&base, None).err().unwrap().contains("public_url"));
        assert!(Oidc::new(&base, Some(&("http://home.example.org".into(), "home.example.org".into())))
            .err()
            .unwrap()
            .contains("https"));
        assert!(Oidc::new(&config::Oidc { groups: vec![], ..base.clone() }, Some(&public))
            .err()
            .unwrap()
            .contains("groups"));
        assert!(Oidc::new(&config::Oidc { client_id: String::new(), ..base.clone() }, Some(&public))
            .err()
            .unwrap()
            .contains("client_id"));
        assert!(Oidc::new(&base, Some(&public)).err().unwrap().contains("client_secret_file"));
    }

    #[test]
    fn who_may() {
        let o = oidc(&["ranunculus", "family"]);
        assert!(o.allowed(&["family".into()]) && !o.allowed(&["admins".into()]) && !o.allowed(&[]));
        assert!(oidc(&["*"]).allowed(&[]));
        assert_eq!(groups_of(Some(&serde_json::json!(["a", "b", 3]))), vec!["a", "b"]);
        assert_eq!(groups_of(Some(&serde_json::json!("a"))), vec!["a"]);
        assert!(groups_of(None).is_empty() && groups_of(Some(&serde_json::json!({"a": 1}))).is_empty());
    }

    #[test]
    fn states() {
        let o = oidc(&["*"]);
        let p = |t: f64| Pending { nonce: "n".into(), verifier: "v".into(), to: "/".into(), t };
        o.remember("a".into(), p(1000.0));
        assert!(o.take("b", 1001.0).is_none());
        assert!(o.take("a", 1001.0).is_some());
        assert!(o.take("a", 1001.0).is_none(), "once");
        o.remember("old".into(), p(1000.0));
        assert!(o.take("old", 1000.0 + PENDING_FOR + 1.0).is_none(), "over its time");
        for i in 0..PENDING_MAX + 10 {
            o.remember(format!("s{i}"), p(2000.0 + i as f64));
        }
        let m = o.pending.lock().unwrap();
        assert_eq!(m.len(), PENDING_MAX);
        assert!(!m.contains_key("s0") && m.contains_key(&format!("s{}", PENDING_MAX + 9)), "the oldest go first");
        drop(m);
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("x=1; __Host-ranunculus-oidc=abc; __Host-ranunculus=def"));
        assert_eq!(state_of(&h).as_deref(), Some("abc"));
        let c = state_cookie("abc");
        assert!(
            c.to_str().unwrap().contains("SameSite=Lax")
                && c.to_str().unwrap().contains("HttpOnly")
                && c.to_str().unwrap().contains("Secure")
        );
    }

    #[test]
    fn coming_back() {
        assert_eq!(local_path("/devices/station-1/talk"), "/devices/station-1/talk");
        assert_eq!(local_path("/settings/account?x=1"), "/settings/account?x=1");
        for bad in [
            "//evil.com",
            "https://evil.com",
            "/\\evil.com",
            "/api/devices",
            "/api",
            "",
            "devices",
            "/a b",
            "/a\nb",
            &format!("/{}", "a".repeat(600)),
        ] {
            assert_eq!(local_path(bad), "/", "{bad:?}");
        }
        let r = refused("group");
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        assert_eq!(r.headers()[header::LOCATION], "/?login=group");
        assert!(r.headers()[header::SET_COOKIE].to_str().unwrap().contains("Max-Age=0"));
    }
}
