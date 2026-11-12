//! The phrases said while a web tool runs («Ищу в интернете.»), synthesized ahead in the voice in use and kept on
//! disk, so they play the moment a search starts instead of after the synthesis. Made in the background at the first
//! request and again when the voice changes; until they are ready the phrase is synthesized on the spot. Keyed by
//! the voice (its service, model, name, speed) and the text; files 600: the voice may be a person's.
use crate::services::{read_wav, wav, Services, Speech};
use crate::session::log;
use rand::seq::IndexedRandom;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const FILLERS: [(&str, &[&str]); 2] = [
    (
        "web_search",
        &[
            "Ищу в интернете.",
            "Смотрю в интернете.",
            "Сейчас поищу.",
            "Секунду, гляну в интернете.",
            "Сейчас посмотрю в сети.",
            "Ща, погуглю.",
        ],
    ),
    ("read_page", &["Открываю страницу.", "Сейчас открою.", "Читаю страницу."]),
];
pub fn kind_of(name: &str) -> Option<&'static [&'static str]> {
    FILLERS.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

/// A voice that did not answer is asked for the rest again this long after.
const RETRY: Duration = Duration::from_secs(60);

pub struct Fillers {
    dir: String,
    /// the clips and the voice they are in
    clips: Mutex<(Option<Services>, HashMap<String, Arc<Speech>>)>,
    /// some not made: the voice did not answer then
    missing: Mutex<Option<Instant>>,
    last: Mutex<HashMap<String, String>>,
    preparing: AtomicBool,
}

impl Fillers {
    pub fn new(dir: &str) -> Arc<Fillers> {
        Arc::new(Fillers {
            dir: dir.into(),
            clips: Default::default(),
            missing: Default::default(),
            last: Default::default(),
            preparing: AtomicBool::new(false),
        })
    }

    fn ready_in(&self, services: &Services) -> bool {
        self.clips.lock().unwrap().0.as_ref().is_some_and(|v| v.same_voice(services))
            && self.missing.lock().unwrap().is_none_or(|t| t.elapsed() < RETRY)
    }

    /// The clips in `services`' voice, made in the background: nothing to do when they are there or on their way.
    pub fn prepare(self: &Arc<Self>, services: &Services) {
        if self.ready_in(services) || self.preparing.swap(true, Ordering::SeqCst) {
            return;
        }
        let (me, services) = (self.clone(), services.clone());
        tokio::spawn(async move {
            me.make(services).await;
            me.preparing.store(false, Ordering::SeqCst);
        });
    }

    /// Where a phrase in a voice is kept.
    fn path(&self, voice: u64, text: &str) -> String {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (voice, text).hash(&mut h);
        format!("{}/{:016x}.wav", self.dir, h.finish())
    }

    /// Every phrase from the cache, or synthesized and cached. A voice that does not answer (the service down, the
    /// network not up yet): the rest are synthesized as they are needed, and asked for again a minute later.
    async fn make(&self, services: Services) {
        let Some(voice) = services.voice_id() else {
            *self.clips.lock().unwrap() = (Some(services), HashMap::new());
            *self.missing.lock().unwrap() = None;
            return;
        };
        if std::fs::create_dir_all(&self.dir).is_ok() {
            let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
        }
        let (mut clips, mut made, mut down) = (HashMap::new(), 0, false);
        for text in FILLERS.iter().flat_map(|(_, v)| v.iter()) {
            let path = self.path(voice, text);
            let sp = match std::fs::read(&path).ok().and_then(|b| read_wav(&b)) {
                Some((pcm, rate)) => Speech { pcm, rate },
                // the cached ones still read
                None if down => continue,
                None => match services.speak(text).await {
                    Ok(sp) => {
                        if std::fs::write(&path, wav(&sp.pcm, sp.rate)).is_ok() {
                            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                        }
                        made += 1;
                        sp
                    }
                    Err(e) => {
                        log("server", format!("search phrases: {e}: they will be synthesized as they are needed"));
                        down = true;
                        continue;
                    }
                },
            };
            clips.insert(text.to_string(), Arc::new(sp));
        }
        if !clips.is_empty() {
            log(
                "server",
                format!(
                    "search phrases ready: {}{}",
                    clips.len(),
                    if made > 0 { format!(" ({made} synthesized)") } else { String::new() }
                ),
            );
        }
        *self.clips.lock().unwrap() = (Some(services), clips);
        *self.missing.lock().unwrap() = down.then(Instant::now);
    }

    /// → (text, its audio if ready in `services`' voice): a random phrase of this kind, not the one said last time.
    pub fn pick(&self, kind: &str, services: &Services) -> Option<(String, Option<Arc<Speech>>)> {
        let options = kind_of(kind)?;
        let mut last = self.last.lock().unwrap();
        let fresh: Vec<&&str> = options.iter().filter(|t| Some(**t) != last.get(kind).map(|s| s.as_str())).collect();
        let text = if fresh.is_empty() {
            options.choose(&mut rand::rng())?.to_string()
        } else {
            fresh.choose(&mut rand::rng())?.to_string()
        };
        last.insert(kind.into(), text.clone());
        let clips = self.clips.lock().unwrap();
        let clip = clips.0.as_ref().filter(|v| v.same_voice(services)).and_then(|_| clips.1.get(&text).cloned());
        Some((text, clip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_the_same_twice() {
        let f = Fillers::new("/nonexistent");
        let services = Services::new(None, None, None);
        let mut prev = String::new();
        for _ in 0..20 {
            let (t, clip) = f.pick("read_page", &services).unwrap();
            assert!(clip.is_none());
            assert_ne!(t, prev);
            prev = t;
        }
        assert!(f.pick("calculate", &services).is_none());
    }

    #[tokio::test]
    async fn a_voice_that_did_not_answer_is_asked_again() {
        let svc = crate::services::fake::Service::start().await;
        let dir = std::env::temp_dir().join(format!("server-fillers-{}-{}", std::process::id(), rand::random::<u32>()));
        let f = Fillers::new(&dir.to_string_lossy());
        let tts = crate::services::Endpoint {
            url: svc.url.clone(),
            model: "m".into(),
            voice: "nobody".into(),
            ..Default::default()
        };
        let services = Services::new(None, Some(tts), None);
        // one phrase made before, while the voice answered
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(f.path(services.voice_id().unwrap(), "Читаю страницу."), wav(&[0u8; 3200], 16000)).unwrap();
        f.make(services.clone()).await;
        assert_eq!(f.clips.lock().unwrap().1.keys().collect::<Vec<_>>(), ["Читаю страницу."]);
        assert_eq!(svc.asked().len(), 1, "a voice that does not answer is asked once, not for every phrase");
        assert!(f.ready_in(&services), "and not again at once");
        *f.missing.lock().unwrap() = Instant::now().checked_sub(RETRY);
        assert!(!f.ready_in(&services), "but a minute later");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
