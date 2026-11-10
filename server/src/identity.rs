//! What makes this server itself for its stations, made at the first start and kept in `data` (600): the token they
//! present, and a self-signed certificate they pin by its SHA-256 (the web UI serves HTTPS with it too, unless
//! `[web] cert` names another).
use crate::config::Config;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub const TOKEN: &str = "token";
pub const CERT: &str = "tls/server.crt";
pub const KEY: &str = "tls/server.key";

pub struct Identity {
    pub token: String,
    /// the certificate (PEM) and its key: paths
    pub cert: String,
    pub key: String,
}

/// A folder only the server's user reads.
pub fn private_dir(path: &str) -> Result<(), String> {
    if !std::path::Path::new(path).exists() {
        std::fs::create_dir_all(path).map_err(|e| format!("{path}: {e}"))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|e| format!("{path}: {e}"))?;
    }
    Ok(())
}

fn write_private(path: &str, text: &str) -> Result<(), String> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{path}: {e}"))?;
    f.write_all(text.as_bytes()).map_err(|e| format!("{path}: {e}"))
}

fn hex(bytes: &[u8], between: &str) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(between)
}

/// Read, or made now: `data` itself, the token, the certificate with its key.
pub fn ensure(cfg: &Config) -> Result<Identity, String> {
    private_dir(&cfg.data)?;
    let (token, cert, key) = (cfg.at(TOKEN), cfg.at(CERT), cfg.at(KEY));
    if !std::path::Path::new(&token).exists() {
        let mut secret = [0u8; 24];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut secret)
            .map_err(|_| "no random numbers from the system".to_string())?;
        write_private(&token, &format!("{}\n", hex(&secret, "").to_lowercase()))?;
    }
    let (has_cert, has_key) = (std::path::Path::new(&cert).exists(), std::path::Path::new(&key).exists());
    if has_cert != has_key {
        return Err(format!("{cert} and {key}: one is there without the other"));
    }
    if !has_cert {
        private_dir(&cfg.at("tls"))?;
        let pair = rcgen::KeyPair::generate().map_err(|e| format!("certificate: {e}"))?;
        let mut params = rcgen::CertificateParams::new(vec!["ranunculus".to_string(), "localhost".to_string()])
            .map_err(|e| format!("certificate: {e}"))?;
        params.distinguished_name.push(rcgen::DnType::CommonName, "Ranunculus");
        // the stations pin it: it is not meant to run out under them
        let now = time::OffsetDateTime::now_utc();
        (params.not_before, params.not_after) = (now - time::Duration::days(1), now + time::Duration::days(3650));
        let made = params.self_signed(&pair).map_err(|e| format!("certificate: {e}"))?;
        write_private(&key, &pair.serialize_pem())?;
        write_private(&cert, &made.pem())?;
    }
    let token = std::fs::read_to_string(&token).map_err(|e| format!("{token}: {e}"))?.trim().to_string();
    if token.len() < 16 {
        return Err(format!("{}: the token is too short", cfg.at(TOKEN)));
    }
    Ok(Identity { token, cert, key })
}

/// The certificate's SHA-256 as a station's config has it («AB:CD:…»).
pub fn fingerprint(cert: &str) -> Result<String, String> {
    use tokio_rustls::rustls::pki_types::{pem::PemObject, CertificateDer};
    let der = CertificateDer::from_pem_file(cert).map_err(|e| format!("{cert}: {e}"))?;
    Ok(hex(ring::digest::digest(&ring::digest::SHA256, der.as_ref()).as_ref(), ":"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn made_once_and_private() {
        let d = std::env::temp_dir().join(format!("server-identity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let cfg = Config { data: d.join("data").to_string_lossy().to_string(), ..Config::default() };
        let a = ensure(&cfg).unwrap();
        assert_eq!(a.token.len(), 48);
        let mode = |p: &str| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!((mode(&cfg.data), mode(&cfg.at(TOKEN)), mode(&a.key), mode(&a.cert)), (0o700, 0o600, 0o600, 0o600));
        // a certificate the listener takes, with a fingerprint as station.toml writes it
        assert!(crate::listener::acceptor(&a.cert, &a.key).is_ok());
        let fp = fingerprint(&a.cert).unwrap();
        assert!(fp.len() == 95 && fp.split(':').all(|b| b.len() == 2), "{fp}");
        // the next start: the same server
        let b = ensure(&cfg).unwrap();
        assert_eq!((b.token, fingerprint(&b.cert).unwrap()), (a.token, fp));
        // half a pair is not made whole behind the stations' backs
        std::fs::remove_file(&a.key).unwrap();
        assert!(ensure(&cfg).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
