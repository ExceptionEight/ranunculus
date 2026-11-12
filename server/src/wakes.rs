//! Every wake, recorded: the 2 s the station heard before it fired (the word, or what the TV said that sounded like
//! it) and the command after it, as a WAV (the data folder's wakes/, 700 / 600), with what the wake word model was
//! sure of, what recognition made of the word's window and of the command, how quiet it was before the word — and the
//! user's mark in the web UI: «это я» / «не мне». The marked ones are what a wake word model is trained on: false
//! triggers (negatives) and real wakes in this room (positives).
//! Nothing is refused here: it only records. Recognition's text alone is no second check: of real wakes it confirms
//! less than half (the word comes out as «Развод», «Красота», «Разок»).
//! The word's window goes to recognition only when recognition is at home: what was said before the word is not for
//! a service in the internet (the command after it is — that is what the user chose it for).
//! Kept until the user deletes them.
use crate::events;
use crate::services::{wav, Services};
use crate::session::log;
use crate::store::Store;
use crate::text::{now_s, tz};
use serde_json::json;
use std::sync::Arc;

pub const RATE: usize = 16000;
/// the command's own start: what it had before proto 4
pub const COMMAND_PRE: usize = 6400;
/// at most this much after the word is kept (a command is cut at 10 s by the listener)
const AFTER_MAX: usize = RATE * 12;

/// A wake being heard: its window, then the command as it comes.
pub struct Recording {
    pub t: f64,
    pub p: f32,
    pub barge: bool,
    pub button: bool,
    /// samples before the detection (the first frame); 0 — an old station: no window
    pub pre: usize,
    pub pcm: Vec<i16>,
}

impl Recording {
    pub fn new(p: f32, barge: bool, button: bool, pre: u32) -> Recording {
        Recording { t: now_s(), p, barge, button, pre: pre as usize, pcm: vec![] }
    }

    /// A mic frame: the first one is the window (→ what of it the command gets: its last COMMAND_PRE samples).
    pub fn push<'a>(&mut self, frame: &'a [i16]) -> &'a [i16] {
        let first = self.pcm.is_empty();
        if self.pcm.len() < self.pre + AFTER_MAX {
            self.pcm.extend_from_slice(frame);
        }
        if first && self.pre > 0 {
            self.pre = frame.len().min(self.pre);
            return &frame[frame.len().saturating_sub(COMMAND_PRE)..];
        }
        frame
    }
}

/// dB of the window's first second against its loudest 100 ms: the TV talks on without a break, a person usually
/// pauses before the word.
pub fn quiet_before(window: &[i16]) -> Option<f64> {
    if window.len() < RATE * 3 / 2 {
        return None;
    }
    let rms = |x: &[i16]| (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt() + 1e-3;
    let before = rms(&window[..RATE]);
    let loud = window.chunks(RATE / 10).map(rms).fold(0.0, f64::max);
    Some((20.0 * (before / loud).log10() * 10.0).round() / 10.0)
}

/// A wake being written down (`save`).
pub struct Saving {
    task: tokio::task::JoinHandle<Option<(i64, Vec<i16>)>>,
    store: Arc<Store>,
    device: String,
}

/// Written down — the file and the row — on a blocking thread: the session's loop goes on, the request starts at once
/// (a WAV of up to ~450 KB, an INSERT and the row read back would stand between the command's end and the request).
pub fn save(store: &Arc<Store>, dir: &str, device: &str, rec: Recording) -> Saving {
    let (s, dir, dev) = (store.clone(), dir.to_string(), device.to_string());
    Saving {
        task: tokio::task::spawn_blocking(move || write(&s, &dir, &dev, rec)),
        store: store.clone(),
        device: device.to_string(),
    }
}

impl Saving {
    /// The command recognised (or silence: ""), for this wake; then the wake's window recognised by `services`, if
    /// recognition is at home. After the command, not before it: a recognition service at home does one at a time,
    /// and the command would wait for the window.
    pub fn heard(self, text: &str, services: Services) {
        let Saving { task, store, device } = self;
        let text = text.to_string();
        tokio::spawn(async move {
            let Ok(Some((id, window))) = task.await else { return };
            if store.set_wake(id, "heard", Some(&text)).unwrap_or(false) {
                changed(&store, id);
            }
            if window.is_empty() || services.asr.as_ref().is_none_or(|e| crate::providers::cloud(&e.url)) {
                return;
            }
            let x: Vec<f32> = window.iter().map(|&v| v as f32 / 32768.0).collect();
            match services.transcribe(&x).await {
                Ok(t) => {
                    let _ = store.set_wake(id, "heard_window", Some(&t));
                    changed(&store, id);
                }
                Err(e) => log(&device, format!("   (wake {id}: {e})")),
            }
        });
    }
}

/// The file and the row. → its id and the window
fn write(store: &Arc<Store>, dir: &str, device: &str, rec: Recording) -> Option<(i64, Vec<i16>)> {
    let stamp = chrono::Utc::now().with_timezone(&tz()).format("%Y%m%d-%H%M%S%.3f").to_string();
    let file = format!("{stamp}-{}.wav", device.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "_"));
    let path = std::path::Path::new(dir).join(&file);
    let bytes: Vec<u8> = rec.pcm.iter().flat_map(|v| v.to_le_bytes()).collect();
    let w = (|| -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&path)?;
        f.write_all(&wav(&bytes, RATE as u32))
    })();
    if let Err(e) = w {
        log(device, format!("   (the wake is not recorded: {}: {e})", path.display()));
        return None;
    }
    let window: Vec<i16> = rec.pcm[..rec.pre.min(rec.pcm.len())].to_vec();
    let row = json!({"t": rec.t, "device": device, "p": rec.p, "barge": rec.barge, "button": rec.button, "pre": rec.pre as f64 / RATE as f64,
                     "seconds": rec.pcm.len() as f64 / RATE as f64, "file": file, "quiet_before": quiet_before(&window)});
    let id = match store.add_wake(&row) {
        Ok(id) => id,
        Err(e) => {
            log(device, format!("   (the wake is not recorded: {e})"));
            return None;
        }
    };
    changed(store, id);
    Some((id, window))
}

pub fn changed(store: &Store, id: i64) {
    if let Some(w) = store.wake(id) {
        events::emit("wake", w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_and_the_command() {
        let mut r = Recording::new(0.9, false, false, 32000);
        let window: Vec<i16> = (0..32000).map(|i| (i % 100) as i16).collect();
        let to_listener = r.push(&window).to_vec();
        assert_eq!(to_listener.len(), COMMAND_PRE, "the command gets what it had before");
        assert_eq!(to_listener[..], window[32000 - COMMAND_PRE..]);
        assert_eq!(r.push(&[1, 2, 3]), &[1, 2, 3]);
        assert_eq!((r.pre, r.pcm.len()), (32000, 32003));
        // an old station: no window
        let mut old = Recording::new(0.9, false, false, 0);
        assert_eq!(old.push(&[5; 6400]).len(), 6400);
        // the TV talking on: the first second as loud as the word; a pause before the word: far below
        let tv: Vec<i16> = (0..32000).map(|i| ((i as f64 * 0.3).sin() * 3000.0) as i16).collect();
        assert!(quiet_before(&tv).unwrap() > -3.0);
        let mut pause = vec![0i16; 16000];
        pause.extend(tv[16000..].iter());
        assert!(quiet_before(&pause).unwrap() < -40.0);
        assert_eq!(quiet_before(&[0; 8000]), None);
    }
}
