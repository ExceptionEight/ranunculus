//! Ducking: the music goes quiet under the listening, an answer and a ring, each of which holds its own reason. One
//! flag for all would let whichever ends first bring the music back up under the others: an answer's end under a ring
//! (it waits for a dialog only so long), a stopped ring's tail under the next ring.
use super::{Music, DUCK, GAIN};
use std::sync::Arc;

impl Music {
    /// The music goes quiet for `why` till the guard is dropped, and comes back up when no reason is left.
    pub fn duck(self: &Arc<Self>, why: Why) -> Ducked {
        self.ducks.lock().unwrap()[why as usize] += 1;
        Ducked { music: self.clone(), why }
    }

    /// Ducked now, for whatever reason.
    pub fn ducked(&self) -> bool {
        self.ducks.lock().unwrap().iter().any(|&n| n > 0)
    }

    /// The level the stream glides to.
    pub(super) fn target(&self) -> f32 {
        if self.ducked() {
            DUCK
        } else {
            GAIN
        }
    }
}

/// Why the music is ducked: the listening (the wake to the command's end), the answer (to its end), a ring.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Why {
    Listen = 0,
    Answer = 1,
    Ring = 2,
}

/// One reason the music is ducked, while it lives.
pub struct Ducked {
    music: Arc<Music>,
    why: Why,
}

impl Ducked {
    /// The same music ducked for `why` now: the new reason is taken before this one goes — no gap to rise in.
    pub fn then(self, why: Why) -> Ducked {
        self.music.duck(why)
    }
}

impl Drop for Ducked {
    fn drop(&mut self) {
        let mut d = self.music.ducks.lock().unwrap();
        d[self.why as usize] = d[self.why as usize].saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_only_when_no_reason_is_left() {
        let m = Music::new("http://127.0.0.1:9", "d", None);
        assert_eq!(m.target(), GAIN);
        // an answer ends while a ring sounds (it waited WAIT s and began over a long answer): the music stays down
        let answer = m.duck(Why::Listen).then(Why::Answer);
        let ring = m.duck(Why::Ring);
        drop(answer);
        assert!(m.ducked() && m.target() == DUCK, "the ring still sounds");
        drop(ring);
        assert_eq!(m.target(), GAIN, "nothing left: up");
        // a ring ends (by itself, a touch, the web) during an answer: down till the answer ends
        let ring = m.duck(Why::Ring);
        let answer = m.duck(Why::Answer);
        drop(ring);
        assert_eq!(m.target(), DUCK, "the answer goes on");
        drop(answer);
        assert!(!m.ducked());
        // a stopped ring's tail ending under the next ring
        let (a, b) = (m.duck(Why::Ring), m.duck(Why::Ring));
        drop(a);
        assert!(m.ducked(), "the next ring");
        drop(b);
        // listening handed over to the answer: never up in between
        let l = m.duck(Why::Listen);
        let a = l.then(Why::Answer);
        assert!(m.ducked());
        drop(a);
        assert!(!m.ducked());
    }
}
