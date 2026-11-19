//! Playback streams: each one goes to an output (here `aplay` into ALSA `default`: the voice, the music and anything
//! else mix there; never `soft_volume` — opening it replaces the volume knob with one that changes nothing). The
//! server sends `play`, PCM frames, then `end` (play out and report `done`) or `kill` (cut now: barge-in, stop).
//!
//! Reliability rules sit above `Out`:
//! - Stop softly (`SIGTERM` first, `SIGKILL` only past `STOP_WAIT`) so the output closes its audio device cleanly;
//!   never wait on a killed output past `KILL_WAIT` (one stuck in the kernel would hold every next stream).
//! - Open no new stream while another is stopping (nor while one ahead is opening): no close/open race in a mixer.
//! - Watch the stream: if a write or draining past the stream's own duration takes over `WATCH_PAD`, stop every
//!   stream and report `done (killed)` so the mixer resets instead of hanging forever.
//!
//! A music stream (`music_lead`) is analysed on its way to the output for the panel: the server sends it `music_lead` s
//! ahead of real time, so a hop is heard `music_lead` s after its place in the arrivals' schedule (a sliding minimum
//! over 2 s of arrival − position: Wi-Fi only ever delays arrivals).
use crate::panel::{self, analyzer, states, Panel};
use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

const STOP_WAIT: Duration = Duration::from_millis(300); // SIGTERM first; SIGKILL only if the player does not let go
const KILL_WAIT: Duration = Duration::from_secs(1); // past SIGKILL: a player stuck in the kernel is left behind
const WATCH_PAD: f64 = 2.0; // past the stream's own length after `end` (or a write blocked this long): the device is stuck

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
    /// Sends a token that `run` drops once the output has stopped.
    kill: Option<oneshot::Sender<oneshot::Sender<()>>>,
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
    /// resolved once streams stopping (or opening) ahead of the next one have finished
    closing: Option<oneshot::Receiver<()>>,
    /// set when the watchdog stops a hung stream so `finished` clears all remaining streams
    stuck: Arc<AtomicBool>,
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
            closing: None,
            stuck: Arc::new(AtomicBool::new(false)),
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
        let wait = self.closing.take();
        let (open_tx, open_rx) = oneshot::channel();
        self.closing = Some(open_rx);
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
        tokio::spawn(run(Job {
            id,
            aplay: self.aplay.clone(),
            rate,
            channels,
            buffer_ms: buffer_ms.or(music_lead.map(|_| 300)),
            wait,
            open_tx,
            rx,
            kill: krx,
            tap,
            voice,
            events: self.events.clone(),
            stuck: self.stuck.clone(),
        }));
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

    /// The stream has reported done: forget it. If it was cut by the watchdog, clear every remaining stream so the
    /// mixer starts fresh.
    pub fn finished(&mut self, id: u32) {
        if let Some(s) = self.streams.remove(&id) {
            if !s.music {
                self.voice_ended = crate::now_s();
            }
        }
        if self.stuck.swap(false, Ordering::SeqCst) {
            self.kill_all();
        }
    }

    /// The voice (any stream that is not music) is playing, or stopped less than `tail` s ago: the residual of its echo
    /// may still be in the microphone.
    pub fn voice_active(&self, tail: f64) -> bool {
        self.streams.values().any(|s| !s.music) || crate::now_s() - self.voice_ended < tail
    }

    fn after_closing(&mut self, rx: oneshot::Receiver<()>) {
        if let Some(mut prev) = self.closing.take() {
            if matches!(prev.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
                let (tx, merged) = oneshot::channel();
                tokio::spawn(async move {
                    let _ = prev.await;
                    let _ = rx.await;
                    drop(tx);
                });
                self.closing = Some(merged);
                return;
            }
        }
        self.closing = Some(rx);
    }

    pub fn kill(&mut self, id: u32) {
        if let Some(mut s) = self.streams.remove(&id) {
            if let Some(k) = s.kill.take() {
                let (tx, rx) = oneshot::channel();
                if k.send(tx).is_ok() {
                    self.after_closing(rx);
                }
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

/// One audio output stream (`aplay` on Linux ALSA; another backend behind the same interface).
struct Out {
    id: u32,
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    /// Set when we stop the output ourselves: what it says then (`write error: Interrupted system call`) is no error.
    stopped: Arc<AtomicBool>,
}

impl Out {
    fn open(aplay: &str, id: u32, rate: u32, channels: u8, buffer_ms: Option<u32>) -> Result<Out, String> {
        let mut cmd = Command::new(aplay);
        cmd.args(["-q", "-t", "raw", "-f", "S16_LE", "-r", &rate.to_string(), "-c", &channels.to_string()]);
        if let Some(b) = buffer_ms {
            cmd.args(["-B", &(b.clamp(20, 2000) * 1000).to_string()]);
        }
        cmd.arg("-").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("stream {id}: aplay: {e}"))?;
        let stdin = child.stdin.take();
        let stopped = Arc::new(AtomicBool::new(false));
        // aplay says why it failed on stderr: into the log, unless we stopped it ourselves
        if let Some(mut err) = child.stderr.take() {
            let stopped = stopped.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut b = vec![];
                let _ = err.read_to_end(&mut b).await;
                let t = String::from_utf8_lossy(&b);
                if !t.trim().is_empty() && !stopped.load(Ordering::SeqCst) {
                    eprintln!("{} play: stream {id}: aplay: {}", crate::stamp(), t.trim().replace('\n', " | "));
                }
            });
        }
        Ok(Out { id, child, stdin, stopped })
    }

    async fn write(&mut self, pcm: &[u8]) -> std::io::Result<()> {
        if let Some(w) = self.stdin.as_mut() {
            w.write_all(pcm).await?;
        }
        Ok(())
    }

    async fn finish(&mut self) -> std::io::Result<std::process::ExitStatus> {
        drop(self.stdin.take()); // EOF: aplay plays out its buffer and exits
        self.child.wait().await
    }

    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    async fn stop(mut self) {
        if self.exited().is_none() {
            if let Some(pid) = self.child.id() {
                self.stopped.store(true, Ordering::SeqCst);
                // SAFETY: a signal to our own child process
                unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            }
        }
        drop(self.stdin.take());
        if self.exited().is_none() && tokio::time::timeout(STOP_WAIT, self.child.wait()).await.is_err() {
            let _ = self.child.start_kill();
            if tokio::time::timeout(KILL_WAIT, self.child.wait()).await.is_err() {
                // stuck in the driver: leave it (tokio reaps it once it dies) rather than hold every next stream
                eprintln!("{} play: stream {}: aplay ignores SIGKILL — left behind", crate::stamp(), self.id);
            }
        }
    }
}

struct Job {
    id: u32,
    aplay: String,
    rate: u32,
    channels: u8,
    buffer_ms: Option<u32>,
    wait: Option<oneshot::Receiver<()>>,
    open_tx: oneshot::Sender<()>,
    rx: mpsc::UnboundedReceiver<Cmd>,
    kill: oneshot::Receiver<oneshot::Sender<()>>,
    tap: Option<Tap>,
    voice: Option<VoiceTap>,
    events: mpsc::UnboundedSender<Event>,
    stuck: Arc<AtomicBool>,
}

async fn run(job: Job) {
    let Job {
        id,
        aplay,
        rate,
        channels,
        buffer_ms,
        wait,
        open_tx,
        mut rx,
        mut kill,
        mut tap,
        mut voice,
        events,
        stuck,
    } = job;
    if let Some(w) = wait {
        let _ = w.await;
    }
    if let Ok(done) = kill.try_recv() {
        drop(open_tx);
        drop(done);
        let _ = events.send(Event::Done(id, true));
        return;
    }
    let mut out = match Out::open(&aplay, id, rate, channels, buffer_ms) {
        Ok(o) => {
            drop(open_tx);
            o
        }
        Err(e) => {
            drop(open_tx);
            eprintln!("{} play: stream {id}: {e}", crate::stamp());
            let _ = events.send(Event::Done(id, true));
            return;
        }
    };
    let bps = rate as f64 * channels as f64 * 2.0;
    let mut t_first: Option<Instant> = None;
    let mut sent = 0usize;
    loop {
        tokio::select! {
            Ok(done) = &mut kill => {
                out.stop().await;
                drop(done);
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
                    t_first.get_or_insert_with(Instant::now);
                    let limit = Duration::from_secs_f64(pcm.len() as f64 / bps + WATCH_PAD);
                    tokio::select! {
                        r = tokio::time::timeout(limit, out.write(&pcm)) => match r {
                            Ok(Ok(())) => sent += pcm.len(),
                            Ok(Err(e)) => {
                                // aplay is gone (a device error): the stream is over
                                let st = out.exited();
                                eprintln!("{} play: stream {id}: write to aplay: {e} (aplay exit {st:?})", crate::stamp());
                                out.stop().await;
                                let _ = events.send(Event::Done(id, true));
                                return;
                            }
                            Err(_) => {
                                eprintln!("{} play: stream {id}: write timed out — resetting the player", crate::stamp());
                                stuck.store(true, Ordering::SeqCst);
                                out.stop().await;
                                let _ = events.send(Event::Done(id, true));
                                return;
                            }
                        },
                        Ok(done) = &mut kill => {
                            out.stop().await;
                            drop(done);
                            let _ = events.send(Event::Done(id, true));
                            return;
                        }
                    }
                }
                Some(Cmd::End) | None => {
                    let elapsed = t_first.map_or(0.0, |t| t.elapsed().as_secs_f64());
                    let left = (sent as f64 / bps - elapsed).max(0.0);
                    let limit = Duration::from_secs_f64(left + WATCH_PAD);
                    tokio::select! {
                        r = tokio::time::timeout(limit, out.finish()) => match r {
                            Ok(st) => {
                                if !matches!(&st, Ok(s) if s.success()) {
                                    eprintln!("{} play: stream {id}: aplay ended with {st:?}", crate::stamp());
                                }
                                let _ = events.send(Event::Done(id, false));
                            }
                            Err(_) => {
                                eprintln!(
                                    "{} play: stream {id}: aplay did not finish in {:.1} s — resetting the player",
                                    crate::stamp(),
                                    limit.as_secs_f64()
                                );
                                stuck.store(true, Ordering::SeqCst);
                                out.stop().await;
                                let _ = events.send(Event::Done(id, true));
                            }
                        },
                        Ok(done) = &mut kill => {
                            out.stop().await;
                            drop(done);
                            let _ = events.send(Event::Done(id, true));
                        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    async fn wait_lines(path: &std::path::Path, want: usize) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let lines: Vec<String> =
                std::fs::read_to_string(path).unwrap_or_default().lines().map(str::to_string).collect();
            if lines.len() >= want || Instant::now() >= deadline {
                return lines;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn stops_softly_before_the_next_stream_opens() {
        let dir = std::env::temp_dir().join(format!("station-play-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("log");
        let aplay = script(
            &dir,
            "aplay.sh",
            &format!(
                "#!/bin/sh\ntrap 'sleep 0.06; echo stopped >> {0}; exit 0' TERM\necho started >> {0}\nwhile :; do sleep 0.02; done\n",
                log.display()
            ),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut p = Player::new(tx, None);
        p.aplay = aplay.to_string_lossy().into();

        p.play(1, 24000, 1, None, None).unwrap();
        p.data(1, vec![0; 4800]);
        assert_eq!(wait_lines(&log, 1).await, ["started"]);

        // kill 1 and open 2 back-to-back: 2 must not spawn until 1 has handled SIGTERM and exited
        p.kill(1);
        p.play(2, 24000, 1, None, None).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), rx.recv()).await,
            Ok(Some(Event::Done(1, true)))
        ));
        assert_eq!(wait_lines(&log, 3).await, ["started", "stopped", "started"]);

        p.kill(2);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), rx.recv()).await,
            Ok(Some(Event::Done(2, true)))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn watchdog_cuts_all_streams_when_one_hangs() {
        let dir = std::env::temp_dir().join(format!("station-play-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let aplay = script(&dir, "hang.sh", "#!/bin/sh\ntrap 'exit 0' TERM\nwhile :; do sleep 0.05; done\n");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut p = Player::new(tx, None);
        p.aplay = aplay.to_string_lossy().into();

        // stream 1 stays open (music), stream 2 finishes sending data and hangs past WATCH_PAD
        p.play(1, 24000, 1, Some(0.5), None).unwrap();
        p.data(1, vec![0; 4800]);
        p.play(2, 24000, 1, None, None).unwrap();
        p.data(2, vec![0; 4800]);
        p.end(2);

        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), rx.recv()).await,
            Ok(Some(Event::Done(2, true)))
        ));
        p.finished(2);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), rx.recv()).await,
            Ok(Some(Event::Done(1, true)))
        ));
        p.finished(1);
        assert!(p.streams.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
