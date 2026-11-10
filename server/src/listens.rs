//! The music as it was listened to: every time a track is put on — a listen — with how much of it
//! sounded, how far into it it got and how it ended. The player (music.rs) tells; a writer (`Writer`) keeps it in server.db.
//! The verdicts are counted when shown, from these facts (a rule changed re-counts the whole history):
//! - played («прослушано»): it sounded ≥ 30 s — a track shorter than that: it got to its end (90 %);
//! - finished («до конца»): played, and it got within 10 s of its end — a shorter one: to its end (90 %); a track of
//!   unknown length (a live stream has none): played, and it ended by itself. Where it got, not «ended by itself»: a
//!   stream that breaks looks like its end;
//! - skipped («пропущено»): not played, and the listener moved on to other music — next, previous, a new request, a seek
//!   past its end. Stopped, paused for good, broken, lost: neither (it says nothing about the song).
//!
//! A pause, a seek, «сначала» with more than 30 s left: the same listen; the repeat, «сначала» nearer the end: a new one.
//! A song is one recording across services and uploads (a clip and the audio on YouTube, Spotify's, Yandex's): the same
//! first artist and title, «(Official Video)», «feat. …» and such left out; an ISRC (where a service gives one) links a
//! new track to the song of a track that has it already. `tracks.song` is kept, so a better rule re-keys them all once
//! (`SONG_RULE`).
use crate::events;
use crate::session::log;
use crate::store::{sql, Store};
use crate::text::now_s;
use regex::Regex;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{mpsc, Arc, OnceLock};

/// s a track must sound to count as played
pub const PLAYED: f64 = 30.0;
/// s from its end within which a played track counts as finished
pub const END_LEFT: f64 = 10.0;
/// «сначала» with this much left or less: a new listen (the song again), else the same one
pub const RESTART_NEW: f64 = 30.0;
/// how a listen ends when the listener moved on to other music
const MOVED_ON: &str = "'next', 'previous', 'other', 'seek'";
/// the version of `song_key`'s rule: changed, every track's song is keyed again once (`resong`) — bump it with the rule
pub const SONG_RULE: &str = "1";

/// A listen as the player knows it now (open: `ended` None).
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub id: i64,
    pub device: String,
    pub track: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    /// the recording's ISRC, where the service gives it (the music service gives none yet)
    pub isrc: Option<String>,
    pub duration: Option<f64>,
    pub t: f64,
    /// s it sounded (pauses out, a stretch heard twice after a seek back counts twice)
    pub heard: f64,
    /// the furthest point of the track it got to, s
    pub reached: f64,
    /// end | next | previous | restart | other | seek | stop | broken | lost | unknown
    pub ended: Option<String>,
    pub t_end: Option<f64>,
}

/// The listens' writer, one per server.db: made in main, held in `Shared`, given to every player (no server.db — none, and
/// the players play all the same). The rows go to its own thread, in order: SQLite commits with an fsync, never on the
/// runtime's workers, and the player never waits for the disk.
#[derive(Clone)]
pub struct Writer(mpsc::Sender<Row>);

impl Writer {
    pub fn start(store: Arc<Store>) -> Writer {
        let (tx, rx) = mpsc::channel::<Row>();
        std::thread::Builder::new()
            .name("listens".into())
            .spawn(move || {
                let mut failing = false;
                for r in rx {
                    match store.save_listen(&r) {
                        Ok(()) => {
                            failing = false;
                            if r.ended.is_some() {
                                events::emit("listen", json!({"device": r.device}));
                            }
                        }
                        Err(e) if !failing => {
                            failing = true;
                            log(&r.device, format!("music: a listen is not written down: {e}"));
                        }
                        Err(_) => {}
                    }
                }
            })
            .expect("the listens' writer thread");
        Writer(tx)
    }

    /// A listen begun, changed or ended.
    pub fn save(&self, r: Row) {
        let _ = self.0.send(r);
    }
}

/// A new listen's id: unique across restarts (the time it was first asked for, µs, and on).
pub fn next_id() -> i64 {
    static N: OnceLock<AtomicI64> = OnceLock::new();
    N.get_or_init(|| AtomicI64::new((now_s() * 1e6) as i64)).fetch_add(1, Ordering::SeqCst)
}

/// The track id's service: «ytm:…» → ytm.
pub fn provider(track: &str) -> &str {
    track.split_once(':').map(|(p, _)| p).unwrap_or("")
}

/// The artists of a track as a service names them: «BOOKER, МУККА и pyrokinesis», «DJ Sava feat. Irina Rimes»
/// (YouTube Music joins them with «, » and « и »; «Miyagi & Andy Panda», «Soap&Skin» are one name each).
pub fn artists(s: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(?i),\s+|\s+и\s+|\s+(?:feat\.?|ft\.|featuring)\s+").unwrap());
    re.split(s).map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect()
}

fn plain(s: &str) -> String {
    let s = s.to_lowercase().replace('ё', "е");
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

/// One song across services and uploads: the first artist and the title, plainly («(Official Video)», «(Lyrics)»,
/// «feat. …», «Artist - » in front of a video's title left out; «(slowed)», «(Remix)», «(Acoustic)» are other songs).
pub fn song_key(artist: &str, title: &str) -> String {
    static RES: OnceLock<(Regex, Regex)> = OnceLock::new();
    let (noise, feat) = RES.get_or_init(|| {
        (Regex::new(r"(?i)\s*[(\[][^)\]]*\b(official|video|audio|lyrics?|visuali[sz]er|mv|m/v|hd|hq|4k|клип|премьера|текст)\b[^)\]]*[)\]]").unwrap(),
         Regex::new(r"(?i)\s*[(\[]\s*(feat\.?|ft\.|featuring|при уч\.)\s[^)\]]*[)\]]|\s+(feat\.?|ft\.|featuring)\s.*$").unwrap())
    });
    let first = artists(artist).into_iter().next().unwrap_or_default();
    let a = plain(&first);
    let t = feat.replace_all(&noise.replace_all(title, ""), "").to_string();
    let mut t = plain(&t);
    if !a.is_empty() && t.starts_with(&format!("{a} ")) && t.len() > a.len() + 1 {
        t = t[a.len() + 1..].to_string(); // «Isabel LaRosa - I'm Yours (Lyrics)» by Isabel LaRosa
    }
    format!("{a} — {t}")
}

/// A track's listens since a time: title, artist, song, listens, played, finished, skipped, s heard, the last.
type TrackTally = (String, String, String, i64, i64, i64, i64, f64, f64);

/// The verdicts, as SQL over `listens l JOIN tracks k` (0 / 1, never NULL).
fn verdicts() -> &'static (String, String, String) {
    static V: OnceLock<(String, String, String)> = OnceLock::new();
    V.get_or_init(|| {
        let dur = "COALESCE(l.duration, k.duration)";
        let played = format!("COALESCE(l.heard >= {PLAYED} OR ({dur} < {PLAYED} AND l.reached >= {dur} * 0.9), 0)");
        let finished = format!(
            "COALESCE(CASE WHEN {dur} IS NULL THEN l.heard >= {PLAYED} AND l.ended = 'end' \
                                WHEN {dur} < {PLAYED} THEN l.reached >= {dur} * 0.9 \
                                ELSE l.heard >= {PLAYED} AND l.reached >= {dur} - {END_LEFT} END, 0)"
        );
        let skipped = format!("COALESCE(NOT {played} AND l.ended IN ({MOVED_ON}), 0)");
        (played, finished, skipped)
    })
}

#[derive(Default, Clone)]
struct Tally {
    title: String,
    artist: String,
    best: i64, // the most listens of one of the song's tracks: its title and artist are the song's
    listens: i64,
    played: i64,
    finished: i64,
    skipped: i64,
    heard: f64,
    last: f64,
}

fn mins(s: f64) -> f64 {
    (s / 6.0).round() / 10.0
}

impl Store {
    /// A listen kept as it is now (its track too: the first time with the song it is).
    pub fn save_listen(&self, r: &Row) -> Result<(), String> {
        let db = self.db.lock().unwrap();
        let tx = db.unchecked_transaction().map_err(sql)?;
        let known: Option<String> =
            tx.query_row("SELECT song FROM tracks WHERE id = ?", [&r.track], |x| x.get(0)).optional().map_err(sql)?;
        let now = now_s();
        if known.is_none() {
            // the song: that of a track with the same ISRC (the same recording elsewhere), else by its name
            let linked: Option<String> = match &r.isrc {
                Some(i) => tx
                    .query_row("SELECT song FROM tracks WHERE isrc = ? ORDER BY first_t, id LIMIT 1", [i], |x| x.get(0))
                    .optional()
                    .map_err(sql)?,
                None => None,
            };
            let song = linked.unwrap_or_else(|| song_key(&r.artist, &r.title));
            tx.execute("INSERT INTO tracks (id, provider, title, artist, album, duration, isrc, song, first_t, last_t) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                       params![r.track, provider(&r.track), r.title, r.artist, r.album, r.duration, r.isrc, song, now, now]).map_err(sql)?;
        } else {
            tx.execute("UPDATE tracks SET duration = COALESCE(?, duration), album = COALESCE(album, ?), isrc = COALESCE(isrc, ?), last_t = ? WHERE id = ?",
                       params![r.duration, r.album, r.isrc, now, r.track]).map_err(sql)?;
        }
        tx.execute("INSERT INTO listens (id, t, device, track, heard, reached, duration, ended, t_end) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT(id) DO UPDATE SET heard = excluded.heard, reached = excluded.reached,
                    duration = COALESCE(excluded.duration, listens.duration), ended = excluded.ended, t_end = excluded.t_end",
                   params![r.id, r.t, r.device, r.track, (r.heard * 100.0).round() / 100.0, (r.reached * 100.0).round() / 100.0, r.duration,
                           r.ended, r.t_end]).map_err(sql)?;
        tx.commit().map_err(sql)
    }

    /// Every track's song by today's rule, once per `SONG_RULE` (meta `song_rule`): the rule changed — the history
    /// regroups (the table is small: a full pass). A track with an ISRC an earlier track has takes that one's song.
    /// → None: up to date; Some(n): the tracks whose song changed
    pub fn resong(&self) -> Result<Option<usize>, String> {
        let db = self.db.lock().unwrap();
        let at: Option<String> =
            db.query_row("SELECT value FROM meta WHERE key = 'song_rule'", [], |r| r.get(0)).optional().map_err(sql)?;
        if at.as_deref() == Some(SONG_RULE) {
            return Ok(None);
        }
        let tx = db.unchecked_transaction().map_err(sql)?;
        let tracks: Vec<(String, String, String, Option<String>, String)> = {
            let mut st =
                tx.prepare("SELECT id, title, artist, isrc, song FROM tracks ORDER BY first_t, id").map_err(sql)?;
            let rows =
                st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).map_err(sql)?;
            rows.collect::<Result<_, _>>().map_err(sql)?
        };
        let mut by_isrc: HashMap<String, String> = HashMap::new();
        let mut n = 0;
        for (id, title, artist, isrc, was) in tracks {
            let song = isrc.as_ref().and_then(|i| by_isrc.get(i).cloned()).unwrap_or_else(|| song_key(&artist, &title));
            if let Some(i) = isrc {
                by_isrc.entry(i).or_insert_with(|| song.clone());
            }
            if song != was {
                tx.execute("UPDATE tracks SET song = ? WHERE id = ?", params![song, id]).map_err(sql)?;
                n += 1;
            }
        }
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('song_rule', ?1) ON CONFLICT(key) DO UPDATE SET value = ?1",
            [SONG_RULE],
        )
        .map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(Some(n))
    }

    /// Listens left open by a server that stopped (it kept them every 10 s): ended «lost». → how many
    pub fn close_lost_listens(&self) -> Result<usize, String> {
        let db = self.db.lock().unwrap();
        db.execute("UPDATE listens SET ended = 'lost', t_end = t + heard WHERE ended IS NULL", []).map_err(sql)
    }

    /// One listen as kept (tests).
    #[cfg(test)]
    pub fn listen(&self, id: i64) -> Option<Value> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT id, t, device, track, heard, reached, duration, ended FROM listens WHERE id = ?", [id], |r| {
            Ok(json!({"id": r.get::<_, i64>(0)?, "t": r.get::<_, f64>(1)?, "device": r.get::<_, String>(2)?, "track": r.get::<_, String>(3)?,
                      "heard": r.get::<_, f64>(4)?, "reached": r.get::<_, f64>(5)?, "duration": r.get::<_, Option<f64>>(6)?,
                      "ended": r.get::<_, Option<String>>(7)?}))
        }).optional().ok().flatten()
    }

    /// The music since `since`, a device's or everyone's: per day (the home's; a day with none left out), the totals,
    /// the songs (most played first), the most skipped, the artists (most minutes first).
    pub fn music_stats(&self, device: Option<&str>, since: f64) -> Result<Value, String> {
        let (played, finished, skipped) = verdicts();
        let (days, rows) = {
            let db = self.db.lock().unwrap();
            let mut st = db.prepare(&format!(
            "SELECT local_day(l.t) AS day, COUNT(*), SUM(l.heard), SUM({played}), SUM({finished}), SUM({skipped})
             FROM listens l JOIN tracks k ON k.id = l.track WHERE (?1 IS NULL OR l.device = ?1) AND l.t >= ?2 GROUP BY day ORDER BY day")).map_err(sql)?;
            let days: Vec<Value> = st.query_map(params![device, since], |r| {
            Ok(json!({"day": r.get::<_, String>(0)?, "listens": r.get::<_, i64>(1)?, "minutes": mins(r.get::<_, f64>(2)?),
                      "played": r.get::<_, i64>(3)?, "finished": r.get::<_, i64>(4)?, "skipped": r.get::<_, i64>(5)?}))
        }).map_err(sql)?.collect::<Result<_, _>>().map_err(sql)?;
            let mut st = db.prepare(&format!(
            "SELECT k.title, k.artist, k.song, COUNT(*), SUM({played}), SUM({finished}), SUM({skipped}), SUM(l.heard), MAX(l.t)
             FROM listens l JOIN tracks k ON k.id = l.track WHERE (?1 IS NULL OR l.device = ?1) AND l.t >= ?2 GROUP BY l.track")).map_err(sql)?;
            let rows: Vec<TrackTally> = st
                .query_map(params![device, since], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                })
                .map_err(sql)?
                .collect::<Result<_, _>>()
                .map_err(sql)?;
            (days, rows)
        };
        let mut songs: HashMap<String, Tally> = HashMap::new();
        let mut by_artist: HashMap<String, Tally> = HashMap::new();
        for (title, artist, song, n, p, f, s, heard, last) in rows {
            let t = songs.entry(song).or_default();
            if n > t.best {
                (t.best, t.title, t.artist) = (n, title, artist.clone());
            }
            t.listens += n;
            t.played += p;
            t.finished += f;
            t.skipped += s;
            t.heard += heard;
            t.last = t.last.max(last);
            for a in artists(&artist) {
                let x = by_artist.entry(plain(&a)).or_insert_with(|| Tally { title: a.clone(), ..Default::default() });
                x.listens += n;
                x.played += p;
                x.heard += heard;
            }
        }
        let song_json = |t: &Tally| {
            json!({"title": t.title, "artist": t.artist, "listens": t.listens, "played": t.played,
                                            "finished": t.finished, "skipped": t.skipped, "minutes": mins(t.heard), "last": t.last})
        };
        let mut top: Vec<&Tally> = songs.values().filter(|t| t.played > 0).collect();
        top.sort_by(|a, b| b.played.cmp(&a.played).then(b.heard.total_cmp(&a.heard)));
        // skipped once says little (it was on when the listener moved on): twice or more
        let mut skips: Vec<&Tally> = songs.values().filter(|t| t.skipped >= 2).collect();
        skips.sort_by(|a, b| b.skipped.cmp(&a.skipped).then(b.last.total_cmp(&a.last)));
        let mut arts: Vec<&Tally> = by_artist.values().filter(|t| t.played > 0).collect();
        arts.sort_by(|a, b| b.heard.total_cmp(&a.heard));
        let sum = |k: &str| days.iter().map(|d| d[k].as_i64().unwrap_or(0)).sum::<i64>();
        let heard: f64 = songs.values().map(|t| t.heard).sum();
        Ok(json!({
            "days": days,
            "totals": {"minutes": mins(heard), "listens": sum("listens"), "played": sum("played"), "finished": sum("finished"),
                       "skipped": sum("skipped"), "songs": top.len(), "artists": by_artist.values().filter(|t| t.played > 0).count()},
            "songs": top.iter().take(50).map(|t| song_json(t)).collect::<Vec<_>>(),
            "skipped": skips.iter().take(10).map(|t| song_json(t)).collect::<Vec<_>>(),
            "artists": arts.iter().take(30).map(|t| json!({"artist": t.title, "listens": t.listens, "played": t.played, "minutes": mins(t.heard)}))
                .collect::<Vec<_>>(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Arc<Store>, std::path::PathBuf) {
        let d = std::env::temp_dir().join(format!("listens-test-{}-{}", std::process::id(), next_id()));
        std::fs::create_dir_all(&d).unwrap();
        (Store::open(d.join("server.db").to_str().unwrap()).unwrap(), d)
    }

    fn row(id: i64, t: f64, track: &str, dur: Option<f64>, heard: f64, reached: f64, ended: Option<&str>) -> Row {
        Row {
            id,
            device: "kitchen".into(),
            track: track.into(),
            title: format!("Song {track}"),
            artist: "A, B и C".into(),
            album: None,
            isrc: None,
            duration: dur,
            t,
            heard,
            reached,
            ended: ended.map(String::from),
            t_end: None,
        }
    }

    #[test]
    fn songs_and_artists() {
        assert_eq!(artists("BOOKER, МУККА и pyrokinesis"), ["BOOKER", "МУККА", "pyrokinesis"]);
        assert_eq!(artists("DJ Sava feat. Irina Rimes"), ["DJ Sava", "Irina Rimes"]);
        assert_eq!(artists("Miyagi & Andy Panda"), ["Miyagi & Andy Panda"]);
        let k = song_key("Isabel LaRosa", "I'm Yours");
        assert_eq!(k, "isabel larosa — i m yours");
        for (a, t) in [
            ("Isabel LaRosa", "Isabel LaRosa - I'm Yours (Lyrics)"),
            ("Isabel LaRosa, Someone", "I'm Yours (Official Video)"),
            ("Isabel LaRosa feat. X", "I'm Yours [Official Music Video]"),
            ("isabel larosa", "I’m Yours (feat. Jaden)"),
            ("Isabel LaRosa", "I'm Yours ft. Jaden"),
        ] {
            assert_eq!(song_key(a, t), k, "{a} / {t}");
        }
        assert_ne!(song_key("Isabel LaRosa", "I'm Yours (slowed)"), k, "slowed is another recording");
        assert_ne!(song_key("Isabel LaRosa", "I'm Yours (Remix)"), k);
        assert_eq!(song_key("Ёлка", "Прованс (Премьера клипа, 2011)"), song_key("Елка", "Прованс"));
        assert_eq!(provider("ytm:abc"), "ytm");
    }

    #[test]
    fn the_verdicts() {
        let (s, d) = store();
        let t = 1_794_009_600.0 + 9.0 * 3600.0; // a day at noon, Moscow time
        for r in [
            row(1, t, "ytm:a", Some(200.0), 195.0, 200.0, Some("end")), // finished
            row(2, t + 1.0, "ytm:a", Some(200.0), 120.0, 195.0, Some("next")), // within 10 s of the end: finished, not skipped
            row(3, t + 2.0, "ytm:a", Some(200.0), 60.0, 60.0, Some("next")),   // played, not finished
            row(4, t + 3.0, "ytm:b", Some(200.0), 10.0, 10.0, Some("next")),   // skipped
            row(5, t + 4.0, "ytm:b", Some(200.0), 10.0, 10.0, Some("stop")),   // neither
            row(6, t + 5.0, "ytm:b", Some(200.0), 10.0, 10.0, Some("unknown")), // neither (brought in)
            row(7, t + 6.0, "ytm:c", Some(20.0), 20.0, 20.0, Some("end")),     // short, to its end: played and finished
            row(8, t + 7.0, "ytm:c", Some(20.0), 12.0, 12.0, Some("other")),   // short, cut by new music: skipped
            row(9, t + 8.0, "ytm:d", None, 3600.0, 3600.0, None), // a live stream, on now: played, never finished
            row(10, t + 9.0, "ytm:b", Some(200.0), 15.0, 190.0, Some("end")), // seeked to the end: not played, not finished
            row(12, t + 10.0, "ytm:b", Some(200.0), 3.0, 3.0, Some("previous")), // skipped again: twice — in «most skipped»
            row(13, t + 11.0, "ytm:e", Some(200.0), 60.0, 60.0, Some("end")), // a stream that broke, told as its end: played only
            row(14, t + 12.0, "ytm:f", None, 40.0, 40.0, Some("end")),        // no length, ended by itself: finished
        ] {
            s.save_listen(&r).unwrap();
        }
        let v = s.music_stats(None, t - 10.0).unwrap();
        let day = &v["days"][0];
        assert_eq!(
            (day["listens"].as_i64(), day["played"].as_i64(), day["finished"].as_i64(), day["skipped"].as_i64()),
            (Some(13), Some(7), Some(4), Some(3)),
            "{day}"
        );
        assert_eq!(v["totals"]["played"], json!(7));
        let a = v["songs"].as_array().unwrap().iter().find(|x| x["title"] == "Song ytm:a").unwrap();
        assert_eq!(
            (a["listens"].as_i64(), a["played"].as_i64(), a["finished"].as_i64(), a["minutes"].as_f64()),
            (Some(3), Some(3), Some(2), Some(6.3))
        );
        assert_eq!(v["skipped"][0]["title"], "Song ytm:b");
        assert_eq!(v["skipped"].as_array().unwrap().len(), 1, "skipped once (ytm:c) is not in it");
        assert_eq!(v["artists"].as_array().unwrap().len(), 3, "A, B and C each");
        let e = v["songs"].as_array().unwrap().iter().find(|x| x["title"] == "Song ytm:e").unwrap();
        assert_eq!(
            (e["played"].as_i64(), e["finished"].as_i64()),
            (Some(1), Some(0)),
            "broken at 60 of 200: not to its end"
        );
        assert!(s.music_stats(Some("elsewhere"), t - 10.0).unwrap()["days"].as_array().unwrap().is_empty());
        // the same listen told again (every 10 s, then its end): one row
        s.save_listen(&row(9, t + 8.0, "ytm:d", None, 3700.0, 3700.0, Some("stop"))).unwrap();
        assert_eq!(s.listen(9).unwrap()["heard"], json!(3700.0));
        assert_eq!(s.music_stats(None, t - 10.0).unwrap()["days"][0]["listens"], json!(13));
        // left open by a server that stopped
        s.save_listen(&row(11, t + 20.0, "ytm:a", Some(200.0), 40.0, 40.0, None)).unwrap();
        assert_eq!(s.close_lost_listens().unwrap(), 1);
        assert_eq!(s.listen(11).unwrap()["ended"], "lost");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn songs_keyed_again_by_a_new_rule_and_linked_by_isrc() {
        let (s, d) = store();
        let song = |id: &str| -> String {
            s.db.lock().unwrap().query_row("SELECT song FROM tracks WHERE id = ?", [id], |r| r.get(0)).unwrap()
        };
        assert_eq!(s.resong().unwrap(), Some(0), "the first start: the rule noted");
        assert_eq!(s.resong().unwrap(), None, "once per rule");
        let mut t = 100.0;
        let mut put = |id: &str, title: &str, artist: &str, isrc: Option<&str>| {
            let mut r = row(t as i64, t, id, Some(200.0), 40.0, 40.0, Some("end"));
            (r.title, r.artist, r.isrc, r.album) =
                (title.into(), artist.into(), isrc.map(String::from), Some("Album".into()));
            s.save_listen(&r).unwrap();
            t += 1.0;
        };
        put("ytm:a", "I'm Yours (Official Video)", "Isabel LaRosa", None);
        put("spotify:b", "Yours, I Am", "LaRosa", Some("USX"));
        put("yandex:c", "I'm Yours", "Isabel LaRosa", Some("USX")); // its name says a's song, its ISRC b's: the ISRC wins
        put("deezer:d", "I'm Yours", "Isabel LaRosa", Some("USY")); // an ISRC no other track has: by its name
        let (a, b) = (song_key("Isabel LaRosa", "I'm Yours"), song_key("LaRosa", "Yours, I Am"));
        assert_eq!(
            [song("ytm:a"), song("spotify:b"), song("yandex:c"), song("deezer:d")],
            [a.clone(), b.clone(), b.clone(), a.clone()]
        );
        let (album, isrc): (Option<String>, Option<String>) =
            s.db.lock()
                .unwrap()
                .query_row("SELECT album, isrc FROM tracks WHERE id = 'yandex:c'", [], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap();
        assert_eq!((album.as_deref(), isrc.as_deref()), (Some("Album"), Some("USX")));
        // kept by an older rule: the songs as it keyed them; a server with this rule keys them all again, once
        {
            let db = s.db.lock().unwrap();
            db.execute_batch(
                "UPDATE tracks SET song = 'old ' || id; UPDATE meta SET value = '2026-11-01' WHERE key = 'song_rule';",
            )
            .unwrap();
        }
        assert_eq!(s.music_stats(None, 0.0).unwrap()["totals"]["songs"], json!(4), "the old rule: four songs");
        assert_eq!(s.resong().unwrap(), Some(4));
        assert_eq!(
            [song("ytm:a"), song("spotify:b"), song("yandex:c"), song("deezer:d")],
            [a.clone(), b.clone(), b, a],
            "the ISRC link holds"
        );
        assert_eq!(s.music_stats(None, 0.0).unwrap()["totals"]["songs"], json!(2), "regrouped: two songs");
        assert_eq!(s.resong().unwrap(), None);
        std::fs::remove_dir_all(d).unwrap();
    }
}
