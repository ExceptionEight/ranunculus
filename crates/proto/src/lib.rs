//! The station ↔ server protocol. One WebSocket, opened by the station:
//! - text frames: JSON control messages, `{"t": "<type>", …}` — [`Up`] from the station, [`Down`] from the server;
//! - binary frames: audio, [`Frame`] — the first byte says which.
//!
//! The station and the server are built from one commit: [`PROTO`] and [`VERSION`] must match exactly. A station that
//! differs is sent the server's own client (`update`) and takes its place with it.
use serde::{Deserialize, Serialize};

/// Bumped on any change of the messages or frames.
pub const PROTO: u32 = 1;
/// The workspace version: the station and the server must be the same build.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The server's WebSocket path.
pub const PATH: &str = "/station";

/// Station → server.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Up {
    /// The first message after connecting.
    Hello {
        proto: u32,
        version: String,
        /// This station's name on the server (its station.toml), e.g. "kitchen".
        device: String,
        token: String,
        /// What the station has: `mic`, `play`, `panel`, `buttons`, `volume`, `kws`.
        caps: Vec<String>,
    },
    /// A playback stream is over: played to its end, or killed (then `killed`).
    Done { id: u32, killed: bool },
    /// The volume knob, 0..255: on change, and as the answer to `volume_get`/`volume_set` (`req`).
    Volume { level: u8, req: Option<u32> },
    /// A button: name `activate` | `volume_up` | `volume_down`; action `tap` | `hold` | `release`.
    Button { name: String, action: String },
    /// The microphones' hardware switch.
    Mute { on: bool },
    /// The station heard the wake word (p ≥ the threshold; `barge`: over the answer's voice). Right after it the
    /// microphone streams up — first the last 0.4 s before the word ended (the start of the command may overlap
    /// it) — until `listen_end`. `t_us`: the station's monotonic time of the hit.
    /// `pre`: the samples of the mic frame that follows that were heard before the detection (the word in them).
    Wake {
        p: f32,
        barge: bool,
        t_us: u64,
        #[serde(default)]
        pre: u32,
        /// the centre button held, not the word
        #[serde(default)]
        button: bool,
    },
    /// Something went wrong on the station (an answer to `req`, or on its own).
    Error { req: Option<u32>, what: String },
}

/// Server → station.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Down {
    /// The hello is accepted.
    Welcome {
        proto: u32,
        version: String,
        session: String,
    },
    /// The hello is refused (wrong token; another version with no client to give); the server closes the connection.
    Refuse {
        reason: String,
    },
    /// To a station of another version, instead of a welcome: the server's own client for stations follows as
    /// binary messages, `size` bytes in all, then the connection closes. The station checks the SHA-256, takes the
    /// client's place with it and starts again. The hello, `refuse` and `update` are the same in every version of
    /// the protocol: it is how a station of any version gets the server's.
    Update {
        version: String,
        size: u64,
        sha256: String,
    },
    /// A new playback stream: its PCM (s16le) follows in [`Frame::Pcm`] frames with this id.
    /// `music_lead`: music — the panel draws it; the PCM is sent this many seconds ahead of real time (the station
    /// stamps when each hop will be heard). `buffer_ms`: the ALSA buffer (default 300 for music, 100 for voice).
    Play {
        id: u32,
        rate: u32,
        channels: u8,
        music_lead: Option<f64>,
        buffer_ms: Option<u32>,
    },
    /// No more PCM for this stream: play out what is queued, then `done`.
    End {
        id: u32,
    },
    /// Cut this stream now (barge-in, stop): `done` with `killed`.
    Kill {
        id: u32,
    },
    VolumeSet {
        level: u8,
        req: Option<u32>,
    },
    VolumeGet {
        req: u32,
    },
    /// The wake word's settings: `threshold`, `barge` (the threshold while an answer plays: its echo's residual is in
    /// the microphone), and `self_name` — the answer playing says the wake word itself: no wake while it plays. Sent
    /// after welcome and whenever one changes.
    Kws {
        threshold: f32,
        barge: f32,
        self_name: bool,
    },
    /// The server has the command (or heard nothing): stop streaming the microphone, forget what the spotter heard,
    /// and do not fire again for 0.3 s (the command is still in its memory).
    ListenEnd,
    /// The assistant's state on the panel: `off` | `listen` | `think` | `search` | `speak` | `music` | `static`. The
    /// station draws it (with a 0.6 s crossfade from whatever is shown); `speak` follows the voice as the station
    /// plays it. Also a one-shot of the player over whatever is shown: `next` | `previous` | `pause` | `continue`.
    Panel {
        state: String,
    },
    /// The panel's look: colours `#RRGGBB` in the LEDs' drive values, levels 0..1, `backlight` 0..255 while active,
    /// `night` (the music's stars slower and dimmer).
    PanelLook(Look),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Look {
    pub color: String,
    pub logo_color: String,
    pub logo: f32,
    pub gain: f32,
    pub gamma: f32,
    pub backlight: f32,
    pub react: f32,
    pub smooth: f32,
    pub speed: f32,
    pub music_react: f32,
    pub music_lag: f32,
    pub night: bool,
}

/// «#FF3000» → [255, 48, 0]; None if malformed.
pub fn hex_rgb(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 {
        return None;
    }
    let b = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    Some([b(0)?, b(2)?, b(4)?])
}

/// Binary frames. Little-endian.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// 0x01, station → server: microphone PCM (s16le mono 16 kHz) with a sequence number and the station's monotonic
    /// time of capture (µs).
    Mic { seq: u32, t_us: u64, pcm: Vec<u8> },
    /// 0x02, server → station: PCM of playback stream `id`.
    Pcm { id: u32, pcm: Vec<u8> },
}

pub const MIC: u8 = 0x01;
pub const PCM: u8 = 0x02;

#[derive(Debug, PartialEq)]
pub enum FrameError {
    Empty,
    Short(u8),
    Kind(u8),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Empty => write!(f, "empty binary frame"),
            FrameError::Short(k) => write!(f, "binary frame 0x{k:02x} too short"),
            FrameError::Kind(k) => write!(f, "unknown binary frame 0x{k:02x}"),
        }
    }
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Frame::Mic { seq, t_us, pcm } => {
                let mut b = Vec::with_capacity(13 + pcm.len());
                b.push(MIC);
                b.extend_from_slice(&seq.to_le_bytes());
                b.extend_from_slice(&t_us.to_le_bytes());
                b.extend_from_slice(pcm);
                b
            }
            Frame::Pcm { id, pcm } => {
                let mut b = Vec::with_capacity(5 + pcm.len());
                b.push(PCM);
                b.extend_from_slice(&id.to_le_bytes());
                b.extend_from_slice(pcm);
                b
            }
        }
    }

    pub fn decode(b: &[u8]) -> Result<Frame, FrameError> {
        let (&k, rest) = b.split_first().ok_or(FrameError::Empty)?;
        match k {
            MIC => {
                if rest.len() < 12 {
                    return Err(FrameError::Short(k));
                }
                Ok(Frame::Mic {
                    seq: u32::from_le_bytes(rest[0..4].try_into().unwrap()),
                    t_us: u64::from_le_bytes(rest[4..12].try_into().unwrap()),
                    pcm: rest[12..].to_vec(),
                })
            }
            PCM => {
                if rest.len() < 4 {
                    return Err(FrameError::Short(k));
                }
                Ok(Frame::Pcm { id: u32::from_le_bytes(rest[0..4].try_into().unwrap()), pcm: rest[4..].to_vec() })
            }
            _ => Err(FrameError::Kind(k)),
        }
    }
}

impl Up {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("an Up message always serializes")
    }
}

impl Down {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a Down message always serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shape_of_the_json() {
        let h = Up::Hello {
            proto: 1,
            version: "0.0.1".into(),
            device: "d".into(),
            token: "x".into(),
            caps: vec!["mic".into()],
        };
        assert_eq!(h.to_json(), r#"{"t":"hello","proto":1,"version":"0.0.1","device":"d","token":"x","caps":["mic"]}"#);
        // a hello of another version may say more: it must still be read, to be answered with `update`
        let old = r#"{"t":"hello","proto":4,"version":"0.0.1","device":"d","room":"","token":"x","caps":["mic"],"backlight":15}"#;
        assert!(matches!(serde_json::from_str::<Up>(old).unwrap(), Up::Hello { proto: 4, .. }));
        assert_eq!(Up::Done { id: 3, killed: false }.to_json(), r#"{"t":"done","id":3,"killed":false}"#);
        let p: Down =
            serde_json::from_str(r#"{"t":"play","id":7,"rate":24000,"channels":1,"music_lead":null,"buffer_ms":null}"#)
                .unwrap();
        assert_eq!(p, Down::Play { id: 7, rate: 24000, channels: 1, music_lead: None, buffer_ms: None });
        // optional fields may be left out by the server
        let p: Down = serde_json::from_str(r#"{"t":"play","id":7,"rate":24000,"channels":1}"#).unwrap();
        assert_eq!(p, Down::Play { id: 7, rate: 24000, channels: 1, music_lead: None, buffer_ms: None });
        let v: Down = serde_json::from_str(r#"{"t":"volume_set","level":77}"#).unwrap();
        assert_eq!(v, Down::VolumeSet { level: 77, req: None });
        assert_eq!(Down::ListenEnd.to_json(), r#"{"t":"listen_end"}"#);
        assert_eq!(serde_json::from_str::<Down>(r#"{"t":"listen_end"}"#).unwrap(), Down::ListenEnd);
        assert_eq!(
            Up::Wake { p: 0.5, barge: true, t_us: 7, pre: 32000, button: false }.to_json(),
            r#"{"t":"wake","p":0.5,"barge":true,"t_us":7,"pre":32000,"button":false}"#
        );
        let pl: Down = serde_json::from_str(r##"{"t":"panel_look","color":"#FF3000","logo_color":"#FF3000","logo":0.85,"gain":1,"gamma":1.5,"backlight":70,"react":0.5,"smooth":0.6,"speed":1,"music_react":0.7,"music_lag":0,"night":false}"##).unwrap();
        assert!(matches!(pl, Down::PanelLook(Look { backlight, .. }) if backlight == 70.0));
        assert_eq!(hex_rgb("#FF3000"), Some([255, 48, 0]));
        assert_eq!(hex_rgb("#FF30"), None);
        assert_eq!(
            serde_json::from_str::<Down>(r#"{"t":"panel","state":"listen"}"#).unwrap(),
            Down::Panel { state: "listen".into() }
        );
        let k: Down = serde_json::from_str(r#"{"t":"kws","threshold":0.8,"barge":0.85,"self_name":false}"#).unwrap();
        assert_eq!(k, Down::Kws { threshold: 0.8, barge: 0.85, self_name: false });
    }

    #[test]
    fn frames_round_trip() {
        for f in [
            Frame::Mic { seq: 9, t_us: 123_456_789, pcm: vec![1, 2, 3, 4] },
            Frame::Pcm { id: 42, pcm: vec![0; 640] },
            Frame::Pcm { id: 1, pcm: vec![] },
        ] {
            assert_eq!(Frame::decode(&f.encode()).unwrap(), f);
        }
        assert_eq!(Frame::decode(&[]), Err(FrameError::Empty));
        assert_eq!(Frame::decode(&[MIC, 0, 0]), Err(FrameError::Short(MIC)));
        assert_eq!(Frame::decode(&[0x7f]), Err(FrameError::Kind(0x7f)));
    }

    #[test]
    fn unknown_messages_are_errors_not_panics() {
        assert!(serde_json::from_str::<Down>(r#"{"t":"nope"}"#).is_err());
        assert!(serde_json::from_str::<Up>(r#"not json"#).is_err());
    }
}
