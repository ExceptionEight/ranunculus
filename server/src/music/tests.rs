//! The player against a stand-in music service and a station with no link.
use super::*;
use crate::store::Store;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio_tungstenite::tungstenite::Message;

/// A music service of a few lines (std only) on `listener`: /v1/audio gives `secs` of silence (X-Duration, paced at
/// about real time), 502 for an id with «mute» in it; while `alive` is off it drops every connection and cuts the
/// streams on. → the requests it saw (their paths)
fn service(listener: TcpListener, secs: f64, alive: Arc<AtomicBool>) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(vec![]));
    let s = seen.clone();
    std::thread::spawn(move || {
        for c in listener.incoming() {
            let Ok(mut c) = c else { return };
            if !alive.load(Ordering::SeqCst) {
                continue; // dropped: closed at once
            }
            let (s, alive) = (s.clone(), alive.clone());
            std::thread::spawn(move || {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap_or(0) <= 2 {
                        break;
                    }
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                s.lock().unwrap().push(path.clone());
                if path.contains("id=ytm%3Amute") {
                    let body = r#"{"error": "нет звука: YouTube не отдаёт этот трек"}"#;
                    let _ = write!(c, "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                   body.len());
                    return;
                }
                let start: f64 = path
                    .split("start=")
                    .nth(1)
                    .and_then(|v| v.split('&').next())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.0);
                let n = ((secs - start).max(0.0) * SR as f64) as usize * CH * 2;
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: audio/L16\r\nX-Duration: {secs}\r\nContent-Length: {n}\r\nConnection: close\r\n\r\n");
                let block = SR / 10 * CH * 2;
                let mut left = n;
                while left > 0 {
                    if !alive.load(Ordering::SeqCst) || c.write_all(&vec![0u8; block.min(left)]).is_err() {
                        let _ = c.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                    left -= block.min(left);
                    std::thread::sleep(Duration::from_millis(90));
                }
            });
        }
    });
    seen
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn queue(ids: &[&str]) -> Vec<Track> {
    ids.iter()
        .map(|id| Track {
            id: id.to_string(),
            title: format!("Song {id}"),
            artist: "A".into(),
            duration: None,
            album: None,
            isrc: None,
        })
        .collect()
}

/// A player on `service` with a server.db of its own and a stand-in station. → (player, store, its folder, PCM bytes sent)
fn player(service: &str, device: &str) -> (Arc<Music>, Arc<Store>, std::path::PathBuf, Arc<AtomicUsize>) {
    let d = std::env::temp_dir().join(format!("music-test-{}-{}", std::process::id(), listens::next_id()));
    std::fs::create_dir_all(&d).unwrap();
    let store = Store::open(d.join("server.db").to_str().unwrap()).unwrap();
    let m = Music::new(service, device, Some(listens::Writer::start(store.clone())));
    let (f, mut rx) = Station::for_test(device);
    m.attach(f);
    let pcm = Arc::new(AtomicUsize::new(0));
    let p = pcm.clone();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Message::Binary(b) = msg {
                p.fetch_add(b.len(), Ordering::SeqCst);
            }
        }
    });
    (m, store, d, pcm)
}

async fn until(what: &str, secs: f64, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed().as_secs_f64() < secs, "{what}: not in {secs} s");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// (track, heard, ended) of every listen kept, in order (the writer is given a moment).
async fn kept(store: &Store) -> Vec<(String, f64, Option<String>)> {
    tokio::time::sleep(Duration::from_millis(300)).await;
    let db = store.db.lock().unwrap();
    let mut st = db.prepare("SELECT track, heard, ended FROM listens ORDER BY t").unwrap();
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().collect::<Result<_, _>>().unwrap()
}

fn tracks_kept(store: &Store) -> Vec<String> {
    let db = store.db.lock().unwrap();
    let mut st = db.prepare("SELECT id FROM tracks ORDER BY id").unwrap();
    st.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_service_down_pauses_and_the_queue_stays() {
    let port = free_port(); // nobody there yet
    let (m, store, d, pcm) = player(&format!("http://127.0.0.1:{port}"), "test-down");
    m.start(|st| st.queue = queue(&["ytm:a", "ytm:b", "ytm:c"]));
    until("a pause", DOWN_FOR.as_secs_f64() + 3.0, || m.st.lock().unwrap().state == State::Pause).await;
    {
        let st = m.st.lock().unwrap();
        assert_eq!((st.pos, st.queue.len(), st.offset), (0, 3, 0.0), "the queue and the place stay");
        assert!(st.listen.is_none());
    }
    assert!(kept(&store).await.is_empty(), "a track that never sounded: no listen");
    assert!(tracks_kept(&store).is_empty());
    // the service back: «продолжи» — the same track
    let alive = Arc::new(AtomicBool::new(true));
    let seen = service(TcpListener::bind(("127.0.0.1", port)).unwrap(), 6.0, alive.clone());
    assert_eq!(m.control("resume")["state"], "play");
    until("its sound", 5.0, || m.st.lock().unwrap().offset > 1.5).await;
    assert!(seen.lock().unwrap()[0].contains("ytm%3Aa"), "{:?}", seen.lock().unwrap());
    // gone mid-track: the stream breaks, then nobody answers — a pause where the listener is, the listen open
    alive.store(false, Ordering::SeqCst);
    until("a pause mid-track", DOWN_FOR.as_secs_f64() + 4.0, || m.st.lock().unwrap().state == State::Pause).await;
    let at = {
        let st = m.st.lock().unwrap();
        assert_eq!(st.pos, 0);
        assert!(st.offset > 1.0 && st.offset < 5.5, "where the listener is: {}", st.offset);
        assert!(st.listen.as_ref().is_some_and(|l| l.heard > 1.0));
        st.offset
    };
    let k = kept(&store).await;
    assert_eq!(k.len(), 1);
    assert_eq!((k[0].0.as_str(), k[0].2.as_deref()), ("ytm:a", None), "the same listen, open (paused)");
    // back again: on from that place, the same listen
    alive.store(true, Ordering::SeqCst);
    m.control("resume");
    until("on again", 5.0, || m.st.lock().unwrap().offset > at + 0.5).await;
    let last = seen.lock().unwrap().last().cloned().unwrap();
    let start: f64 = last.split("start=").nth(1).unwrap().split('&').next().unwrap().parse().unwrap();
    assert!((start - at).abs() < 0.3, "asked from {start}, paused at {at}");
    m.control("stop");
    let k = kept(&store).await;
    assert_eq!(k.len(), 1, "{k:?}");
    assert_eq!(k[0].2.as_deref(), Some("stop"));
    assert!(k[0].1 > at, "heard both stretches: {}", k[0].1);
    assert_eq!(tracks_kept(&store), ["ytm:a"]);
    assert!(pcm.load(Ordering::SeqCst) > 0);
    std::fs::remove_dir_all(d).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_audio_is_the_next_track_and_no_listen() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = service(l, 1.0, Arc::new(AtomicBool::new(true)));
    let (m, store, d, _) = player(&format!("http://127.0.0.1:{port}"), "test-mute");
    m.start(|st| st.queue = queue(&["ytm:mute1", "ytm:a", "ytm:mute2"]));
    until("the queue played out", 10.0, || m.st.lock().unwrap().state == State::Stop).await;
    assert_eq!(seen.lock().unwrap().len(), 3, "each asked once: {:?}", seen.lock().unwrap());
    let k = kept(&store).await;
    assert_eq!(k.len(), 1, "{k:?}");
    assert_eq!((k[0].0.as_str(), k[0].2.as_deref()), ("ytm:a", Some("end")));
    assert_eq!(tracks_kept(&store), ["ytm:a"], "no row for a track that never sounded");
    std::fs::remove_dir_all(d).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn next_on_the_last_track_ends_the_queue() {
    let (m, store, d, _) = player(&format!("http://127.0.0.1:{}", free_port()), "test-last");
    let on = |m: &Arc<Music>, pos: usize| {
        m.change(|st| {
            st.queue = queue(&["ytm:a", "ytm:b"]);
            st.queue[1].duration = Some(200.0);
            st.pos = pos;
            st.state = State::Pause;
            st.offset = 12.0;
            st.listen = Some(Listen {
                id: listens::next_id(),
                track: st.queue[pos].clone(),
                t: now_s(),
                heard: 12.0,
                reached: 12.0,
            });
        });
    };
    // «дальше» on the last: the queue has played out — stopped, not the last again as a new listen
    on(&m, 1);
    let r = m.control("next");
    assert_eq!((r["ok"].as_bool(), r["state"].as_str()), (Some(true), Some("stop")), "{r}");
    assert!(!m.has_queue() && m.st.lock().unwrap().listen.is_none());
    // a seek past the last one's end: the same
    on(&m, 1);
    let r = m.seek(None, Some(500.0));
    assert_eq!(r["state"], "stop", "{r}");
    // «назад» on the first: it again from its start (a new listen)
    on(&m, 0);
    let r = m.control("previous");
    assert_eq!(r["state"], "play", "{r}");
    {
        let st = m.st.lock().unwrap();
        assert_eq!((st.pos, st.offset), (0, 0.0));
    }
    m.control("stop");
    let k = kept(&store).await;
    let how: Vec<_> = k.iter().map(|(t, _, e)| (t.as_str(), e.as_deref())).collect();
    assert_eq!(how, [("ytm:b", Some("next")), ("ytm:b", Some("seek")), ("ytm:a", Some("previous"))]);
    std::fs::remove_dir_all(d).unwrap();
}
