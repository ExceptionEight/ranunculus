//! Listening to the command after the wake word: Silero VAD on the signal boosted by 35 dB (the station's processed
//! microphone gives speech at about −60 dBFS), threshold 0.5, the end after 700 ms of silence; nothing said in 3 s —
//! silence; 10 s at most. The time counts the audio after the pre-roll (the station's 0.4 s before
//! the word ended), so network jitter does not move the limits. The pre-roll goes to the recognizer only: it holds the
//! word's own tail, and in the VAD it would be «speech started» at once — then a breath's pause ends the command.
use ranunculus_vad::{Event, Silero, VadIterator, CHUNK};

pub const GAIN: f32 = 56.234_13; // 10^(35/20)
const NO_SPEECH_S: f64 = 3.0;
const MAX_S: f64 = 10.0;

pub enum Outcome {
    /// What was said (with the pre-roll): for the speech recognizer
    Command(Vec<f32>),
    /// Nothing was said
    Silence,
}

pub struct Listener {
    vad: Silero,
    it: VadIterator,
    cmd: Vec<f32>,
    pending: Vec<f32>, // boosted samples waiting for a 512 window
    started: bool,
    preroll: Option<usize>, // samples of the pre-roll (the first chunk after the wake)
    heard: usize,           // samples after the pre-roll
}

impl Listener {
    pub fn new(mut vad: Silero) -> Listener {
        vad.reset();
        Listener {
            vad,
            it: VadIterator::new(0.5, 700, 100),
            cmd: vec![],
            pending: vec![],
            started: false,
            preroll: None,
            heard: 0,
        }
    }

    pub fn into_vad(self) -> Silero {
        self.vad
    }

    /// Int16 samples of the stream; Some once the command is over.
    pub fn push(&mut self, pcm: &[i16]) -> Option<Outcome> {
        let x: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        self.cmd.extend_from_slice(&x);
        if self.preroll.is_none() {
            self.preroll = Some(x.len()); // the first chunk after the wake: the recognizer's, not the VAD's
            return None;
        }
        self.heard += x.len();
        self.pending.extend(x.iter().map(|v| (v * GAIN).clamp(-1.0, 1.0)));
        let mut ended = false;
        while self.pending.len() >= CHUNK {
            let w: Vec<f32> = self.pending.drain(..CHUNK).collect();
            match self.it.push(self.vad.prob(&w)) {
                Some(Event::Start) => self.started = true,
                Some(Event::End) if self.started => ended = true,
                _ => {}
            }
        }
        let el = self.heard as f64 / 16000.0;
        if !(ended || (!self.started && el > NO_SPEECH_S) || el > MAX_S) {
            return None;
        }
        Some(if self.started { Outcome::Command(std::mem::take(&mut self.cmd)) } else { Outcome::Silence })
    }
}
