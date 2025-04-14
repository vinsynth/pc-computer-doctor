use color_eyre::Result;

pub mod pads;
pub mod active;

pub const PAD_COUNT: usize = 8;
pub const GRAIN_LEN: usize = 1024;
pub const PPQ: u8 = 24;
pub const STEP_DIV: u8 = 4;
pub const LOOP_DIV: u8 = 8;
pub const MAX_PHRASE_LEN: u16 = 2u16.pow(PAD_COUNT as u32 - 1);

pub enum Cmd<const N: usize> {
    Clock,
    Stop,
    AssignTempo(f32),

    AssignSpeed(f32),
    OffsetSpeed(f32),
    AssignDrift(f32),
    AssignBias(f32),
    AssignWidth(f32),

    AssignKit(u8),
    LoadKit(u8),
    SaveScene(std::fs::File),
    LoadScene(Box<pads::Scene<N>>),

    AssignOnset(u8, bool, Box<Onset>),
    ForceSync,
    Input(Event),
    TakeRecord(Option<u8>),
    BakeRecord(u16),
    ClearPool,
    PushPool(u8),
}

#[derive(Copy, Clone, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Rd {
    pub tempo: Option<f32>,
    pub steps: u16,
    pub onsets: Vec<u64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Wav {
    pub tempo: Option<f32>,
    pub steps: u16,
    pub path: Box<std::path::Path>,
    /// pcm length in bytes
    pub len: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Onset {
    pub wav: Wav,
    pub start: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub enum Event {
    Sync,
    Hold { index: u8 },
    Loop { index: u8, len: Fraction },
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Stamped {
    event: Event,
    step: u16,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Phrase {
    events: Vec<Stamped>,
    len: u16,
}

impl Phrase {
    fn generate_active<const N: usize>(&self, active: &mut Option<active::Phrase>, step: u16, bias: f32, drift: f32, pads: &pads::Pads<N>) -> Result<Option<active::Phrase>> {
        if let Some(active) = active.as_mut() {
            if self.events.first().is_some_and(|v| v.step == 0) {
                // phrase events start on first step
                if let Some(event_rem) = self.generate_stamped(&mut active.active, 0, step, bias, drift, pads)? {
                    active.next = 1;
                    active.event_rem = event_rem;
                    active.phrase_rem = self.len;
                }
            } else {
                // phrase events start after first step
                let event_rem = self.events.first().map(|v| v.step).unwrap_or(self.len);
                active.active.trans(&Event::Sync, step, bias, pads)?;
                active.next = 0;
                active.event_rem = event_rem;
                active.phrase_rem = self.len;
            }
        } else if self.events.first().is_some_and(|v| v.step == 0) {
            // phrase events start on first step
            let mut active = active::Event::Sync;
            if let Some(event_rem) = self.generate_stamped(&mut active, 0, step, bias, drift, pads)? {
                return Ok(Some(active::Phrase {
                    next: 1,
                    event_rem,
                    phrase_rem: self.len,
                    active,
                }));
            }
        } else {
            // phrase events start after first step
            let event_rem = self.events.first().map(|v| v.step).unwrap_or(self.len);
            return Ok(Some(active::Phrase {
                next: 0,
                event_rem,
                phrase_rem: self.len,
                active: active::Event::Sync,
            }));
        }
        Ok(None)
    }

    fn generate_stamped<const N: usize>(&self, active: &mut active::Event, index: usize, step: u16, bias: f32, drift: f32, pads: &pads::Pads<N>) -> Result<Option<u16>> {
        let drift = rand::random_range(0..=((drift * self.events.len() as f32 - 1.).round()) as usize);
        let index = (index + drift) % self.events.len();
        let stamped = &self.events[index];
        let event_rem = self.events.get(index + 1).map(|v| v.step).unwrap_or(self.len) - stamped.step;
        active.trans(&stamped.event, step, bias, pads)?;
        Ok(Some(event_rem))
    }
}
