pub mod pads;
pub mod active;
use crate::input::Global;

use color_eyre::Result;

pub const PAD_COUNT: usize = 8;
pub const GRAIN_LEN: usize = 1024;
pub const PPQ: u8 = 24;
pub const STEP_DIV: u8 = 4;
pub const LOOP_DIV: u8 = 8;

pub enum Cmd {
    Start,
    Clock,
    Input(Event),
    ClearRecord,
    Record(bool),
    AssignOnset(u8, bool, Onset),
    AssignPhrase(u8),
    ClearGhost,
    PushGhost(u8),
    ClearSequence,
    PushSequence(u8),
    AssignGlobal(Global),
}

#[derive(Copy, Clone)]
pub struct Fraction {
    numerator: u8,
    denominator: u8,
}

impl Fraction {
    pub fn new(numerator: u8, denominator: u8) -> Self {
        Self {
            numerator,
            denominator,
        }
    }
}

impl From<Fraction> for f32 {
    fn from(value: Fraction) -> Self {
        value.numerator as f32 / value.denominator as f32
    }
}

#[derive(Clone, miniserde::Deserialize)]
pub struct Rd {
    pub tempo: f32,
    pub steps: u16,
    pub onsets: Vec<u64>,
}

pub struct Wav {
    pub rd: Rd,
    pub path: Box<std::path::Path>,
    /// pcm length in bytes
    pub len: u64,
}

pub struct Onset {
    pub wav: Wav,
    pub start: u64,
    pub steps: u16,
}

pub enum Event {
    Sync,
    Hold { index: u8 },
    Loop { index: u8, len: Fraction },
}

struct Stamped {
    event: Event,
    steps: u16,
}

#[derive(Default)]
pub struct Phrase {
    offset: u16,
    events: Vec<Stamped>,
}

impl Phrase {
    fn is_empty(&self) -> bool {
        self.offset == 0 && self.events.is_empty()
    }
}
