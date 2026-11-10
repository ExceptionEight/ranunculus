//! What happens on the server, for the web UI's live view (GET /api/events, server-sent events): a device connects, the
//! panel changes (listening, thinking, speaking), the volume, the music, a ring; what was heard and what is said, sentence
//! by sentence; an exchange written to the chats; the settings, the timers, the sounds changed. Anything may emit, from
//! anywhere (one bus for the process); nobody listening costs nothing.
use serde_json::Value;
use std::sync::{Arc, OnceLock};
use tokio::sync::broadcast;

/// An event as it goes out: its kind (the SSE `event:`) and its data, serialized once for every listener.
#[derive(Clone, Debug)]
pub struct Ev {
    pub kind: &'static str,
    pub data: Arc<str>,
}

/// Events a slow listener may fall behind by before it is told to fetch everything again (`resync`).
const BACKLOG: usize = 512;

fn bus() -> &'static broadcast::Sender<Ev> {
    static BUS: OnceLock<broadcast::Sender<Ev>> = OnceLock::new();
    BUS.get_or_init(|| broadcast::channel(BACKLOG).0)
}

pub fn emit(kind: &'static str, data: Value) {
    let b = bus();
    if b.receiver_count() > 0 {
        let _ = b.send(Ev { kind, data: data.to_string().into() });
    }
}

pub fn subscribe() -> broadcast::Receiver<Ev> {
    bus().subscribe()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn to_every_listener_in_order() {
        emit("nobody", json!({})); // no listener: dropped, no error
        let (mut a, mut b) = (subscribe(), subscribe());
        emit("test.one", json!({"n": 1}));
        emit("test.two", json!({"n": 2}));
        for r in [&mut a, &mut b] {
            // other tests (same process) may emit meanwhile: only these two count
            let mut got = vec![];
            while got.len() < 2 {
                let e = r.recv().await.unwrap();
                if e.kind.starts_with("test.") {
                    got.push(e);
                }
            }
            assert_eq!((got[0].kind, &*got[0].data, got[1].kind), ("test.one", r#"{"n":1}"#, "test.two"));
        }
    }
}
