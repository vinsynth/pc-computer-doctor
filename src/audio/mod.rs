use color_eyre::Result;

pub mod pads;
pub mod active;

pub const PAD_COUNT: usize = 8;
pub const GRAIN_LEN: usize = 1024;
pub const PPQ: u8 = 24;
pub const STEP_DIV: u8 = 4;
pub const LOOP_DIV: u8 = 8;
pub const MAX_PHRASE_LEN: u16 = 2u16.pow(PAD_COUNT as u32 - 1) - 1;

pub enum Cmd {
    Start,
    Clock,
    Stop,
    AssignTempo(f32),
    AssignBias(f32),
    AssignDrift(f32),
    AssignSpeed(f32),
    OffsetSpeed(f32),
    AssignWidth(f32),
    AssignOnset(u8, bool, Onset),
    Input(Event),
    BakeRecord(u16),
    TakeRecord(Option<u8>),
    PushPool(u8),
    ClearPool,
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
    pub tempo: Option<f32>,
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

#[derive(Clone)]
pub enum Event {
    Sync,
    Hold { index: u8 },
    Loop { index: u8, len: Fraction },
}

#[derive(Clone)]
struct Stamped {
    event: Event,
    step: u16,
}

pub struct Phrase {
    events: Vec<Stamped>,
    len: u16,
}

impl Phrase {
    fn generate_stamped<const N: usize>(&self, active: &mut active::Event, bias: f32, drift: f32, pads: &pads::Pads<N>) -> Result<Option<u16>> {
        let drift = rand::random_range(0..=((drift * self.events.len() as f32 - 1.).round()) as usize);
        let index = drift % self.events.len();
        let Stamped { event, step } = &self.events[index];
        let event_rem = self.events.get(index + 1).map(|v| v.step).unwrap_or(self.len) - step;
        active.trans(event, bias, pads)?;
        Ok(Some(event_rem))
    }
}
