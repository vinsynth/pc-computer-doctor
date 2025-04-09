use super::{pads, Fraction};
use std::{
    collections::VecDeque, fs::File, io::{Seek, SeekFrom}, mem::MaybeUninit
};
use color_eyre::Result;

pub struct Wav {
    pub tempo: Option<f32>,
    pub steps: u16,
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
    Hold(Onset),
    Loop(Onset, Fraction),
}

impl Event {
    pub fn trans<const N: usize>(&mut self, input: &super::Event, bias: f32, pads: &pads::Pads<N>) -> Result<()> {
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
                    *self = Event::Hold(onset);
                } else if let Some(alt) = pads.generate_alt(*index, bias) {
                    let onset = pads.onset_seek(*index, alt, pads::Pads::<N>::generate_pan(*index))?;
                    *self = Event::Hold(onset);
                }
            }
            super::Event::Loop { index, len } => {
                match self {
                    Event::Hold(onset) | Event::Loop(onset, ..) if onset.index == *index => {
                        // recast event variant with same Onset
                        let uninit: &mut MaybeUninit<Onset> = unsafe { std::mem::transmute(onset) };
                        let mut onset = unsafe { std::mem::replace(uninit, MaybeUninit::uninit()).assume_init() };
                        // i don't know either, girl
                        onset.wav.file = onset.wav.file.try_clone()?;
                        *self = Event::Loop(onset, *len);
                    }
                    _ => if let Some(alt) = pads.generate_alt(*index, bias) {
                        let onset = pads.onset(*index, alt, pads::Pads::<N>::generate_pan(*index))?;
                        *self = Event::Loop(onset, *len);
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
    phrase: Option<super::Phrase>,
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
        self.events.retain(|v| step - v.step < super::MAX_PHRASE_LEN);
        self.events.push_back(super::Stamped { event, step });
    }

    pub fn bake(&mut self, step: u16) {
        self.events.retain(|v| step - v.step < super::MAX_PHRASE_LEN);
        self.buffer = self.events.iter_mut().map(|v| {
            super::Stamped {
                event: v.event.clone(),
                step: v.step - step,
            }
        }).collect::<Vec<_>>();
    }

    pub fn trim<const N: usize>(&mut self, len: u16, bias: f32, drift: f32, pads: &pads::Pads<N>) -> Result<()> {
        let events = self.buffer.iter().skip_while(|v| super::MAX_PHRASE_LEN - v.step > len).cloned().collect::<Vec<_>>();
        let phrase = super::Phrase { events, len };
        if phrase.events.first().is_some_and(|v| v.step == 0) {
            // trimmed phrase events start on first step
            if let Some(Phrase { next, event_rem, phrase_rem, active }) = self.active.as_mut() {
                if let Some(rem) = self.phrase.as_ref().unwrap().generate_stamped(active, bias, drift, pads)? {
                    *next = 1;
                    *event_rem = rem;
                    *phrase_rem = len;
                }
            } else {
                let mut active = Event::Sync;
                if let Some(event_rem) = self.phrase.as_ref().unwrap().generate_stamped(&mut active, bias, drift, pads)? {
                    self.active = Some(Phrase {
                        next: 1,
                        event_rem,
                        phrase_rem: len,
                        active,
                    });
                }
            }
        } else {
            // trimmed phrase events start after first step
            let event_rem = phrase.events.first().map(|v| v.step).unwrap_or(len);
            if let Some(active) = self.active.as_mut() {
                active.next = 0;
                active.event_rem = event_rem;
                active.phrase_rem = len;
            } else {
                self.active = Some(Phrase {
                    next: 0,
                    event_rem,
                    phrase_rem: len,
                    active: Event::Sync,
                });
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

    pub fn generate_phrase<const N: usize>(&mut self, bias: f32, drift: f32, pads: &pads::Pads<N>) -> Result<Option<u8>> {
        if !self.phrases.is_empty() {
            let index = {
                // FIXME: use independent phrase_drift instead of same "event_drift"?
                let drift = rand::random_range(0..=((drift * self.phrases.len() as f32 - 1.).round()) as usize);
                let index = self.next + drift % self.phrases.len();
                self.phrases[index]
            };
            if let Some(phrase) = &pads.inner[index as usize].phrase {
                if phrase.events.first().is_some_and(|v| v.step == 0) {
                    // phrase events start on first step
                    if let Some(Phrase { next, event_rem, phrase_rem, active }) = self.active.as_mut() {
                        if let Some(rem) = phrase.generate_stamped(active, bias, drift, pads)? {
                            *next = 1;
                            *event_rem = rem;
                            *phrase_rem = phrase.len;
                        }
                    } else {
                        let mut active = Event::Sync;
                        if let Some(event_rem) = phrase.generate_stamped(&mut active, bias, drift, pads)? {
                            self.active = Some(Phrase {
                                next: 1,
                                event_rem,
                                phrase_rem: phrase.len,
                                active,
                            })
                        }
                    }
                } else {
                    // phrase events start after first step
                    let event_rem = phrase.events.get(index as usize + 1).map(|v| v.step).unwrap_or(phrase.len);
                    if let Some(active) = self.active.as_mut() {
                        active.next = 0;
                        active.event_rem = event_rem;
                        active.phrase_rem = phrase.len;
                    } else {
                        self.active = Some(Phrase {
                            next: 0,
                            event_rem,
                            phrase_rem: phrase.len,
                            active: Event::Sync,
                        });
                    }
                }
            }
            return Ok(Some(index));
        }
        Ok(None)
    }
}
