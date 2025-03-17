use super::Fraction;
use std::{
    fs::File,
    io::{Seek, SeekFrom},
};

pub struct Wav {
    pub tempo: f32,
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

pub enum State {
    Input(Event),
    Ghost(Event, u16),
    Phrase,
}

impl Default for State {
    fn default() -> Self {
        State::Input(Event::Sync)
    }
}

pub struct Phrase {
    /// source phrase index
    pub index: u8,
    /// next event index
    pub next: usize,
    /// remaining steps in event
    pub event_rem: u16,
    /// remaining steps in phrase
    pub phrase_rem: u16,
    /// active event, if any
    pub event: Option<Event>,
}

#[derive(Default)]
pub struct Record {
    pub stopped: bool,
    pub phrase: super::Phrase,
}
