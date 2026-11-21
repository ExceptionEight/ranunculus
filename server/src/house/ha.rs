//! Home Assistant's WebSocket API, as much of it as the house needs: the address a user types made into where to
//! connect, the connection opened and the token shown, a message sent, the next one read. What is said over it — the
//! registries asked for, the states followed, a service called — is the house's (mod.rs).
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// A step of connecting (the address resolved and reached, TLS, the handshake, the token answered): a Home Assistant
/// in the same network takes milliseconds.
const STEP: Duration = Duration::from_secs(8);

pub trait Wire: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Wire for T {}
pub type Ws = WebSocketStream<Box<dyn Wire>>;

/// Why there is no connection, as the settings page says it.
#[derive(Clone, Debug, PartialEq)]
pub enum Fault {
    /// what was typed is not an address
    Address(String),
    /// nothing answers there
    Unreachable(String),
    /// something answers, but not as Home Assistant does
    NotHa(String),
    /// Home Assistant refused the token
    Token,
    /// the connection was there and ended
    Lost(String),
}

impl Fault {
    pub fn text(&self) -> String {
        match self {
            Fault::Address(s) => format!("Не похоже на адрес: {s}"),
            Fault::Unreachable(s) => format!("Home Assistant по этому адресу не отвечает: {s}"),
            Fault::NotHa(s) => format!("По этому адресу отвечает не Home Assistant: {s}"),
            Fault::Token => "Home Assistant не принял токен. Проверьте, что он скопирован целиком и не отозван".into(),
            Fault::Lost(s) => format!("Связь с Home Assistant прервалась: {s}"),
        }
    }
}

/// Where Home Assistant is.
#[derive(Clone, Debug, PartialEq)]
pub struct Address {
    pub tls: bool,
    pub host: String,
    pub port: u16,
}

impl Address {
    /// «192.168.1.10», «ha.local:8123», «https://ha.example.org/»: a scheme is http unless said, a path is dropped.
    pub fn parse(s: &str) -> Result<Address, Fault> {
        let s = s.trim();
        let with = if s.contains("://") { s.to_string() } else { format!("http://{s}") };
        let u = reqwest::Url::parse(&with).map_err(|e| Fault::Address(e.to_string()))?;
        let tls = match u.scheme() {
            "http" | "ws" => false,
            "https" | "wss" => true,
            other => return Err(Fault::Address(format!("схема «{other}» не подходит, нужен http или https"))),
        };
        if !u.username().is_empty() || u.password().is_some() {
            return Err(Fault::Address(
                "в адресе не нужны имя и пароль: Home Assistant узнаёт сервер по токену".into(),
            ));
        }
        let host =
            u.host_str().filter(|h| !h.is_empty()).ok_or_else(|| Fault::Address("нет имени или адреса".into()))?;
        let port = u.port().unwrap_or(if tls { 443 } else { 80 });
        // «[fe80::1]» is how a URL writes it; a socket and a certificate's name take it bare
        Ok(Address { tls, host: host.trim_start_matches('[').trim_end_matches(']').to_string(), port })
    }

    /// As it is kept and shown: «http://192.168.1.10», «https://ha.example.org:8443».
    pub fn url(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        if self.port == if self.tls { 443 } else { 80 } {
            format!("{scheme}://{}", self.in_url())
        } else {
            format!("{scheme}://{}:{}", self.in_url(), self.port)
        }
    }

    fn ws(&self) -> String {
        format!("{}://{}:{}/api/websocket", if self.tls { "wss" } else { "ws" }, self.in_url(), self.port)
    }

    fn in_url(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        }
    }
}

async fn step<T, E: std::fmt::Display>(
    what: &str,
    f: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, Fault> {
    match tokio::time::timeout(STEP, f).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(Fault::Unreachable(format!("{what}: {e}"))),
        Err(_) => Err(Fault::Unreachable(format!("{what}: нет ответа за {} с", STEP.as_secs()))),
    }
}

/// The connection opened and the token accepted. → it and Home Assistant's version.
pub async fn open(a: &Address, token: &str) -> Result<(Ws, String), Fault> {
    let tcp = step("соединение", TcpStream::connect((a.host.as_str(), a.port))).await?;
    let _ = tcp.set_nodelay(true);
    let wire: Box<dyn Wire> = if a.tls {
        use rustls_platform_verifier::ConfigVerifierExt;
        use tokio_rustls::rustls::{pki_types::ServerName, ClientConfig};
        let cfg = ClientConfig::with_platform_verifier().map_err(|e| Fault::Unreachable(format!("TLS: {e}")))?;
        let name = ServerName::try_from(a.host.clone()).map_err(|e| Fault::Address(e.to_string()))?;
        Box::new(step("TLS", tokio_rustls::TlsConnector::from(Arc::new(cfg)).connect(name, tcp)).await?)
    } else {
        Box::new(tcp)
    };
    let (mut ws, _) = match tokio::time::timeout(STEP, tokio_tungstenite::client_async(a.ws(), wire)).await {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => return Err(Fault::NotHa(e.to_string())),
        Err(_) => return Err(Fault::NotHa("нет ответа на приветствие".into())),
    };
    let hello = first(&mut ws).await?;
    if hello["type"] != "auth_required" {
        return Err(Fault::NotHa("неожиданное приветствие".into()));
    }
    send(&mut ws, &json!({"type": "auth", "access_token": token})).await?;
    let answer = first(&mut ws).await?;
    match answer["type"].as_str() {
        Some("auth_ok") => Ok((ws, answer["ha_version"].as_str().unwrap_or("").to_string())),
        Some("auth_invalid") => Err(Fault::Token),
        _ => Err(Fault::NotHa("неожиданный ответ на токен".into())),
    }
}

async fn first(ws: &mut Ws) -> Result<Value, Fault> {
    match tokio::time::timeout(STEP, next(ws)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(Fault::Lost(e))) => Err(Fault::NotHa(e)),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(Fault::NotHa("молчит".into())),
    }
}

pub async fn send(ws: &mut Ws, v: &Value) -> Result<(), Fault> {
    ws.send(Message::Text(v.to_string().into())).await.map_err(|e| Fault::Lost(e.to_string()))
}

/// The next message that is JSON (pings are answered by the library, the rest is passed over).
pub async fn next(ws: &mut Ws) -> Result<Value, Fault> {
    loop {
        match ws.next().await {
            None => return Err(Fault::Lost("закрыто".into())),
            Some(Err(e)) => return Err(Fault::Lost(e.to_string())),
            Some(Ok(Message::Text(t))) => {
                return serde_json::from_str(t.as_str()).map_err(|e| Fault::Lost(format!("не JSON: {e}")))
            }
            Some(Ok(Message::Close(_))) => return Err(Fault::Lost("закрыто".into())),
            Some(Ok(_)) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        let a = |s: &str| Address::parse(s).map(|a| a.url());
        assert_eq!(a("192.168.1.10").as_deref(), Ok("http://192.168.1.10"));
        assert_eq!(a(" http://192.168.1.10/ ").as_deref(), Ok("http://192.168.1.10"));
        assert_eq!(a("ha.local:8123").as_deref(), Ok("http://ha.local:8123"));
        assert_eq!(a("https://ha.example.org/lovelace/0").as_deref(), Ok("https://ha.example.org"));
        assert_eq!(a("https://ha.example.org:8443").as_deref(), Ok("https://ha.example.org:8443"));
        assert!(matches!(a("ftp://ha.local"), Err(Fault::Address(_))));
        assert!(matches!(a("http://me:secret@ha.local"), Err(Fault::Address(_))));
        assert!(matches!(a(""), Err(Fault::Address(_))));
        let six = Address::parse("http://[fe80::1]:8123/").unwrap();
        assert_eq!(
            (six.host.as_str(), six.url().as_str(), six.ws().as_str()),
            ("fe80::1", "http://[fe80::1]:8123", "ws://[fe80::1]:8123/api/websocket")
        );
        let x = Address::parse("https://ha.example.org").unwrap();
        assert_eq!((x.tls, x.port, x.ws().as_str()), (true, 443, "wss://ha.example.org:443/api/websocket"));
    }
}
