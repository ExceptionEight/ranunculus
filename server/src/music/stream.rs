//! Playing: the queue's tracks one after another, each one's audio from the music service to the station, paced and
//! levelled; every change of the player ends the task that plays (`gen`).
use super::*;
use futures_util::StreamExt;

/// How one track's play ended.
enum End {
    /// to its end
    Done,
    /// no audio for it (the service said so, or its stream broke TRIES times): the next one
    Failed,
    /// the music service does not answer (DOWN_FOR): pause, the queue and the place stay
    Down,
    /// replaced, stopped, paused
    Replaced,
}

impl Music {
    pub(super) async fn run(self: Arc<Self>, gen: u64) {
        loop {
            let (track, start) = {
                let mut st = self.st.lock().unwrap();
                if !self.current(gen) {
                    return;
                }
                if st.pos >= st.queue.len() {
                    st.state = State::Stop;
                    st.offset = 0.0;
                    st.repeat = false;
                    drop(st);
                    self.notify();
                    log(&self.device, "music: the queue has played out");
                    return;
                }
                let track = st.queue[st.pos].clone();
                if st.listen.as_ref().is_some_and(|l| l.track.id != track.id) {
                    self.end_listen(&mut st, "other");
                }
                // a new listen begins with the track's first sound (stream)
                (track, st.offset)
            };
            log(
                &self.device,
                format!(
                    "music: a track starts{}",
                    if start > 0.5 { format!(" (from {start:.0} s)") } else { String::new() }
                ),
            );
            let end = self.track(gen, &track, start).await;
            let mut st = self.st.lock().unwrap();
            if !self.current(gen) {
                return;
            }
            match end {
                End::Done => {
                    self.end_listen(&mut st, "end");
                    if !st.repeat {
                        st.pos += 1;
                    }
                    st.offset = 0.0;
                }
                End::Failed => {
                    self.end_listen(&mut st, "broken");
                    st.pos += 1;
                    st.offset = 0.0;
                    st.repeat = false;
                }
                End::Down => {
                    // the place, the queue and the listen as they are: «продолжи» tries again (no new generation —
                    // this task is the one playing, and it ends here)
                    st.state = State::Pause;
                    self.keep(&st, None);
                    drop(st);
                    self.notify();
                    log(&self.device, "music: the service does not answer: paused, the queue stays");
                    return;
                }
                End::Replaced => {}
            }
        }
    }

    /// One track from `start` s. → how it ended: the service's answer «no audio» (an HTTP error) — the track's; no answer
    /// at all (refused, timed out) for DOWN_FOR — the service's.
    async fn track(&self, gen: u64, track: &Track, mut start: f64) -> End {
        let mut track = track.clone();
        let mut broken = 0;
        let mut down: Option<Instant> = None; // since when the service does not answer
        loop {
            let url = format!("{}/v1/audio", self.service);
            let q = [("id", track.id.clone()), ("start", format!("{start:.2}"))];
            let r = tokio::select! {
                r = self.http.get(&url).query(&q).send() => r,
                _ = self.until_changed(gen) => return End::Replaced,
            };
            let r = match r {
                Ok(r) if r.status().is_success() => r,
                Ok(r) => {
                    // the service found no audio at all (it has tried itself): the next track
                    let e: Value = r.json().await.unwrap_or(Value::Null);
                    log(
                        &self.device,
                        format!(
                            "music: no audio for the track: {}",
                            e["error"].as_str().unwrap_or("the service gave no reason")
                        ),
                    );
                    return End::Failed;
                }
                Err(e) => {
                    let since = *down.get_or_insert_with(|| {
                        log(&self.device, format!("music: the service: {e}"));
                        Instant::now()
                    });
                    if since.elapsed() >= DOWN_FOR {
                        return End::Down;
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(DOWN_EVERY) => {}
                        _ = self.until_changed(gen) => return End::Replaced,
                    }
                    continue;
                }
            };
            if down.take().is_some() {
                log(&self.device, "music: the service answers again");
            }
            // the audio's own length: the catalogue's is rounded, or missing
            if let Some(d) = r
                .headers()
                .get("x-duration")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<f64>().ok())
                .filter(|d| *d > 0.0)
            {
                track.duration = Some(d);
                let mut st = self.st.lock().unwrap();
                if self.current(gen) {
                    let pos = st.pos;
                    if let Some(t) = st.queue.get_mut(pos).filter(|t| t.id == track.id) {
                        t.duration = Some(d);
                    }
                    if let Some(l) = st.listen.as_mut().filter(|l| l.track.id == track.id) {
                        l.track.duration = Some(d);
                    }
                }
            }
            match self.stream(gen, r, &track, start).await {
                Some(true) => return End::Done,
                Some(false) => {
                    broken += 1;
                    if broken >= TRIES {
                        return End::Failed;
                    }
                    // the connection to the service broke mid-track: once more from where the listener is
                    start = self.st.lock().unwrap().offset;
                    log(&self.device, format!("music: the service's stream broke at {start:.0} s: once more"));
                }
                None => return End::Replaced,
            }
        }
    }

    async fn until_changed(&self, gen: u64) {
        loop {
            let n = self.changed.notified();
            if !self.current(gen) {
                return;
            }
            n.await;
        }
    }

    /// The service's audio to the station. → Some(true): to its end; Some(false): the service's stream broke; None: replaced.
    async fn stream(&self, gen: u64, r: reqwest::Response, track: &Track, start: f64) -> Option<bool> {
        let Some(f) = self.station() else {
            self.halt(State::Pause);
            return None;
        };
        let (id, done) = f.open(SR as u32, CH as u8, Some(LEAD + HEARD_LAG), Some(300));
        {
            let mut st = self.st.lock().unwrap();
            if !self.current(gen) {
                drop(st);
                f.kill(id);
                return None;
            }
            st.stream = Some(id);
            st.start = start;
            st.t0 = Some(Instant::now());
            st.sent = 0.0;
        }
        let block = FRAMES * CH * 2;
        let step = FRAMES as f32 / (0.4 * SR as f32); // gain change per block: a full swing in ~0.4 s
        let t0 = Instant::now();
        let mut sent_frames = 0usize;
        let mut buf: Vec<u8> = Vec::with_capacity(block * 4);
        let mut body = r.bytes_stream();
        let mut kept = Instant::now(); // the listen kept every 10 s: a server that stops loses no more
        let mut ok = true;
        let mut eof = false;
        while !eof || buf.len() >= 4 {
            if buf.len() < block && !eof {
                let c = tokio::select! {
                    c = body.next() => c,
                    _ = self.until_changed(gen) => return None,
                };
                match c {
                    Some(Ok(b)) => buf.extend_from_slice(&b),
                    Some(Err(_)) => {
                        ok = false;
                        eof = true;
                    }
                    None => eof = true,
                }
                continue;
            }
            let n = buf.len().min(block) / 4 * 4;
            let raw: Vec<u8> = buf.drain(..n).collect();
            if !ok && raw.is_empty() {
                break;
            }
            let frames = raw.len() / (CH * 2);
            let (g0, g1) = {
                let target = self.target();
                let mut g = self.gain.lock().unwrap();
                let g0 = *g;
                let g1 = if g0 < target { (g0 + step).min(target) } else { (g0 - step).max(target) };
                *g = g1;
                (g0, g1)
            };
            let mut out = Vec::with_capacity(raw.len());
            for (i, s) in raw.as_chunks::<2>().0.iter().enumerate() {
                let frame = i / CH;
                let g = g0 + (g1 - g0) * if frames > 1 { frame as f32 / (frames - 1) as f32 } else { 1.0 };
                let v = (i16::from_le_bytes(*s) as f32 * g).round().clamp(-32768.0, 32767.0) as i16;
                out.extend_from_slice(&v.to_le_bytes());
            }
            let ahead = sent_frames as f64 / SR as f64 - t0.elapsed().as_secs_f64();
            if ahead > LEAD {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs_f64(ahead - LEAD)) => {}
                    _ = self.until_changed(gen) => return None,
                }
            }
            if !self.current(gen) {
                return None;
            }
            if !f.pcm(id, out) {
                self.halt(State::Pause); // the station is gone
                return None;
            }
            sent_frames += frames;
            let mut st = self.st.lock().unwrap();
            if self.current(gen) {
                st.sent = sent_frames as f64 / SR as f64;
                st.offset = start + st.sent;
                if st.listen.is_none() {
                    // the listen begins with the first sound: begun before the request, a track with no audio, or a
                    // queue the service failed, would leave a row per track
                    st.listen = Some(Listen {
                        id: listens::next_id(),
                        track: track.clone(),
                        t: now_s(),
                        heard: 0.0,
                        reached: start,
                    });
                    self.keep(&st, None);
                    kept = Instant::now();
                } else if kept.elapsed() >= Duration::from_secs(10) {
                    kept = Instant::now();
                    self.keep(&st, None);
                }
            }
        }
        let got = start + sent_frames as f64 / SR as f64;
        let duration = track.duration;
        if ok && duration.is_some_and(|d| got < d - SHORT_BY) {
            log(
                &self.device,
                format!("music: the stream ended at {got:.0} s of {:.0}: broken", duration.unwrap_or(0.0)),
            );
            ok = false;
        }
        if !ok {
            f.end(id);
            let mut st = self.st.lock().unwrap();
            if self.current(gen) {
                close_stretch(&mut st);
                st.stream = None;
            }
            return Some(false);
        }
        f.end(id); // the tail plays out, then the next track
        let wait = tokio::time::timeout(Duration::from_secs_f64(LEAD + 3.0), done);
        tokio::select! {
            _ = wait => {}
            _ = self.until_changed(gen) => return None,
        }
        let mut st = self.st.lock().unwrap();
        if self.current(gen) {
            st.stream = None;
            close_stretch(&mut st);
        }
        Some(true)
    }
}
