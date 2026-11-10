//! The web UI's connections: plain TCP, or TLS (`[web] tls`) with each handshake off the accepting loop (10 s at most:
//! one slow or stuck client holds no one else up). A browser that came by http:// to the HTTPS port is sent on to
//! https:// — it would get an empty answer, «the page does not open».
use crate::session::log;
use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;

const HANDSHAKE: Duration = Duration::from_secs(10);

/// Who connected, and how: what the handlers get as ConnectInfo.
#[derive(Clone, Copy, Debug)]
pub struct Peer {
    pub addr: SocketAddr,
    pub tls: bool,
}

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub struct Incoming {
    rx: mpsc::Receiver<(Box<dyn Io>, Peer)>,
    local: Peer,
}

impl Listener for Incoming {
    type Io = Box<dyn Io>;
    type Addr = Peer;

    async fn accept(&mut self) -> (Self::Io, Peer) {
        match self.rx.recv().await {
            Some(c) => c,
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Peer> {
        Ok(self.local)
    }
}

impl Connected<IncomingStream<'_, Incoming>> for Peer {
    fn connect_info(s: IncomingStream<'_, Incoming>) -> Peer {
        *s.remote_addr()
    }
}

/// The connections `l` accepts, through TLS if `tls`.
pub fn incoming(l: TcpListener, tls: Option<TlsAcceptor>) -> Incoming {
    let local =
        Peer { addr: l.local_addr().unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0))), tls: tls.is_some() };
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        loop {
            let (tcp, addr) = match l.accept().await {
                Ok(c) => c,
                Err(e) => {
                    log("web", format!("accept: {e}"));
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
            };
            let _ = tcp.set_nodelay(true);
            let Some(acc) = tls.clone() else {
                if tx.send((Box::new(tcp) as Box<dyn Io>, Peer { addr, tls: false })).await.is_err() {
                    return;
                }
                continue;
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                let _ = tokio::time::timeout(HANDSHAKE, async move {
                    let mut first = [0u8; 1];
                    match tcp.peek(&mut first).await {
                        Ok(1) if first[0] != 0x16 => to_https(tcp).await, // not a TLS handshake: plain HTTP
                        Ok(1) => {
                            if let Ok(s) = acc.accept(tcp).await {
                                let _ = tx.send((Box::new(s) as Box<dyn Io>, Peer { addr, tls: true })).await;
                            }
                        }
                        _ => {}
                    }
                })
                .await;
            });
        }
    });
    Incoming { rx, local }
}

/// Where a plain-HTTP request to the HTTPS port is sent: the same host and path by https:// (None: not a request).
fn https_of(head: &str) -> Option<String> {
    let mut lines = head.split("\r\n");
    let path = lines.next()?.split(' ').nth(1).filter(|p| p.starts_with('/'))?;
    let host = lines.find_map(|l| {
        l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("host")).map(|(_, v)| v.trim())
    })?;
    let plain = |s: &str, more: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || more.contains(c));
    (plain(host, ".-:[]") && plain(path, "/-._~%!$&'()*+,;=:@?")).then(|| format!("https://{host}{path}"))
}

async fn to_https(mut tcp: TcpStream) {
    let mut buf = vec![0u8; 8192];
    let mut n = 0;
    while n < buf.len() {
        match tcp.read(&mut buf[n..]).await {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
        if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf[..n]);
    let answer = match https_of(&head) {
        Some(to) => format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ),
        None => {
            let body = "Этот адрес работает по HTTPS: откройте его с https://";
            format!("HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
        }
    };
    let _ = tcp.write_all(answer.as_bytes()).await;
    let _ = tcp.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_http_sent_to_https() {
        let head = "GET /devices/station-1/talk?x=1 HTTP/1.1\r\nUser-Agent: x\r\nhost: 192.168.1.65:52702\r\n\r\n";
        assert_eq!(https_of(head).as_deref(), Some("https://192.168.1.65:52702/devices/station-1/talk?x=1"));
        assert_eq!(
            https_of("GET / HTTP/1.1\r\nHost: [fe80::1]:52702\r\n\r\n").as_deref(),
            Some("https://[fe80::1]:52702/")
        );
        for bad in [
            "GET / HTTP/1.1\r\n\r\n",
            "GET http://x/ HTTP/1.1\r\nHost: x\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: evil\"><script>\r\n\r\n",
            "GET /a\"b HTTP/1.1\r\nHost: x\r\n\r\n",
            "\u{16}\u{3}\u{1}",
            "",
        ] {
            assert_eq!(https_of(bad), None, "{bad:?}");
        }
    }
}
