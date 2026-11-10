//! The phrases said while a web tool runs («Ищу в интернете.»), synthesized ahead in the voice in use and kept on
//! disk, so they play the moment a search starts instead of after the synthesis. Made in the background at the first
//! request and again when the voice changes; until they are ready the phrase is synthesized on the spot. Keyed by
//! the synthesizer's health (engine and settings), the speed and the text; files 600: the voice may be a person's.
use crate::services::{read_wav, wav, Services, Speech};
use crate::session::log;
use rand::seq::IndexedRandom;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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

pub struct Fillers {
    dir: String,
    /// the clips and the voice they are in
    clips: Mutex<(Option<Services>, HashMap<String, Arc<Speech>>)>,
    last: Mutex<HashMap<String, String>>,
    preparing: AtomicBool,
}

impl Fillers {
    pub fn new(dir: &str) -> Arc<Fillers> {
        Arc::new(Fillers {
            dir: dir.into(),
            clips: Default::default(),
            last: Default::default(),
            preparing: AtomicBool::new(false),
        })
    }

    fn ready_in(&self, services: &Services) -> bool {
        self.clips.lock().unwrap().0.as_ref().is_some_and(|v| v.same_voice(services))
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

    /// Every phrase from the cache, or synthesized and cached.
    async fn make(&self, services: Services) {
        // the synthesizer may still be starting (the services start together): up to a minute
        let mut health = services.tts_health().await;
        for _ in 0..30 {
            if health.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            health = services.tts_health().await;
        }
        let health = match health {
            Ok(h) => h,
            Err(e) => {
                log("server", format!("search phrases: the synthesizer did not answer ({e}): they will be synthesized as they are needed"));
                return;
            }
        };
        if std::fs::create_dir_all(&self.dir).is_ok() {
            let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
        }
        let (mut clips, mut made) = (HashMap::new(), 0);
        for text in FILLERS.iter().flat_map(|(_, v)| v.iter()) {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (&health, services.speed().to_bits(), *text).hash(&mut h);
            let path = format!("{}/{:016x}.wav", self.dir, h.finish());
            let sp = match std::fs::read(&path).ok().and_then(|b| read_wav(&b)) {
                Some((pcm, rate)) => Speech { pcm, rate },
                None => match services.speak(text).await {
                    Ok(sp) => {
                        if std::fs::write(&path, wav(&sp.pcm, sp.rate)).is_ok() {
                            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                        }
                        made += 1;
                        sp
                    }
                    Err(e) => {
                        log("server", format!("search phrase «{text}»: {e}"));
                        continue;
                    }
                },
            };
            clips.insert(text.to_string(), Arc::new(sp));
        }
        log(
            "server",
            format!(
                "search phrases ready: {}{}",
                clips.len(),
                if made > 0 { format!(" ({made} synthesized)") } else { String::new() }
            ),
        );
        *self.clips.lock().unwrap() = (Some(services), clips);
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
        let services = Services::new("http://127.0.0.1:1", "http://127.0.0.1:1", 1.0);
        let mut prev = String::new();
        for _ in 0..20 {
            let (t, clip) = f.pick("read_page", &services).unwrap();
            assert!(clip.is_none());
            assert_ne!(t, prev);
            prev = t;
        }
        assert!(f.pick("calculate", &services).is_none());
    }
}
