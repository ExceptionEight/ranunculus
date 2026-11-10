//! /data/ranunculus/station.toml — written at setup (station/install.sh), 600:
//! ```toml
//! server = "wss://192.168.1.10:52700/station"
//! token = "…"                   # the server's token for its stations
//! server_cert_sha256 = "ab12…"  # the server's certificate, pinned (it is the server's own); empty: the usual CA check
//! device = "station-1"          # this station's name on the server: its chats, alarms and settings go by it
//! ```
use serde::Deserialize;

pub const PATH: &str = "/data/ranunculus/station.toml";

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: String,
    pub token: String,
    #[serde(default)]
    pub server_cert_sha256: String,
    #[serde(default = "default_device")]
    pub device: String,
    /// The panel's backlight when nothing is shown (the firmware's own idle level). Not read at start: a station
    /// stopped in the middle of a show leaves it bright, and the next start would take that for the idle level.
    #[serde(default = "default_idle_bl")]
    pub idle_backlight: i32,
}

fn default_idle_bl() -> i32 {
    15
}

fn default_device() -> String {
    "station-1".into()
}

impl Config {
    pub fn load(path: &str) -> Result<Config, String> {
        let s = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        Config::parse(&s).map_err(|e| format!("{path}: {e}"))
    }

    pub fn parse(s: &str) -> Result<Config, String> {
        let c: Config = toml::from_str(s).map_err(|e| e.to_string())?;
        if !(c.server.starts_with("wss://") || c.server.starts_with("ws://")) {
            return Err(format!("server must be a wss:// (or ws:// for tests) URL, not «{}»", c.server));
        }
        if c.token.len() < 16 {
            return Err("token is too short (16+ characters)".into());
        }
        if !c.server_cert_sha256.is_empty() {
            pin_bytes(&c.server_cert_sha256)?;
        }
        Ok(c)
    }

    pub fn pin(&self) -> Option<Vec<u8>> {
        if self.server_cert_sha256.is_empty() {
            None
        } else {
            pin_bytes(&self.server_cert_sha256).ok()
        }
    }
}

/// «AB:12:…» or «ab12…» → 32 bytes.
pub fn pin_bytes(s: &str) -> Result<Vec<u8>, String> {
    let h: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if h.len() != 64 || s.chars().any(|c| !(c.is_ascii_hexdigit() || c == ':')) {
        return Err(format!("server_cert_sha256 must be 64 hex digits (colons allowed), not «{s}»"));
    }
    Ok((0..32).map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_checks() {
        let c = Config::parse("server = \"wss://h:1/station\"\ntoken = \"0123456789abcdef0\"\nserver_cert_sha256 = \"AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89\"\n").unwrap();
        assert_eq!(c.device, "station-1");
        assert_eq!(c.pin().unwrap()[0], 0xAB);
        assert!(Config::parse("server = \"http://h\"\ntoken = \"0123456789abcdef0\"").is_err());
        assert!(Config::parse("server = \"wss://h\"\ntoken = \"short\"").is_err());
        assert!(
            Config::parse("server = \"wss://h\"\ntoken = \"0123456789abcdef0\"\nserver_cert_sha256 = \"zz\"").is_err()
        );
        assert!(
            Config::parse("server = \"wss://h\"\ntoken = \"0123456789abcdef0\"\ntypo = 1").is_err(),
            "unknown keys are errors"
        );
    }
}
