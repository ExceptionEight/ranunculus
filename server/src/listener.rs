//! The stations connect here: TLS (the server's self-signed certificate; the stations pin it), a WebSocket on
//! /station, then the hello: proto and version must be exactly ours (one commit builds both), the token the
//! configured one. Then the session (session.rs) — one per device: a new connection from the same device ends the
//! old one.
use crate::identity::Identity;
use crate::session::{self, log, Shared};
use futures_util::{SinkExt, StreamExt};
use ranunculus_proto::{Down, Up, PATH, PROTO, VERSION};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::Message;

/// TLS by `cert` and `key` (PEM): the stations' link, and the web UI's (web/tls.rs).
pub fn acceptor(cert: &str, key: &str) -> Result<TlsAcceptor, String> {
    use tokio_rustls::rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
    let certs = CertificateDer::pem_file_iter(cert)
        .and_then(|it| it.collect::<Result<Vec<_>, _>>())
        .map_err(|e| format!("{cert}: {e}"))?;
    let key = PrivateKeyDer::from_pem_file(key).map_err(|e| format!("{key}: {e}"))?;
    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let cfg = tokio_rustls::rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("certificate: {e}"))?;
    Ok(TlsAcceptor::from(Arc::new(cfg)))
}

/// `client`: the stations' client of this server's version (a file), for a station of another one.
pub async fn serve(listen: &str, me: Identity, client: String, shared: Arc<Shared>) -> Result<(), String> {
    let tls = acceptor(&me.cert, &me.key)?;
    let listener = TcpListener::bind(listen).await.map_err(|e| format!("{listen}: {e}"))?;
    log(
        "server",
        format!(
            "listening on wss://{listen}{PATH} (version {VERSION}, proto {PROTO}), certificate SHA-256 {}",
            crate::identity::fingerprint(&me.cert)?
        ),
    );
    // each station's link now: told to end when the same station connects again
    let sessions: Arc<Mutex<HashMap<String, Arc<tokio::sync::Notify>>>> = Arc::new(Mutex::new(HashMap::new()));
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                log("server", format!("accept: {e}"));
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
        let _ = tcp.set_nodelay(true);
        let (tls, token, client, shared, sessions) =
            (tls.clone(), me.token.clone(), client.clone(), shared.clone(), sessions.clone());
        tokio::spawn(async move {
            if let Err(e) = handle(tcp, tls, token, client, shared, sessions).await {
                log("server", format!("{peer}: {e}"));
            }
        });
    }
}

async fn handle(
    tcp: tokio::net::TcpStream,
    tls: TlsAcceptor,
    token: String,
    client: String,
    shared: Arc<Shared>,
    sessions: Arc<Mutex<HashMap<String, Arc<tokio::sync::Notify>>>>,
) -> Result<(), String> {
    let s = tokio::time::timeout(Duration::from_secs(5), tls.accept(tcp))
        .await
        .map_err(|_| "TLS: timed out".to_string())?
        .map_err(|e| format!("TLS: {e}"))?;
    // the signature is tungstenite's handshake callback: its error is the HTTP response itself
    #[allow(clippy::result_large_err)]
    let check = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        if req.uri().path() == PATH {
            Ok(resp)
        } else {
            let mut r = ErrorResponse::new(Some("not here".into()));
            *r.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND;
            Err(r)
        }
    };
    let mut ws = tokio_tungstenite::accept_hdr_async(s, check).await.map_err(|e| format!("websocket: {e}"))?;
    let first =
        tokio::time::timeout(Duration::from_secs(5), ws.next()).await.map_err(|_| "no hello in 5 s".to_string())?;
    let Some(Ok(Message::Text(t))) = first else { return Err("expected a hello".into()) };
    let Ok(Up::Hello { proto, version, device, token: tok, caps, .. }) = serde_json::from_str::<Up>(&t) else {
        return Err("expected a hello".into());
    };
    let mut refuse = (tok != token).then(|| "wrong token".to_string());
    if refuse.is_none() && (proto != PROTO || version != VERSION) {
        // a station of another version: it gets this server's client and comes back as one of ours
        match tokio::fs::read(&client).await {
            Ok(file) => {
                let sha256: String = ring::digest::digest(&ring::digest::SHA256, &file)
                    .as_ref()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                log(&device, format!("version {version} (proto {proto}): given the client {VERSION} to update itself"));
                ws.send(Message::Text(
                    Down::Update { version: VERSION.into(), size: file.len() as u64, sha256 }.to_json().into(),
                ))
                .await
                .map_err(|e| format!("update: {e}"))?;
                for chunk in file.chunks(64 << 10) {
                    ws.send(Message::Binary(chunk.to_vec().into())).await.map_err(|e| format!("update: {e}"))?;
                }
                let _ = ws.close(None).await;
                return Ok(());
            }
            Err(_) => {
                refuse = Some(format!("version mismatch: the server is {VERSION} (proto {PROTO}), the station {version} (proto {proto}), and the server has no client to give ({client})"));
            }
        }
    }
    if let Some(reason) = refuse {
        log(&device, format!("refused: {reason}"));
        let _ = ws.send(Message::Text(Down::Refuse { reason }.to_json().into())).await;
        let _ = ws.close(None).await;
        return Ok(());
    }
    let session_id = format!("{}", crate::text::now_s() as u64);
    ws.send(Message::Text(
        Down::Welcome { proto: PROTO, version: VERSION.into(), session: session_id }.to_json().into(),
    ))
    .await
    .map_err(|e| format!("welcome: {e}"))?;
    log(&device, format!("connected (caps {})", caps.join(", ")));
    let kick = Arc::new(tokio::sync::Notify::new());
    if let Some(old) = sessions.lock().unwrap().insert(device.clone(), kick.clone()) {
        old.notify_one(); // the same station again: the old link is dead anyway — it ends and cleans up
    }
    let _ = tokio::spawn(session::run(ws, device.clone(), caps, shared, kick.clone())).await;
    let mut s = sessions.lock().unwrap();
    if s.get(&device).is_some_and(|k| Arc::ptr_eq(k, &kick)) {
        s.remove(&device);
    }
    Ok(())
}
