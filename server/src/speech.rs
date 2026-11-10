//! The answer as speech: the sentences the model writes → the synthesizer → the station.
//! - Sentences that queued up while the previous piece was being synthesized go to the synthesizer as one piece (up to
//!   BATCH characters): whole phrases sound more connected, and a neural synthesizer pays its fixed
//!   cost once. The answer's first sentence waits up to HOLD s for the rest: a short answer then goes whole.
//! - GAP s of silence after every piece; a piece too long for the synthesizer (HTTP 507: over its memory cap) is redone
//!   sentence by sentence; any other failure is logged and skipped — one bad call never silences the rest.
//! - Ready audio (a filler phrase) goes in its turn, no synthesis. Whatever is synthesized by the time a stream starts
//!   goes out in one stream (the station does not restart its player between sentences).
//! - The panel: «speak» while the answer sounds, «search» when only the filler was said and the search goes on.
use crate::services::{beeps, Services, Speech};
use crate::session::{log, Gen, Station};
use crate::text::now_s;
use regex::Regex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{mpsc, Notify};

const BATCH: usize = 150; // longest text sent to the synthesizer in one call (OmniVoice under its 0.8 GB cap: ~160)
const HOLD: f64 = 0.5;
const GAP: f64 = 0.12;

/// What the answer's queue holds: a sentence to synthesize (with `beeps` before it: a timer), or ready audio.
pub enum Say {
    Text { text: String, beeps: usize },
    Clip(Arc<Speech>),
}

/// The answer's sentences, in order; `finish()` once the whole text is in.
#[derive(Default)]
pub struct Queue {
    inner: Mutex<(VecDeque<Say>, bool)>,
    notify: Notify,
}

impl Queue {
    pub fn put(&self, s: Say) {
        self.inner.lock().unwrap().0.push_back(s);
        self.notify.notify_one();
    }

    pub fn text(&self, text: impl Into<String>) {
        self.put(Say::Text { text: text.into(), beeps: 0 });
    }

    pub fn finish(&self) {
        self.inner.lock().unwrap().1 = true;
        self.notify.notify_one();
    }

    async fn next(&self) -> Option<Say> {
        loop {
            let n = self.notify.notified();
            {
                let mut q = self.inner.lock().unwrap();
                if let Some(s) = q.0.pop_front() {
                    return Some(s);
                }
                if q.1 {
                    return None;
                }
            }
            n.await;
        }
    }

    /// (characters of the texts queued, each +1 for the space; the whole answer is in)
    fn waiting(&self) -> (usize, bool) {
        let q = self.inner.lock().unwrap();
        (q.0.iter().map(|s| if let Say::Text { text, .. } = s { text.chars().count() + 1 } else { 0 }).sum(), q.1)
    }

    /// The next sentence, if it can join `len` characters already taken (no beeps, no clip, within BATCH).
    fn join(&self, len: usize) -> Option<String> {
        let mut q = self.inner.lock().unwrap();
        match q.0.front() {
            Some(Say::Text { text, beeps: 0 }) if len + text.chars().count() <= BATCH => {}
            _ => return None,
        }
        match q.0.pop_front() {
            Some(Say::Text { text, .. }) => Some(text),
            _ => None,
        }
    }
}

/// Text in pieces the synthesizer can take — cut at sentence ends, a long sentence at a comma, a long
/// clause at a space.
pub fn pieces(text: &str, size: usize) -> Vec<String> {
    static END: OnceLock<Regex> = OnceLock::new();
    let end = END.get_or_init(|| Regex::new(r"[.!?…]\s+").unwrap());
    let mut sentences = vec![];
    let mut last = 0;
    for m in end.find_iter(text) {
        let cut = m.start() + m.as_str().chars().next().unwrap().len_utf8(); // after the mark
        sentences.push(&text[last..cut]);
        last = m.end();
    }
    sentences.push(&text[last..]);
    let mut out = vec![];
    for s in sentences.into_iter().filter(|s| !s.is_empty()) {
        let mut s: Vec<char> = s.chars().collect();
        while s.len() > size {
            let window = |c: &[char], lo: usize, hi: usize, pat: &[char]| -> Option<usize> {
                let hi = hi.min(c.len());
                (lo..hi.saturating_sub(pat.len() - 1)).rev().find(|&i| c[i..i + pat.len()] == *pat)
            };
            let k = match window(&s, 30, size, &[',', ' ']) {
                Some(k) if k > 0 => k + 1,
                _ => match window(&s, 30, size, &[' ']) {
                    Some(k) if k > 0 => k,
                    _ => size,
                },
            };
            out.push(s[..k].iter().collect::<String>().trim().to_string());
            s = s[k..].iter().collect::<String>().trim().chars().collect();
        }
        if !s.is_empty() {
            out.push(s.into_iter().collect());
        }
    }
    out
}

fn silence(rate: u32, s: f64) -> Vec<u8> {
    vec![0u8; (s * rate as f64) as usize * 2]
}

/// The synthesizer on one piece: on 507 sentence by sentence (with gaps), any other failure — nothing.
async fn synth(services: &Services, text: &str, dev: &str) -> Option<Speech> {
    match services.speak(text).await {
        Ok(sp) => Some(sp),
        Err(e) if e.code == Some(507) && pieces(text, 120).len() > 1 => {
            let mut out: Option<Speech> = None;
            for p in pieces(text, 120) {
                if let Some(sp) = Box::pin(synth(services, &p, dev)).await {
                    let o = out.get_or_insert(Speech { pcm: vec![], rate: sp.rate });
                    if o.rate == sp.rate {
                        o.pcm.extend(sp.pcm);
                        o.pcm.extend(silence(sp.rate, GAP));
                    }
                }
            }
            out
        }
        Err(e) => {
            log(dev, format!("   ({e}: {} characters not said)", text.chars().count()));
            None
        }
    }
}

/// Plays the answer: synthesizes in the background (the next piece while one plays) and streams to the station.
/// Returns when everything has played, or the request was cancelled.
pub async fn speak(
    q: Arc<Queue>,
    station: Station,
    services: Services,
    g: Gen,
    t_wake: f64,
    searching: Arc<AtomicBool>,
) {
    let (atx, mut arx) = mpsc::channel::<Speech>(8);
    let (gs, qs, dev) = (g.clone(), q.clone(), station.device.clone());
    let synth_task = tokio::spawn(async move {
        let mut first = true;
        while let Some(item) = qs.next().await {
            if gs.cancelled() {
                break;
            }
            let sp = match item {
                Say::Clip(c) => {
                    let mut sp = (*c).clone();
                    sp.pcm.extend(silence(sp.rate, GAP));
                    Some(sp)
                }
                Say::Text { text, beeps: n } => {
                    if first && n == 0 {
                        // the answer's first piece: give a short answer a moment to end
                        let until = now_s() + HOLD;
                        loop {
                            let (queued, ended) = qs.waiting();
                            if ended || now_s() >= until || text.chars().count() + queued >= BATCH {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                    }
                    let mut text = text;
                    while n == 0 && text.chars().count() < BATCH {
                        match qs.join(text.chars().count() + 1) {
                            Some(more) => {
                                text.push(' ');
                                text.push_str(&more);
                            }
                            None => break,
                        }
                    }
                    if gs.cancelled() {
                        break;
                    }
                    synth(&services, &text, &dev).await.map(|mut sp| {
                        if n > 0 {
                            let mut b = beeps(sp.rate, n);
                            b.extend(sp.pcm);
                            sp.pcm = b;
                        }
                        sp.pcm.extend(silence(sp.rate, GAP));
                        sp
                    })
                }
            };
            first = false;
            if let Some(sp) = sp {
                if atx.send(sp).await.is_err() {
                    break;
                }
            }
        }
    });
    let mut first = true;
    let mut next: Option<Speech> = None;
    loop {
        let mut sp = match next.take() {
            Some(sp) => sp,
            None => match arx.recv().await {
                Some(sp) => sp,
                None => break,
            },
        };
        if g.cancelled() {
            break;
        }
        while let Ok(more) = arx.try_recv() {
            // everything already synthesized goes out in one piece
            if more.rate == sp.rate {
                sp.pcm.extend(more.pcm);
            } else {
                next = Some(more);
                break;
            }
        }
        if first {
            first = false;
            log(&station.device, format!("   (the first sound {:.2} s after the wake word)", now_s() - t_wake));
        }
        station.panel("speak");
        if !station.play(&sp.pcm, sp.rate, Some(&g)).await {
            break;
        }
        if next.is_none() && searching.load(Ordering::SeqCst) {
            match arx.try_recv() {
                Ok(more) => next = Some(more),
                Err(_) => station.panel("search"), // only the «ищу» phrase has played, the search goes on
            }
        }
    }
    synth_task.abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_the_synthesizer_can_take() {
        let t = "Первое предложение. Второе! А третье — очень длинное, с запятыми, которое не влезает в сто двадцать символов, \
                 поэтому его режут по запятой, а потом ещё и ещё раз, пока не станет короче.";
        assert_eq!(pieces(t, 120), vec!["Первое предложение.", "Второе!",
            "А третье — очень длинное, с запятыми, которое не влезает в сто двадцать символов, поэтому его режут по запятой,",
            "а потом ещё и ещё раз, пока не станет короче."]);
        assert_eq!(pieces("Коротко.", 120), vec!["Коротко."]);
        assert_eq!(pieces("Один.  Два?! Три… Четыре", 120), vec!["Один.", "Два?!", "Три…", "Четыре"]);
        let long = "слово ".repeat(40);
        let p = pieces(long.trim(), 120);
        assert!(p.iter().all(|x| x.chars().count() <= 120) && p.len() == 2, "{p:?}");
    }

    #[tokio::test]
    async fn queue_joins_sentences() {
        let q = Queue::default();
        q.text("Раз.");
        q.text("Два.");
        q.put(Say::Text { text: "Таймер.".into(), beeps: 3 });
        q.text("Три.");
        assert!(matches!(q.next().await, Some(Say::Text { ref text, .. }) if text == "Раз."));
        assert_eq!(q.waiting(), (5 + 8 + 5, false));
        assert_eq!(q.join(5).as_deref(), Some("Два."));
        assert_eq!(q.join(10), None, "beeps never join");
        assert_eq!(q.join(BATCH), None);
        q.finish();
        assert!(q.next().await.is_some() && q.next().await.is_some());
        assert!(q.next().await.is_none(), "finished and empty");
    }
}
