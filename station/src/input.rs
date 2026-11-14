//! Buttons and the microphones' switch, from the kernel's input devices:
//! - `event0` «gpio-keys»: a key and the switch `SW_MUTE_DEVICE` (the microphones' hardware switch);
//! - `event1` «midi_touch»: the touch buttons (the centre one: hold — call, tap — pause/resume; volume ±: a tap).
//!
//! Key codes are named by `KEYS`; an unknown one goes up as `key<code>` (`ranunculus-station input-dump` shows them
//! live).
//! A press shorter than HOLD is a `tap` (sent on release); a longer one is a `hold` (sent once HOLD has passed while
//! still pressed) and then a `release`.
//! The three touch keys (−, centre, +) are a ring: a finger sliding over it presses them one after another, the next
//! 15–30 ms after the last let go. `Gestures` tells that slide — `swipe right`
//! (− → +) or `swipe left` (+ → −); in the original firmware it switches tracks — from a tap: a tap is only sent once
//! GAP has passed with no other key. So a slide that starts on a volume key never moves the volume. On the ring only
//! the centre has a hold; − and + held long are still one tap, when the finger lets go.
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

pub const DEVICES: [&str; 2] = ["/dev/input/event0", "/dev/input/event1"];
/// The touch ring is the client's alone (EVIOCGRAB): the firmware (`maind --platformd` reads both devices) handles
/// holds and draws its own volume ring over ours even with its button handling off, and the panel flickers between
/// the two. The kernel drops the grab when the client exits. The microphones' button (event0) stays shared: the
/// firmware keeps muting the microphones by it.
pub const GRAB: &str = "/dev/input/event1";
const EV_KEY: u16 = 1;
const EV_SW: u16 = 5;
pub const SW_MUTE_DEVICE: u16 = 0x0e;
pub const HOLD: Duration = Duration::from_millis(600);
/// after a touch key lets go, another one pressed within this is the same slide
pub const GAP: Duration = Duration::from_millis(120);
/// the ring from left to right
const RING: [u16; 3] = [114, 582, 115];

/// Key codes → our names: the centre touch button is
/// KEY_VOICECOMMAND, the volume ones KEY_VOLUMEDOWN/UP; the microphones' button gives KEY_MUTE and flips SW_MUTE_DEVICE.
pub const KEYS: &[(u16, &str)] = &[(582, "activate"), (114, "volume_down"), (115, "volume_up"), (113, "mic_mute")];

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Button { name: String, action: &'static str },
    Mute(bool),
}

pub fn key_name(code: u16) -> String {
    KEYS.iter().find(|k| k.0 == code).map(|k| k.1.to_string()).unwrap_or_else(|| format!("key{code}"))
}

/// struct input_event on aarch64: timeval (2 × i64), type u16, code u16, value i32 — 24 bytes.
pub fn parse(b: &[u8]) -> Option<(u16, u16, i32)> {
    if b.len() < 24 {
        return None;
    }
    Some((
        u16::from_le_bytes([b[16], b[17]]),
        u16::from_le_bytes([b[18], b[19]]),
        i32::from_le_bytes(b[20..24].try_into().unwrap()),
    ))
}

/// The state of the input switches now (EVIOCGSW): is the microphones' switch on?
pub fn mute_now(dev: &str) -> Option<bool> {
    let f = File::open(dev).ok()?;
    let mut bits = [0u8; 8];
    // _IOC(_IOC_READ, 'E', 0x1b, len)
    let req: u32 = (2 << 30) | ((bits.len() as u32) << 16) | (0x45 << 8) | 0x1b;
    if unsafe { libc::ioctl(f.as_raw_fd(), req as _, bits.as_mut_ptr()) } < 0 {
        return None;
    }
    Some(bits[(SW_MUTE_DEVICE / 8) as usize] & (1 << (SW_MUTE_DEVICE % 8)) != 0)
}

/// Turns presses and releases into tap / hold / release. `poll(now)` must be called while a key is down (it sends
/// the hold when its time comes).
#[derive(Default)]
pub struct Presses {
    down: Vec<(u16, Instant, bool)>, // (code, since, hold sent)
}

impl Presses {
    pub fn key(&mut self, code: u16, value: i32, now: Instant) -> Option<Input> {
        match value {
            1 => {
                self.down.retain(|d| d.0 != code);
                self.down.push((code, now, false));
                None
            }
            0 => {
                let i = self.down.iter().position(|d| d.0 == code)?;
                let (_, since, held) = self.down.remove(i);
                let action = if held {
                    "release"
                } else if now - since < HOLD {
                    "tap"
                } else {
                    "hold"
                };
                Some(Input::Button { name: key_name(code), action })
            }
            _ => None, // autorepeat
        }
    }

    pub fn poll(&mut self, now: Instant) -> Vec<Input> {
        let mut out = vec![];
        for d in self.down.iter_mut() {
            if !d.2 && now - d.1 >= HOLD {
                d.2 = true;
                out.push(Input::Button { name: key_name(d.0), action: "hold" });
            }
        }
        out
    }

    pub fn any_down(&self) -> bool {
        self.down.iter().any(|d| !d.2)
    }
}

/// The ring's taps, holds and slides. A slide overlaps its keys (the next pressed before the last let go: − 438 ms →
/// centre 596 → − up 616 → + 650 → centre up 678, recorded) or leaves gaps under GAP between them.
#[derive(Default)]
pub struct Gestures {
    down: Vec<(u16, Instant, bool)>, // the keys down now: since, hold sent
    seq: Vec<u16>,                   // the keys of this gesture so far, in the order pressed
    last_up: Option<Instant>,        // all keys up since then
}

fn ring_pos(code: u16) -> i32 {
    RING.iter().position(|&c| c == code).map_or(0, |p| p as i32)
}

impl Gestures {
    pub fn is_ring(code: u16) -> bool {
        RING.contains(&code)
    }

    pub fn key(&mut self, code: u16, value: i32, now: Instant) -> Vec<Input> {
        match value {
            1 => {
                // the same gesture if another key is still down, or the last one let go less than GAP ago
                let goes_on = !self.down.is_empty() || self.last_up.is_some_and(|t| now - t < GAP);
                if !goes_on {
                    self.seq.clear();
                }
                self.seq.push(code);
                self.down.retain(|d| d.0 != code);
                self.down.push((code, now, false));
                self.last_up = None;
                vec![]
            }
            0 => {
                let Some(i) = self.down.iter().position(|d| d.0 == code) else { return vec![] };
                let (_, _, held) = self.down.remove(i);
                if held {
                    self.seq.clear();
                    self.down.clear();
                    return vec![Input::Button { name: key_name(code), action: "release" }];
                }
                if self.down.is_empty() {
                    self.last_up = Some(now);
                }
                vec![]
            }
            _ => vec![],
        }
    }

    /// Call while something is pending (`busy()`): a hold when its time comes, the end of a gesture after GAP.
    pub fn poll(&mut self, now: Instant) -> Vec<Input> {
        if !self.down.is_empty() {
            // a hold only for the centre alone (a slide that stops on it is still a slide; − and + have none)
            let d = &self.down[0];
            if self.down.len() == 1 && self.seq.len() == 1 && d.0 == RING[1] && !d.2 && now - d.1 >= HOLD {
                self.down[0].2 = true;
                return vec![Input::Button { name: key_name(self.down[0].0), action: "hold" }];
            }
            return vec![];
        }
        let Some(up) = self.last_up else { return vec![] };
        if now - up < GAP {
            return vec![];
        }
        self.last_up = None;
        let seq = std::mem::take(&mut self.seq);
        if seq.is_empty() {
            return vec![];
        }
        let (first, last) = (seq[0], *seq.last().unwrap());
        let distinct = seq.windows(2).any(|w| w[0] != w[1]);
        if distinct && first != last {
            let dir = if ring_pos(last) > ring_pos(first) { "right" } else { "left" };
            return vec![Input::Button { name: "swipe".into(), action: dir }];
        }
        if distinct {
            return vec![]; // there and back to the same key: not meant
        }
        seq.iter().map(|&c| Input::Button { name: key_name(c), action: "tap" }).collect()
    }

    pub fn busy(&self) -> bool {
        self.last_up.is_some() || self.down.iter().any(|d| !d.2)
    }
}

fn grab(f: &File) -> std::io::Result<()> {
    let req: u32 = (1 << 30) | (4 << 16) | (0x45 << 8) | 0x90; // EVIOCGRAB = _IOW('E', 0x90, int)
    if unsafe { libc::ioctl(f.as_raw_fd(), req as _, 1 as libc::c_int) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Blocking, forever (one thread per device): reads its events and calls `got`.
pub fn watch(dev: &'static str, got: impl Fn(Input)) {
    loop {
        match File::open(dev) {
            Ok(f) => {
                if dev == GRAB {
                    match grab(&f) {
                        Ok(()) => eprintln!("{} input: {dev} is ours alone (grabbed)", crate::stamp()),
                        Err(e) => {
                            eprintln!("{} input: {dev}: could not grab it ({e}): Alice still sees it", crate::stamp())
                        }
                    }
                }
                if let Err(e) = read_events(f, &got) {
                    eprintln!("{} input: {dev}: {e}", crate::stamp());
                }
            }
            Err(e) => {
                eprintln!("{} input: {dev}: {e}", crate::stamp());
                std::thread::sleep(Duration::from_secs(30));
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn read_events(mut f: File, got: &impl Fn(Input)) -> std::io::Result<()> {
    let mut presses = Presses::default();
    let mut ring = Gestures::default();
    let mut buf = [0u8; 24 * 32];
    loop {
        // while a key is down or a gesture may go on, wake up in time for its hold / its end
        if presses.any_down() || ring.busy() {
            let mut p = libc::pollfd { fd: f.as_raw_fd(), events: libc::POLLIN, revents: 0 };
            if unsafe { libc::poll(&mut p, 1, 20) } == 0 {
                let now = Instant::now();
                for i in presses.poll(now).into_iter().chain(ring.poll(now)) {
                    got(i);
                }
                continue;
            }
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        let now = Instant::now();
        for e in buf[..n].as_chunks::<24>().0 {
            let Some((typ, code, value)) = parse(e) else { continue };
            match typ {
                EV_KEY if Gestures::is_ring(code) => {
                    for i in ring.poll(now).into_iter().chain(ring.key(code, value, now)) {
                        got(i);
                    }
                }
                EV_KEY => {
                    if let Some(i) = presses.key(code, value, now) {
                        got(i);
                    }
                }
                EV_SW if code == SW_MUTE_DEVICE => got(Input::Mute(value != 0)),
                _ => {}
            }
        }
        for i in presses.poll(now).into_iter().chain(ring.poll(now)) {
            got(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_hold_release() {
        let t0 = Instant::now();
        let mut p = Presses::default();
        assert_eq!(p.key(115, 1, t0), None);
        assert_eq!(
            p.key(115, 0, t0 + Duration::from_millis(200)),
            Some(Input::Button { name: "volume_up".into(), action: "tap" })
        );
        assert_eq!(p.key(28, 1, t0), None);
        assert!(p.poll(t0 + Duration::from_millis(300)).is_empty());
        assert_eq!(
            p.poll(t0 + Duration::from_millis(700)),
            vec![Input::Button { name: "key28".into(), action: "hold" }]
        );
        assert!(p.poll(t0 + Duration::from_millis(900)).is_empty(), "one hold per press");
        assert_eq!(
            p.key(28, 0, t0 + Duration::from_millis(1500)),
            Some(Input::Button { name: "key28".into(), action: "release" })
        );
        assert_eq!(p.key(28, 0, t0 + Duration::from_millis(1600)), None, "a release without a press");
    }

    #[test]
    fn ring_taps_holds_and_slides() {
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);
        let b = |n: &str, a: &'static str| Input::Button { name: n.into(), action: a };
        // a tap of − (as recorded: 258 ms down): nothing while the finger is on it, the `tap` once GAP has passed
        let mut g = Gestures::default();
        assert!(g.key(114, 1, ms(0)).is_empty());
        assert!(g.key(114, 0, ms(258)).is_empty());
        assert!(g.poll(ms(300)).is_empty(), "not yet: another key may follow");
        assert_eq!(g.poll(ms(400)), vec![b("volume_down", "tap")]);
        // the recorded slide − → centre → +: right
        let mut g = Gestures::default();
        for (c, v, t) in [(114, 1, 0), (114, 0, 282), (582, 1, 301), (582, 0, 459), (115, 1, 478), (115, 0, 819)] {
            assert!(g.key(c, v, ms(t)).is_empty(), "nothing on the way: the volume stays");
            assert!(g.poll(ms(t)).is_empty());
        }
        assert_eq!(g.poll(ms(1000)), vec![b("swipe", "right")]);
        // an overlapping slide as a finger makes it: − down, centre down, − up, + down, centre up, + up
        let mut g = Gestures::default();
        for (c, v, t) in [(114, 1, 438), (582, 1, 596), (114, 0, 616), (115, 1, 650), (582, 0, 678), (115, 0, 803)] {
            g.key(c, v, ms(t));
            assert!(g.poll(ms(t)).is_empty(), "nothing before the slide ends");
        }
        assert_eq!(g.poll(ms(1000)), vec![b("swipe", "right")]);
        // and back + → centre → −: left
        let mut g = Gestures::default();
        for (c, v, t) in [(115, 1, 0), (115, 0, 269), (582, 1, 283), (582, 0, 470), (114, 1, 499), (114, 0, 700)] {
            g.key(c, v, ms(t));
        }
        assert_eq!(g.poll(ms(900)), vec![b("swipe", "left")]);
        // holding the centre: hold, then release; no tap
        let mut g = Gestures::default();
        g.key(582, 1, ms(0));
        assert!(g.poll(ms(300)).is_empty());
        assert_eq!(g.poll(ms(700)), vec![b("activate", "hold")]);
        assert_eq!(g.key(582, 0, ms(1500)), vec![b("activate", "release")]);
        assert!(g.poll(ms(2000)).is_empty());
        // + held long: no hold, no repeats, one tap when the finger lets go
        let mut g = Gestures::default();
        g.key(115, 1, ms(0));
        for t in (100..3000).step_by(100) {
            assert!(g.poll(ms(t)).is_empty(), "nothing while held ({t} ms)");
        }
        assert!(g.key(115, 0, ms(3000)).is_empty());
        assert_eq!(g.poll(ms(3200)), vec![b("volume_up", "tap")]);
        // two taps of + far apart are two taps
        let mut g = Gestures::default();
        assert!(g.key(115, 1, ms(0)).is_empty());
        g.key(115, 0, ms(150));
        assert_eq!(g.poll(ms(400)), vec![b("volume_up", "tap")]);
        g.key(115, 1, ms(1000));
        g.key(115, 0, ms(1150));
        assert_eq!(g.poll(ms(1400)), vec![b("volume_up", "tap")]);
    }

    #[test]
    fn event_layout() {
        let mut e = [0u8; 24];
        e[16..18].copy_from_slice(&EV_SW.to_le_bytes());
        e[18..20].copy_from_slice(&SW_MUTE_DEVICE.to_le_bytes());
        e[20..24].copy_from_slice(&1i32.to_le_bytes());
        assert_eq!(parse(&e), Some((EV_SW, SW_MUTE_DEVICE, 1)));
    }
}
