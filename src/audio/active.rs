use super::{pads, Fraction};
use std::{
    collections::VecDeque, fs::File, io::{Seek, SeekFrom}, mem::MaybeUninit
};
use color_eyre::Result;

pub struct Wav {
    pub tempo: Option<f32>,
    pub steps: Option<u16>,
    pub file: File,
    pub len: u64,
}

impl Wav {
    pub fn pos(&mut self) -> Result<u64, std::io::Error> {
        Ok(self.file.stream_position()? - 44)
    }

    pub fn seek(&mut self, offset: i64) -> Result<(), std::io::Error> {
        self.file.seek(SeekFrom::Start(
            44 + (offset.rem_euclid(self.len as i64) as u64),
        ))?;
        Ok(())
    }
}

pub struct Onset {
    /// source onset index
    pub index: u8,
    pub pan: f32,
    pub wav: Wav,
    pub start: u64,
}

pub enum Event {
    Sync,
    Hold(Onset, u16),
    Loop(Onset, u16, Fraction),
}

impl Event {
    pub fn trans<const N: usize>(&mut self, input: &super::Event, step: u16, bias: f32, pads: &pads::Kit<N>) -> Result<()> {
        match input {
            super::Event::Sync => {
                *self = Event::Sync;
            }
            super::Event::Hold { index } => {
                if let Event::Loop(onset, ..) = self {
                    // recast event variant with same Onset
                    let uninit: &mut MaybeUninit<Onset> = unsafe { std::mem::transmute(onset) };
                    let mut onset = unsafe { std::mem::replace(uninit, MaybeUninit::uninit()).assume_init() };
                    // i don't know either, girl
                    onset.wav.file = onset.wav.file.try_clone()?;
                    *self = Event::Hold(onset, step);
                } else if let Some(alt) = pads.generate_alt(*index, bias) {
                    let onset = pads.onset_seek(*index, alt, pads::Kit::<N>::generate_pan(*index))?;
                    *self = Event::Hold(onset, step);
                }
            }
            super::Event::Loop { index, len } => {
                match self {
                    Event::Hold(onset, step) | Event::Loop(onset, step, ..) if onset.index == *index => {
                        // recast event variant with same Onset
                        let uninit: &mut MaybeUninit<Onset> = unsafe { std::mem::transmute(onset) };
                        let mut onset = unsafe { std::mem::replace(uninit, MaybeUninit::uninit()).assume_init() };
                        // i don't know either, girl
                        onset.wav.file = onset.wav.file.try_clone()?;
                        *self = Event::Loop(onset, *step, *len);
                    }
                    _ => if let Some(alt) = pads.generate_alt(*index, bias) {
                        let onset = pads.onset(*index, alt, pads::Kit::<N>::generate_pan(*index))?;
                        *self = Event::Loop(onset, step, *len);
                    }
                }
            }
        }
        Ok(())
    }
}

pub struct Input {
    pub active: Event,
    pub buffer: Option<super::Event>,
}

impl Input {
    pub fn new() -> Self {
        Self { active: Event::Sync, buffer: None }
    }
}

pub struct Phrase {
    /// next event index (sans drift)
    pub next: usize,
    /// remaining steps in event
    pub event_rem: u16,
    /// remaining steps in phrase
    pub phrase_rem: u16,
    /// active event (last consumed)
    pub active: Event,
}

pub struct Record {
    /// running bounded event queue
    events: VecDeque<super::Stamped>,
    /// baked events
    buffer: Vec<super::Stamped>,
    /// trimmed source phrase, if any
    pub phrase: Option<super::Phrase>,
    /// active phrase, if any
    pub active: Option<Phrase>,
}

impl Record {
    pub fn new() -> Self {
        Self {
            events: VecDeque::new(),
            buffer: Vec::new(),
            phrase: None,
            active: None,
        }
    }

    pub fn push(&mut self, event: super::Event, step: u16) {
        // remove steps beyond max phrase len
        while self.events.front().is_some_and(|v| step - v.step > super::MAX_PHRASE_LEN) {
            self.events.pop_front();
        }
        self.events.push_back(super::Stamped { event, step });
    }

    pub fn bake(&mut self, step: u16) {
        self.buffer = self.events.iter().flat_map(|v| Some(super::Stamped {
            event: v.event.clone(),
            step: (v.step + super::MAX_PHRASE_LEN).checked_sub(step)?,
        })).collect::<Vec<_>>();
    }

    pub fn trim(&mut self, len: u16) {
        let events = self.buffer.iter().flat_map(|v| Some(super::Stamped {
            event: v.event.clone(),
            step: (v.step + len).checked_sub(super::MAX_PHRASE_LEN)?,
        })).collect::<Vec<_>>();
        self.phrase = Some(super::Phrase { events, len });
    }

    pub fn generate_phrase<const N: usize>(&mut self, step: u16, bias: f32, drift: f32, pads: &pads::Kit<N>) -> Result<()> {
        if let Some(phrase) = self.phrase.as_mut() {
            if let Some(phrase) = phrase.generate_active(&mut self.active, step, bias, drift, pads)? {
                self.active = Some(phrase);
            }
        }
        Ok(())
    }

    pub fn take(&mut self) -> Option<(super::Phrase, Phrase)> {
        self.buffer.clear();
        Some((self.phrase.take()?, self.active.take()?))
    }
}

pub struct Pool {
    /// next phrase index (sans drift)
    pub next: usize,
    /// base phrase sequence
    pub phrases: Vec<u8>,
    /// pad index of source phrase, if any
    pub index: Option<u8>,
    /// active phrase, if any
    pub active: Option<Phrase>,
}

impl Pool {
    pub fn new() -> Self {
        Self {
            next: 0,
            phrases: Vec::new(),
            index: None,
            active: None,
        }
    }

    pub fn generate_phrase<const N: usize>(&mut self, step: u16, bias: f32, drift: f32, pads: &pads::Kit<N>) -> Result<()> {
        if self.phrases.is_empty() {
            self.next = 0;
            self.active = None;
        } else {
            let index = {
                // FIXME: use independent phrase_drift instead of same "stamped_drift"?
                let drift = rand::random_range(0..=((drift * self.phrases.len() as f32 - 1.).round()) as usize);
                let index = (self.next + drift) % self.phrases.len();
                self.phrases[index]
            };
            self.index = Some(index);
            if let Some(phrase) = &pads.inner[index as usize].phrase {
                if let Some(phrase) = phrase.generate_active(&mut self.active, step, bias, drift, pads)? {
                    self.active = Some(phrase);
                }
            }
            self.next = (self.next + 1) % self.phrases.len();
        }
        Ok(())
    }
}
