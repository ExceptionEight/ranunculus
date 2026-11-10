//! The microphone: `vqe_0` — 16 kHz s16le mono after the station's own echo cancellation and noise suppression —
//! from the firmware's audio service on 127.0.0.1:9850 (inside `maind --platformd`).
//! Its IPC: 4-byte LE length + protobuf. Subscribe `{252: {159: {1: ""}}, 253: 159}`; audio comes in messages
//! with `253 == 158`, channels in `252.1 {2: name, 3: PCM}`.
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

pub const ADDR: &str = "127.0.0.1:9850";
pub const CHANNEL: &[u8] = b"vqe_0";
const MAX_FRAME: usize = 4 << 20;

pub struct Chunk {
    /// monotonic µs when it arrived
    pub t_us: u64,
    pub pcm: Vec<u8>,
}

/// The subscription request, as bytes on the wire (length prefix included).
pub fn subscribe_frame() -> Vec<u8> {
    let inner = enc_bytes(1, b"");
    let mid = enc_bytes(159, &inner);
    let mut msg = enc_bytes(252, &mid);
    msg.extend(enc_varint_field(253, 159));
    let mut out = (msg.len() as u32).to_le_bytes().to_vec();
    out.extend(msg);
    out
}

fn varint(v: u64) -> Vec<u8> {
    let mut v = v;
    let mut out = vec![];
    loop {
        let c = (v & 0x7f) as u8;
        v >>= 7;
        out.push(c | if v != 0 { 0x80 } else { 0 });
        if v == 0 {
            return out;
        }
    }
}

fn enc_bytes(field: u64, value: &[u8]) -> Vec<u8> {
    let mut out = varint(field << 3 | 2);
    out.extend(varint(value.len() as u64));
    out.extend_from_slice(value);
    out
}

fn enc_varint_field(field: u64, value: u64) -> Vec<u8> {
    let mut out = varint(field << 3);
    out.extend(varint(value));
    out
}

/// A protobuf field: number and value (varints as numbers, length-delimited as bytes; fixed ones skipped).
#[derive(Debug, PartialEq)]
pub enum Val<'a> {
    Int(u64),
    Bytes(&'a [u8]),
    Fixed,
}

/// The fields of a protobuf message, or None if it is malformed.
pub fn fields(b: &[u8]) -> Option<Vec<(u64, Val<'_>)>> {
    fn rd(b: &[u8], p: &mut usize) -> Option<u64> {
        let (mut r, mut s) = (0u64, 0u32);
        loop {
            let c = *b.get(*p)?;
            *p += 1;
            if s >= 64 {
                return None;
            }
            r |= ((c & 0x7f) as u64) << s;
            s += 7;
            if c < 0x80 {
                return Some(r);
            }
        }
    }
    let mut out = vec![];
    let mut p = 0;
    while p < b.len() {
        let tag = rd(b, &mut p)?;
        let (f, w) = (tag >> 3, tag & 7);
        match w {
            0 => out.push((f, Val::Int(rd(b, &mut p)?))),
            1 => {
                p += 8;
                out.push((f, Val::Fixed));
            }
            2 => {
                let n = rd(b, &mut p)? as usize;
                let end = p.checked_add(n)?;
                out.push((f, Val::Bytes(b.get(p..end)?)));
                p = end;
            }
            5 => {
                p += 4;
                out.push((f, Val::Fixed));
            }
            _ => return None,
        }
        if p > b.len() {
            return None;
        }
    }
    Some(out)
}

/// The PCM of `channel` in one IPC message, if it is an audio message carrying it.
pub fn audio_of<'a>(msg: &'a [u8], channel: &[u8]) -> Option<&'a [u8]> {
    let top = fields(msg)?;
    if !top.iter().any(|(f, v)| *f == 253 && *v == Val::Int(158)) {
        return None;
    }
    for (f, v) in &top {
        if let (252, Val::Bytes(body)) = (*f, v) {
            for (g, ch) in fields(body)? {
                if let (1, Val::Bytes(ch)) = (g, ch) {
                    let d = fields(ch)?;
                    let name =
                        d.iter().find_map(|(k, v)| if let (2, Val::Bytes(n)) = (*k, v) { Some(*n) } else { None });
                    if name == Some(channel) {
                        return d
                            .iter()
                            .find_map(|(k, v)| if let (3, Val::Bytes(p)) = (*k, v) { Some(*p) } else { None });
                    }
                }
            }
        }
    }
    None
}

/// Forever: connect to yio_audio, subscribe, pass vqe_0 chunks on (dropped when the receiver lags), reconnect on
/// any error. `say` gets one line per state change (connected / lost).
pub async fn run(tx: mpsc::Sender<Chunk>, say: impl Fn(String)) {
    let mut dropped = 0u64;
    let mut was_up = false;
    loop {
        match stream(&tx, &mut dropped, &mut was_up, &say).await {
            Ok(()) => say("mic: yio_audio closed the stream".into()),
            Err(e) => {
                if was_up {
                    say(format!("mic: lost yio_audio: {e}"));
                }
            }
        }
        was_up = false;
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

async fn stream(
    tx: &mpsc::Sender<Chunk>,
    dropped: &mut u64,
    was_up: &mut bool,
    say: &impl Fn(String),
) -> std::io::Result<()> {
    let mut s = TcpStream::connect(ADDR).await?;
    s.set_nodelay(true)?;
    s.write_all(&subscribe_frame()).await?;
    let mut len = [0u8; 4];
    let mut buf = vec![];
    loop {
        s.read_exact(&mut len).await?;
        let n = u32::from_le_bytes(len) as usize;
        if n > MAX_FRAME {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("an IPC frame of {n} bytes")));
        }
        buf.resize(n, 0);
        s.read_exact(&mut buf).await?;
        if let Some(pcm) = audio_of(&buf, CHANNEL) {
            if !*was_up {
                *was_up = true;
                say(format!("mic: vqe_0 is streaming ({} bytes a chunk)", pcm.len()));
            }
            let c = Chunk { t_us: crate::now_us(), pcm: pcm.to_vec() };
            if tx.try_send(c).is_err() {
                *dropped += 1; // no session reading (between connections): old audio is no use anyway
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_matches_the_python_client() {
        // ipc.enc(252, ipc.enc(159, ipc.enc(1, b''))) + ipc.enc(253, 159), length-prefixed
        let want: Vec<u8> = vec![0xE2, 0x0F, 0x05, 0xFA, 0x09, 0x02, 0x0A, 0x00, 0xE8, 0x0F, 0x9F, 0x01];
        let f = subscribe_frame();
        assert_eq!(&f[..4], &(want.len() as u32).to_le_bytes());
        assert_eq!(&f[4..], &want[..]);
    }

    #[test]
    fn finds_vqe_0_among_channels() {
        let ch = |name: &[u8], pcm: &[u8]| {
            let mut c = enc_bytes(2, name);
            c.extend(enc_bytes(3, pcm));
            enc_bytes(1, &c)
        };
        let mut body = ch(b"raw_mic_0", &[9, 9]);
        body.extend(ch(b"vqe_0", &[1, 2, 3, 4]));
        let mut msg = enc_bytes(252, &body);
        msg.extend(enc_varint_field(253, 158));
        assert_eq!(audio_of(&msg, b"vqe_0"), Some(&[1u8, 2, 3, 4][..]));
        assert_eq!(audio_of(&msg, b"vqe_bnr"), None);
        let mut other = enc_bytes(252, &body);
        other.extend(enc_varint_field(253, 157));
        assert_eq!(audio_of(&other, b"vqe_0"), None, "not an audio message");
        assert_eq!(audio_of(&[0xff, 0xff, 0xff], b"vqe_0"), None, "garbage is no audio, not a panic");
    }
}
