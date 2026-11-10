//! Playback streams: each one is an `aplay` into ALSA `default` (dmix: the voice, the music and anything else mix
//! there; never the `soft_volume` PCM: opening it replaces the volume knob with one that changes nothing). The server
//! sends `play`, PCM frames, then `end` (play out and report `done`) or `kill` (cut now: barge-in, stop).
//! A music stream (`music_lead`) is analysed on its way to aplay for the panel: the server sends it `music_lead` s
//! ahead of real time, so a hop is heard `music_lead` s after its place in the arrivals' schedule (a sliding minimum
//! over 2 s of arrival − position: Wi-Fi only ever delays arrivals).
use crate::panel::{self, analyzer, states, Panel};
use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

pub enum Event {
    /// A stream is over: its id, and whether it was cut.
    Done(u32, bool),
}

enum Cmd {
    Data(Vec<u8>, f64),
    End,
}

struct Stream {
    tx: mpsc::UnboundedSender<Cmd>,
    music: bool,
    /// Kept until the stream reports done: dropping it would read as a kill on the other end — and a stream
    /// that has had its `end` can still be cut (a barge-in while the last sentence plays out).
    kill: Option<oneshot::Sender<()>>,
    ended: bool,
}

pub struct Player {
    streams: HashMap<u32, Stream>,
    voice_ended: f64, // when the last voice (not music) stream reported done, monotonic s
    events: mpsc::UnboundedSender<Event>,
    panel: Option<Panel>,
    aplay: String,
    /// the look's «smooth» for the envelope of the voice
    pub smooth: f64,
}

impl Player {
    pub fn new(events: mpsc::UnboundedSender<Event>, panel: Option<Panel>) -> Player {
        Player {
            streams: HashMap::new(),
            voice_ended: -1e9,
            events,
            panel,
            aplay: std::env::var("STATION_APLAY").unwrap_or_else(|_| "aplay".into()),
            smooth: 0.6,
        }
    }

    pub fn play(
        &mut self,
        id: u32,
        rate: u32,
        channels: u8,
        music_lead: Option<f64>,
        buffer_ms: Option<u32>,
    ) -> Result<(), String> {
        if self.streams.contains_key(&id) {
            return Err(format!("stream {id} is already playing"));
        }
        if !(8000..=48000).contains(&rate) || !(1..=2).contains(&channels) {
            return Err(format!("stream {id}: {rate} Hz × {channels} is not playable"));
        }
        let mut cmd = Command::new(&self.aplay);
        cmd.args(["-q", "-t", "raw", "-f", "S16_LE", "-r", &rate.to_string(), "-c", &channels.to_string()]);
        if let Some(b) = buffer_ms.or(music_lead.map(|_| 300)) {
            cmd.args(["-B", &(b.clamp(20, 2000) * 1000).to_string()]);
        }
        cmd.arg("-").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
        let child = cmd.spawn().map_err(|e| format!("stream {id}: aplay: {e}"))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let (ktx, krx) = oneshot::channel();
        let tap = match (music_lead, &self.panel) {
            (Some(lead), Some(p)) => Some(Tap::new(rate as f64, channels as usize, lead, p.clone())),
            _ => None,
        };
        // the voice: its loudness for the «speak» animation, timed by where each sample is in the stream
        let voice = match (music_lead, &self.panel) {
            (None, Some(p)) => Some(VoiceTap {
                env: states::Envelope::new(rate),
                rate: rate as f64,
                ch: channels as usize,
                t_first: None,
                hops: 0,
                smooth: self.smooth,
                panel: p.clone(),
            }),
            _ => None,
        };
        tokio::spawn(run(id, child, rx, krx, tap, voice, self.events.clone()));
        self.streams.insert(id, Stream { tx, music: music_lead.is_some(), kill: Some(ktx), ended: false });
        Ok(())
    }

    pub fn data(&mut self, id: u32, pcm: Vec<u8>) {
        if let Some(s) = self.streams.get(&id) {
            if !s.ended {
                let _ = s.tx.send(Cmd::Data(pcm, crate::now_s()));
            }
        }
    }

    /// No more data: the stream plays out on its own and reports done.
    pub fn end(&mut self, id: u32) {
        if let Some(s) = self.streams.get_mut(&id) {
            s.ended = true;
            let _ = s.tx.send(Cmd::End);
        }
    }

    /// The stream has reported done: forget it.
    pub fn finished(&mut self, id: u32) {
        if let Some(s) = self.streams.remove(&id) {
            if !s.music {
                self.voice_ended = crate::now_s();
            }
        }
    }

    /// The voice (any stream that is not music) is playing, or stopped less than `tail` s ago: the residual of its echo
    /// may still be in the microphone.
    pub fn voice_active(&self, tail: f64) -> bool {
        self.streams.values().any(|s| !s.music) || crate::now_s() - self.voice_ended < tail
    }

    pub fn kill(&mut self, id: u32) {
        if let Some(mut s) = self.streams.remove(&id) {
            if let Some(k) = s.kill.take() {
                let _ = k.send(());
            }
            if !s.music {
                self.voice_ended = crate::now_s();
            }
        }
    }

    pub fn kill_all(&mut self) {
        let ids: Vec<u32> = self.streams.keys().cloned().collect();
        for id in ids {
            self.kill(id);
        }
    }
}

async fn run(
    id: u32,
    mut child: tokio::process::Child,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    mut kill: oneshot::Receiver<()>,
    mut tap: Option<Tap>,
    mut voice: Option<VoiceTap>,
    events: mpsc::UnboundedSender<Event>,
) {
    let mut stdin = child.stdin.take();
    // aplay says why it failed on stderr: into the log
    if let Some(mut err) = child.stderr.take() {
        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut b = vec![];
            let _ = err.read_to_end(&mut b).await;
            let t = String::from_utf8_lossy(&b);
            if !t.trim().is_empty() {
                eprintln!("{} play: stream {id}: aplay: {}", crate::stamp(), t.trim().replace('\n', " | "));
            }
        });
    }
    loop {
        tokio::select! {
            Ok(()) = &mut kill => {
                let _ = child.kill().await;
                let _ = events.send(Event::Done(id, true));
                return;
            }
            cmd = rx.recv() => match cmd {
                Some(Cmd::Data(pcm, t_arr)) => {
                    if let Some(t) = tap.as_mut() {
                        t.feed(&pcm, t_arr);
                    }
                    if let Some(v) = voice.as_mut() {
                        v.feed(&pcm, t_arr);
                    }
                    let Some(w) = stdin.as_mut() else { continue };
                    tokio::select! {
                        r = w.write_all(&pcm) => if let Err(e) = r {
                            // aplay is gone (a device error): the stream is over
                            let st = child.try_wait().ok().flatten();
                            eprintln!("{} play: stream {id}: write to aplay: {e} (aplay exit {st:?})", crate::stamp());
                            let _ = child.kill().await;
                            let _ = events.send(Event::Done(id, true));
                            return;
                        },
                        Ok(()) = &mut kill => {
                            let _ = child.kill().await;
                            let _ = events.send(Event::Done(id, true));
                            return;
                        }
                    }
                }
                Some(Cmd::End) | None => {
                    drop(stdin.take()); // EOF: aplay plays out its buffer and exits
                    tokio::select! {
                        st = child.wait() => {
                            if !matches!(&st, Ok(s) if s.success()) {
                                eprintln!("{} play: stream {id}: aplay ended with {st:?}", crate::stamp());
                            }
                            let _ = events.send(Event::Done(id, false));
                        }
                        Ok(()) = &mut kill => { let _ = child.kill().await; let _ = events.send(Event::Done(id, true)); }
                    }
                    return;
                }
            }
        }
    }
}

/// The loudness of the voice for the panel: 20-ms levels, each stamped with when it will be heard — the first sample
/// VOICE_LAG after the stream's first block arrived, then at the stream's own pace.
struct VoiceTap {
    env: states::Envelope,
    rate: f64,
    ch: usize,
    t_first: Option<f64>,
    hops: u64,
    smooth: f64,
    panel: Panel,
}

impl VoiceTap {
    fn feed(&mut self, pcm: &[u8], t_arr: f64) {
        let t_first = *self.t_first.get_or_insert(t_arr);
        let mono: Vec<i16> = pcm.chunks_exact(2 * self.ch).map(|f| i16::from_le_bytes([f[0], f[1]])).collect();
        let mut levels = vec![];
        self.env.push(&mono, self.smooth, |v| levels.push(v));
        if !levels.is_empty() {
            let t0 = t_first + states::VOICE_LAG + self.hops as f64 * (self.rate / 50.0).round() / self.rate;
            self.hops += levels.len() as u64;
            self.panel.send(panel::Msg::Voice { t0, levels });
        }
    }
}

/// The music's analysis for the panel, hop by hop, stamped with when the hop will be heard.
struct Tap {
    sr: f64,
    ch: usize,
    lead: f64,
    an: analyzer::Analyzer,
    pend: Vec<u8>,
    frames: u64,
    win: VecDeque<(f64, f64)>, // (arrival s, arrival − position s)
    mono: Vec<f32>,
    panel: Panel,
}

const HOP: usize = 1024;

impl Tap {
    fn new(sr: f64, ch: usize, lead: f64, panel: Panel) -> Tap {
        Tap {
            sr,
            ch,
            lead,
            an: analyzer::Analyzer::new(sr, HOP, 2048),
            pend: vec![],
            frames: 0,
            win: VecDeque::new(),
            mono: vec![0.0; HOP],
            panel,
        }
    }

    fn feed(&mut self, pcm: &[u8], t_arr: f64) {
        self.pend.extend_from_slice(pcm);
        let bpf = 2 * self.ch;
        let mut off = 0;
        while self.pend.len() - off >= HOP * bpf {
            for j in 0..HOP {
                let p = off + j * bpf;
                let mut s = 0f32;
                for c in 0..self.ch {
                    s += i16::from_le_bytes([self.pend[p + 2 * c], self.pend[p + 2 * c + 1]]) as f32;
                }
                self.mono[j] = s / self.ch as f32;
            }
            off += HOP * bpf;
            self.frames += HOP as u64;
            let pos = self.frames as f64 / self.sr;
            self.win.push_back((t_arr, t_arr - pos));
            while self.win.front().is_some_and(|x| t_arr - x.0 > 2.0) {
                self.win.pop_front();
            }
            let base = self.win.iter().map(|x| x.1).fold(f64::INFINITY, f64::min);
            let heard = base + pos + self.lead;
            let o = self.an.push(&self.mono);
            self.panel.send(panel::Msg::Music {
                heard,
                bass: o.bass,
                mid: o.mid,
                high: o.high,
                onsets_at: heard - analyzer::ONSET_BACK,
                onsets: o.onsets_prev,
                peaks: o.onsets_abs,
            });
        }
        self.pend.drain(..off);
    }
}
