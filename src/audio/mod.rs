mod active;
use crate::{input::Global, tui};

use std::{io::Read, mem::MaybeUninit};

use color_eyre::Result;
use ringbuf::{traits::Producer, CachingProd, HeapRb};

pub const SAMPLE_RATE: u64 = 48000;
pub const GRAIN_LEN: usize = 1024;
pub const PAD_COUNT: usize = 8;
pub const STEP_DIVISOR: u8 = 8;

/// audio command
pub enum Cmd {
    Input(Input),
    Record(bool),
    AssignOnset(u8, bool, Onset),
    AssignPhrase(u8),
    ClearGhost,
    PushGhost(u8),
    ClearSequence,
    PushSequence(u8),
    AssignGlobal(Global),
}

/// self explanatory
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
    pub steps: usize,
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
    pub steps: usize,
}

pub enum Input {
    Sync,
    Hold { index: u8 },
    Loop { index: u8, len: Fraction },
}

struct Event {
    input: Input,
    steps: f32,
}

#[derive(Default)]
struct Phrase {
    offset: f32,
    events: Vec<Event>,
}

impl Phrase {
    fn is_empty(&self) -> bool {
        self.offset == 0. && self.events.is_empty()
    }
}

#[derive(Default)]
struct Pad {
    onsets: [Option<Onset>; 2],
    phrase: Phrase,
    onset_weight: u8,
    phrase_weight: u8,
}

pub struct Pads {
    step: f32,
    state: active::State,
    phrase: Option<active::Phrase>,
    record: Option<active::Record>,
    pads: [Pad; PAD_COUNT],
    bias: f32,
    roll: f32,
    drift: f32,
    width: f32,
    tempo: f32,
    tui_tx: std::sync::mpsc::Sender<tui::Cmd>,
}

impl Pads {
    fn delta(tempo: f32) -> f32 {
        tempo / 60. / SAMPLE_RATE as f32 * GRAIN_LEN as f32
    }

    fn read_grain(
        tempo: f32,
        width: f32,
        onset: &mut active::Onset,
        producer: &mut CachingProd<std::sync::Arc<HeapRb<[f32; 2]>>>,
    ) -> Result<()> {
        // TODO: tempo sync via resampling OR jump on step boundary
        let rem = (GRAIN_LEN as f32 * 2. * tempo / onset.wav.tempo) as usize & !1;
        let mut read = vec![0u8; rem + 2];
        let mut slice = &mut read[..];
        let wav = &mut onset.wav;
        // read grain
        while !slice.is_empty() {
            let n = wav.file.read(slice)?;
            if n == 0 {
                wav.seek(0)?;
            }
            slice = &mut slice[n..];
        }
        // resync form reading extra word for interpolation
        let pos = wav.pos()?;
        wav.seek(pos as i64 - 2)?;
        // resample via linear interpolation
        for i in 0..GRAIN_LEN {
            let read_idx = i as f32 * tempo / onset.wav.tempo;
            let mut i16_buffer = [0u8; 2];

            i16_buffer.copy_from_slice(&read[read_idx as usize * 2..][0..2]);
            let word_a = i16::from_le_bytes(i16_buffer) as f32 / i16::MAX as f32 * read_idx.fract();
            i16_buffer.copy_from_slice(&read[read_idx as usize * 2..][2..4]);
            let word_b =
                i16::from_le_bytes(i16_buffer) as f32 / i16::MAX as f32 * (1. - read_idx.fract());

            let sample = word_a + word_b;
            let l = sample * (1. + width * ((onset.pan - 0.5).abs() - 1.));
            let r = sample * (1. + width * ((onset.pan + 0.5).abs() - 1.));
            while producer.try_push([l, r]).is_err() {}
        }
        Ok(())
    }

    fn handle_active_input(
        tempo: f32,
        width: f32,
        input: &mut active::Input,
        producer: &mut CachingProd<std::sync::Arc<HeapRb<[f32; 2]>>>,
    ) -> Result<()> {
        match input {
            active::Input::Sync => {
                for _ in 0..GRAIN_LEN {
                    while producer.try_push([0., 0.]).is_err() {}
                }
                Ok(())
            }
            active::Input::Hold(onset) => Pads::read_grain(tempo, width, onset, producer),
            active::Input::Loop(onset, len) => {
                let wav = &mut onset.wav;
                let pos = wav.pos()?;
                let end =
                    onset.start + (f32::from(*len) * wav.len as f32 / wav.steps as f32) as u64;
                // TODO: quantization? how?
                if end < pos || pos < onset.start && end < pos + wav.len {
                    wav.seek(onset.start as i64)?;
                }
                Pads::read_grain(tempo, width, onset, producer)
            }
        }
    }

    pub fn new(tui_tx: std::sync::mpsc::Sender<tui::Cmd>) -> Self {
        Pads {
            // default 172 bpm in sixteenths
            tempo: 688.,
            step: 0.5,
            state: active::State::Input(active::Input::Sync),
            phrase: None,
            record: None,
            tui_tx,
            pads: core::array::from_fn(|_| Pad::default()),
            bias: 0.,
            roll: 0.,
            drift: 0.,
            width: 0.,
        }
    }

    pub fn run(
        &mut self,
        cmd_rx: std::sync::mpsc::Receiver<Cmd>,
        mut producer: CachingProd<std::sync::Arc<HeapRb<[f32; 2]>>>,
    ) -> Result<()> {
        loop {
            match cmd_rx.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return Ok(()),
                Ok(cmd) => match cmd {
                    Cmd::Input(input) => self.cmd_input(input)?,
                    Cmd::Record(recording) => self.cmd_record(recording),
                    Cmd::AssignOnset(index, alt, onset) => {
                        self.cmd_assign_onset(index, alt, onset)?
                    }
                    Cmd::AssignPhrase(index) => self.cmd_assign_phrase(index),
                    Cmd::ClearGhost => self.cmd_clear_ghost(),
                    Cmd::PushGhost(index) => self.cmd_push_ghost(index),
                    Cmd::ClearSequence => self.cmd_clear_sequence(),
                    Cmd::PushSequence(index) => self.cmd_push_sequence(index)?,
                    Cmd::AssignGlobal(global) => self.cmd_global(global),
                },
                _ => (),
            }
            self.tick(&mut producer)?;
        }
    }

    fn tick(&mut self, producer: &mut CachingProd<std::sync::Arc<HeapRb<[f32; 2]>>>) -> Result<()> {
        self.handle_state(producer)?;
        self.advance_steps()?;
        Ok(())
    }

    fn handle_state(
        &mut self,
        producer: &mut CachingProd<std::sync::Arc<HeapRb<[f32; 2]>>>,
    ) -> Result<()> {
        match &mut self.state {
            active::State::Input(input) => {
                Pads::handle_active_input(self.tempo, self.width, input, producer)?;
            }
            active::State::Ghost(input, rem) => {
                Pads::handle_active_input(self.tempo, self.width, input, producer)?;
                *rem -= Pads::delta(self.tempo);
                if *rem <= 0. {
                    if let Some((input, rem)) = self.generate_ghost()? {
                        self.state = active::State::Ghost(input, rem);
                    } else {
                        self.state = active::State::Input(active::Input::Sync);
                    }
                }
            }
            active::State::Phrase => {
                if let Some(active::Phrase { input, .. }) = self.phrase.as_mut() {
                    Pads::handle_active_input(
                        self.tempo,
                        self.width,
                        input.as_mut().unwrap_or(&mut active::Input::Sync),
                        producer,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// advance all active steps (excluding ghost; that's handled in handle_state())
    fn advance_steps(&mut self) -> Result<()> {
        // advance phrase step, if any
        if let Some(active::Phrase {
            index, next, rem, ..
        }) = self.phrase.as_mut()
        {
            *rem -= Pads::delta(self.tempo);
            if *rem <= 0. {
                if *next < self.pads[*index as usize].phrase.events.len() {
                    // generate next event
                    let index = *index;
                    let next = *next + 1;
                    let rem = *rem;
                    // FIXME: overall pattern length should be consistent, but rn lengths of random event matches source
                    // to keep overall length same, random should take length from original events[index]
                    // or treat phrase.events as urn rather than pool
                    // or better yet log cumulative pattern length and end pattern once length reached, rather than when events exhausted
                    let (input, rem) = self.generate_event(index, next, rem, self.drift)?;
                    self.phrase = Some(active::Phrase {
                        index,
                        next,
                        rem,
                        input: Some(input),
                    });
                } else {
                    // generate phrase
                    let phrase = self.generate_phrase()?;
                    self.phrase = phrase;
                }
                match &self.state {
                    active::State::Input(input) if !matches!(input, active::Input::Sync) => (),
                    active::State::Phrase => {
                        if let Some((input, rem)) = self.generate_ghost()? {
                            self.state = active::State::Ghost(input, rem);
                        } else {
                            self.state = active::State::Input(active::Input::Sync);
                        }
                    }
                    _ => {
                        if matches!(&self.phrase, Some(active::Phrase { input: Some(input), .. }) if !matches!(input, active::Input::Sync))
                        {
                            self.state = active::State::Phrase;
                        }
                    }
                }
            }
        }
        // advance record step, if any
        if let Some(active::Record {
            stopped: false,
            phrase: Phrase { offset, events },
        }) = self.record.as_mut()
        {
            if let Some(Event { steps, .. }) = events.last_mut() {
                *steps += Pads::delta(self.tempo);
            } else {
                *offset += Pads::delta(self.tempo);
            }
        }
        // advance global qauntize step
        let prev = self.step;
        self.step = (self.step + Pads::delta(self.tempo)).fract();
        if self.step.round() > prev.round() {
            self.tui_tx.send(tui::Cmd::Clock)?;
        }
        Ok(())
    }

    fn generate_alt(&self, index: u8) -> Option<bool> {
        match self.pads[index as usize].onsets {
            [None, None] => None,
            [Some(_), None] => Some(false),
            [None, Some(_)] => Some(true),
            [Some(_), Some(_)] => Some(rand::random_bool(self.bias as f64)),
        }
    }

    fn generate_pan(&self, index: u8) -> f32 {
        index as f32 / 8. - 0.5
    }

    fn generate_ghost(&mut self) -> Result<Option<(active::Input, f32)>> {
        let mut weights: [u8; PAD_COUNT] = core::array::from_fn(|i| self.pads[i].onset_weight);
        let sum = weights.iter().sum::<u8>();
        if sum == 0 {
            return Ok(None);
        }
        let mut offset = rand::random_range(1..=sum);
        let mut index = 0;
        while offset > 0 {
            while weights[index] == 0 {
                index += 1;
            }
            weights[index] -= 1;
            offset -= 1;
        }
        let (input, rem) = if let Some(alt) = self.generate_alt(index as u8) {
            let Onset { wav, start, steps } =
                self.pads[index].onsets[alt as usize].as_ref().unwrap();
            let mut wav = active::Wav::open(wav)?;
            wav.seek_quantized(*start as i64, self.step)?;
            let onset = active::Onset {
                index: index as u8,
                pan: self.generate_pan(index as u8),
                wav,
                start: *start,
            };
            if rand::random_bool(self.roll as f64) {
                let steps = rand::random_range(0..STEP_DIVISOR as usize) as f32;
                let len = Fraction::new(rand::random_range(0..STEP_DIVISOR), STEP_DIVISOR);
                (active::Input::Loop(onset, len), steps)
            } else {
                (active::Input::Hold(onset), *steps as f32)
            }
        } else {
            let steps = rand::random_range(0..STEP_DIVISOR as usize) as f32;
            (active::Input::Sync, steps)
        };
        Ok(Some((input, rem)))
    }

    fn generate_event(
        &mut self,
        index: u8,
        next: usize,
        offset: f32,
        drift: f32,
    ) -> Result<(active::Input, f32)> {
        let Phrase { events, .. } = &self.pads[index as usize].phrase;
        let drift = rand::random_range(0..=((drift * events.len() as f32 - 1.).round()) as usize);
        let index = (next + drift) % events.len();
        let Event { input, steps } = &events[index];
        let input = match input {
            Input::Sync => active::Input::Sync,
            Input::Hold { index } => {
                if let Some(alt) = self.generate_alt(*index) {
                    let Onset { wav, start, .. } = self.pads[*index as usize].onsets[alt as usize]
                        .as_ref()
                        .unwrap();
                    let mut wav = active::Wav::open(wav)?;
                    wav.seek_quantized(*start as i64, self.step)?;
                    let onset = active::Onset {
                        index: *index,
                        pan: self.generate_pan(*index),
                        wav,
                        start: *start,
                    };
                    active::Input::Hold(onset)
                } else {
                    active::Input::Sync
                }
            }
            Input::Loop { index, len } => {
                if let Some(alt) = self.generate_alt(*index) {
                    let Onset { wav, start, .. } = self.pads[*index as usize].onsets[alt as usize]
                        .as_ref()
                        .unwrap();
                    let wav = active::Wav::open(wav)?;
                    let onset = active::Onset {
                        index: *index,
                        pan: self.generate_pan(*index),
                        wav,
                        start: *start,
                    };
                    active::Input::Loop(onset, *len)
                } else {
                    active::Input::Sync
                }
            }
        };
        Ok((input, *steps + offset))
    }

    fn generate_phrase(&mut self) -> Result<Option<active::Phrase>> {
        let mut weights: [u8; PAD_COUNT] = core::array::from_fn(|i| {
            self.pads[i].phrase_weight * !self.pads[i].phrase.is_empty() as u8
        });
        let sum = weights.iter().sum::<u8>();
        if sum == 0 || self.pads.iter().all(|v| v.phrase.is_empty()) {
            return Ok(None);
        }
        let mut offset = rand::random_range(1..=sum);
        let mut index = 0;
        while offset > 0 {
            while weights[index] == 0 || self.pads[index].phrase.is_empty() {
                index += 1;
            }
            weights[index] -= 1;
            offset -= 1;
        }
        let phrase = &self.pads[index].phrase;
        if phrase.offset != 0. {
            Ok(Some(active::Phrase {
                index: index as u8,
                next: 0,
                rem: phrase.offset,
                input: None,
            }))
        } else {
            let (input, rem) = self.generate_event(index as u8, 0, self.step - 0.5, 0.)?;
            Ok(Some(active::Phrase {
                index: index as u8,
                next: 1,
                rem,
                input: Some(input),
            }))
        }
    }

    fn cmd_input(&mut self, input: Input) -> Result<()> {
        match input {
            Input::Sync => {
                if let active::State::Input(active::Input::Loop(onset, ..)) = &mut self.state {
                    // downcast input variant with same active::Onset
                    let uninit: &mut MaybeUninit<active::Onset> =
                        unsafe { std::mem::transmute(onset) };
                    let mut onset =
                        unsafe { std::mem::replace(uninit, MaybeUninit::uninit()).assume_init() };
                    // i don't know either, girl
                    onset.wav.file = onset.wav.file.try_clone()?;
                    self.state = active::State::Input(active::Input::Hold(onset));
                } else if matches!(&self.phrase, Some(active::Phrase { input: Some(input), .. }) if !matches!(input, active::Input::Sync))
                {
                    self.state = active::State::Phrase;
                } else if let Some((input, rem)) = self.generate_ghost()? {
                    self.state = active::State::Ghost(input, rem);
                } else {
                    self.state = active::State::Input(active::Input::Sync);
                }
            }
            Input::Hold { index } => {
                if let Some(alt) = self.generate_alt(index) {
                    let Onset { wav, start, .. } = self.pads[index as usize].onsets[alt as usize]
                        .as_ref()
                        .unwrap();
                    let mut wav = active::Wav::open(wav)?;
                    wav.seek_quantized(*start as i64, self.step)?;
                    let onset = active::Onset {
                        index,
                        pan: self.generate_pan(index),
                        wav,
                        start: *start,
                    };
                    self.state = active::State::Input(active::Input::Hold(onset));
                }
            }
            Input::Loop { index, len } => {
                match &mut self.state {
                    active::State::Input(
                        active::Input::Hold(onset) | active::Input::Loop(onset, ..),
                    ) if onset.index == index => {
                        // recast input variant with same active::Onset
                        let uninit: &mut MaybeUninit<active::Onset> =
                            unsafe { std::mem::transmute(onset) };
                        let mut onset = unsafe {
                            std::mem::replace(uninit, MaybeUninit::uninit()).assume_init()
                        };
                        // i don't know either, girl
                        onset.wav.file = onset.wav.file.try_clone()?;
                        self.state = active::State::Input(active::Input::Loop(onset, len));
                    }
                    _ => {
                        if let Some(alt) = self.generate_alt(index) {
                            let Onset { wav, start, .. } = self.pads[index as usize].onsets
                                [alt as usize]
                                .as_ref()
                                .unwrap();
                            let wav = active::Wav::open(wav)?;
                            let onset = active::Onset {
                                index,
                                pan: self.generate_pan(index),
                                wav,
                                start: *start,
                            };
                            self.state = active::State::Input(active::Input::Loop(onset, len));
                        }
                    }
                }
            }
        }
        if let Some(active::Record {
            stopped: false,
            phrase,
        }) = self.record.as_mut()
        {
            // push to/replace last record
            if let Some(Event { input: i, steps }) = phrase.events.last_mut() {
                if steps.round() < 0. {
                    *i = input;
                } else {
                    let s = (*steps - 0.5).fract();
                    *steps = steps.round();
                    phrase.events.push(Event { input, steps: s });
                }
            } else {
                phrase.events.push(Event {
                    input,
                    steps: (phrase.offset - 0.5).fract(),
                });
                phrase.offset = phrase.offset.round();
            }
        }
        Ok(())
    }

    fn cmd_record(&mut self, recording: bool) {
        if recording {
            self.record = Some(active::Record::default());
        } else if let Some(active::Record { stopped, phrase }) = self.record.as_mut() {
            *stopped = true;
            if let Some(Event { steps, .. }) = phrase.events.last_mut() {
                *steps = steps.round();
            } else {
                phrase.offset = phrase.offset.round();
            }
        }
    }

    fn cmd_assign_onset(&mut self, index: u8, alt: bool, onset: Onset) -> Result<()> {
        let mut wav = active::Wav::open(&onset.wav)?;
        wav.seek(onset.start as i64)?;
        let active = active::Onset {
            index,
            pan: self.generate_pan(index),
            wav,
            start: onset.start,
        };
        self.state = active::State::Input(active::Input::Hold(active));

        self.pads[index as usize].onsets[alt as usize] = Some(onset);
        Ok(())
    }

    fn cmd_assign_phrase(&mut self, index: u8) {
        if let Some(active::Record { phrase, .. }) = self.record.take() {
            self.pads[index as usize].phrase = phrase;
        }
    }

    fn cmd_clear_ghost(&mut self) {
        for Pad { onset_weight, .. } in self.pads.iter_mut() {
            *onset_weight = 0;
        }
    }

    fn cmd_push_ghost(&mut self, index: u8) {
        self.pads[index as usize].onset_weight =
            (self.pads[index as usize].onset_weight + 1).min(15);
    }

    fn cmd_clear_sequence(&mut self) {
        for Pad { phrase_weight, .. } in self.pads.iter_mut() {
            *phrase_weight = 0;
        }
    }

    fn cmd_push_sequence(&mut self, index: u8) -> Result<()> {
        self.pads[index as usize].phrase_weight =
            (self.pads[index as usize].phrase_weight + 1).min(15);
        if self.phrase.is_none() {
            // generate phrase
            self.step = 0.;
            let phrase = self.generate_phrase()?;
            self.phrase = phrase;
        }
        Ok(())
    }

    fn cmd_global(&mut self, global: Global) {
        match global {
            Global::Bias(value) => self.bias = value as f32 / 127.,
            Global::Roll(value) => self.roll = value as f32 / 127.,
            Global::Drift(value) => self.drift = value as f32 / 127.,
            Global::Width(value) => self.width = value as f32 / 127.,
        }
    }
}
