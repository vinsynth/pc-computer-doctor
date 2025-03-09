use super::Fraction;
use std::{
    fs::File,
    io::{Seek, SeekFrom},
};

pub struct Wav {
    pub tempo: f32,
    pub steps: usize,
    pub file: File,
    pub len: u64,
}

impl Wav {
    pub fn open(wav: &super::Wav) -> Result<Self, std::io::Error> {
        Ok(Self {
            tempo: wav.rd.tempo,
            steps: wav.rd.steps,
            file: File::open(wav.path.clone())?,
            len: wav.len,
        })
    }

    pub fn pos(&mut self) -> Result<u64, std::io::Error> {
        Ok(self.file.stream_position()? - 44)
    }

    pub fn seek(&mut self, offset: i64) -> Result<(), std::io::Error> {
        self.file.seek(SeekFrom::Start(
            44 + (offset.rem_euclid(self.len as i64) as u64),
        ))?;
        Ok(())
    }

    pub fn seek_quantized(&mut self, offset: i64, step: f32) -> Result<(), std::io::Error> {
        let shift = ((step - 0.5) / self.steps as f32 * self.len as f32) as i64 & !1;
        self.seek(offset + shift)
    }
}

pub struct Onset {
    /// source onset index
    pub index: u8,
    pub pan: f32,
    pub wav: Wav,
    pub start: u64,
}

pub enum Input {
    Sync,
    Hold(Onset),
    Loop(Onset, Fraction),
}

pub enum State {
    Input(Input),
    Ghost(Input, f32),
    Phrase,
}

impl Default for State {
    fn default() -> Self {
        State::Input(Input::Sync)
    }
}

pub struct Phrase {
    /// source phrase index
    pub index: u8,
    /// next event index
    pub next: usize,
    /// remaining step duration
    pub rem: f32,
    /// active input, in any
    pub input: Option<Input>,
}

#[derive(Default)]
pub struct Record {
    pub stopped: bool,
    pub phrase: super::Phrase,
}
