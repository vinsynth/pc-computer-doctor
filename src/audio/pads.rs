use super::*;
use std::{fs::File, io::Read, mem::MaybeUninit};
use cpal::{FromSample, SizedSample};

#[derive(Default)]
struct Pad {
    onsets: [Option<Onset>; 2],
    phrase: Phrase,
    onset_weight: u8,
    phrase_weight: u8,
}

struct Pads<const N: usize> {
    inner: [Pad; N],
}

impl<const N: usize> Pads<N> {
    fn new() -> Self {
        Self { inner: core::array::from_fn(|_| Pad::default()) }
    }

    fn onset(
        &self,
        index: impl Into<usize> + Copy,
        alt: bool,
        pan: f32,
    ) -> Result<super::active::Onset, std::io::Error> {
        let Onset { wav, start, .. } = self.inner[index.into()].onsets[alt as usize].as_ref().unwrap();
        let wav = super::active::Wav {
            tempo: wav.rd.tempo,
            steps: wav.rd.steps,
            file: File::open(wav.path.clone())?,
            len: wav.len,
        };
        Ok(super::active::Onset {
            index: index.into() as u8,
            pan,
            wav,
            start: *start,
        })
    }

    fn onset_seek(
        &self,
        index: impl Into<usize> + Copy,
        alt: bool,
        pan: f32,
    ) -> Result<super::active::Onset, std::io::Error> {
        let Onset { wav, start, .. } = self.inner[index.into()].onsets[alt as usize].as_ref().unwrap();
        let mut wav = super::active::Wav {
            tempo: wav.rd.tempo,
            steps: wav.rd.steps,
            file: File::open(wav.path.clone())?,
            len: wav.len,
        };
        wav.seek(*start as i64)?;
        Ok(super::active::Onset {
            index: index.into() as u8,
            pan,
            wav,
            start: *start,
        })
    }

    fn onset_weights(&self) -> [u8; N] {
        core::array::from_fn(|i| self.inner[i].onset_weight)
    }

    fn phrase_weights(&self) -> [u8; N] {
        core::array::from_fn(|i| self.inner[i].phrase_weight)
    }
}

pub struct PadsHandler<const N: usize> {
    clock: u8,
    quant: bool,
    state: active::State,
    input: Option<Event>,
    phrase: Option<active::Phrase>,
    record: Option<active::Record>,
    pads: Pads<N>,
    bias: f32,
    roll: f32,
    drift: f32,
    width: f32,
    tempo: f32,
    input_rx: std::sync::mpsc::Receiver<Cmd>,
}

impl<const N: usize> PadsHandler<N> {
    fn read_grain<T>(
        tempo: f32,
        width: f32,
        onset: &mut active::Onset,
        buffer: &mut [T],
        channels: usize,
    ) -> Result<()>
    where
        T: SizedSample + FromSample<f32>,
    {
        let speed = tempo * STEP_DIV as f32 / onset.wav.tempo;
        let rem = (GRAIN_LEN as f32 * 2. * speed) as usize & !1;
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
        // resync from reading extra word for interpolation
        let pos = wav.pos()?;
        wav.seek(pos as i64 - 2)?;
        // resample via linear interpolation
        for i in 0..buffer.len() / channels {
            let read_idx = i as f32 * speed;
            let mut i16_buffer = [0u8; 2];

            // TODO: support alternative channel counts?
            assert!(channels == 2);
            // handle float shenanigans(?)
            let sample = if read_idx as usize * 2  + 4 < read.len() {
                i16_buffer.copy_from_slice(&read[read_idx as usize * 2..][0..2]);
                let word_a = i16::from_le_bytes(i16_buffer) as f32 / i16::MAX as f32 * read_idx.fract();
                i16_buffer.copy_from_slice(&read[read_idx as usize * 2..][2..4]);
                let word_b = i16::from_le_bytes(i16_buffer) as f32 / i16::MAX as f32 * (1. - read_idx.fract());
                word_a + word_b
            } else {
                i16_buffer.copy_from_slice(&read[read_idx as usize * 2..][0..2]);
                i16::from_le_bytes(i16_buffer) as f32 / i16::MAX as f32
            };

            let l = sample * (1. + width * ((onset.pan - 0.5).abs() - 1.));
            let r = sample * (1. + width * ((onset.pan + 0.5).abs() - 1.));
            buffer[i * channels] = T::from_sample(l);
            buffer[i * channels + 1] = T::from_sample(r);
        }
        Ok(())
    }

    fn handle_event<T>(
        tempo: f32,
        width: f32,
        event: &mut active::Event,
        buffer: &mut [T],
        channels: usize,
    ) -> Result<()>
    where
        T: SizedSample + FromSample<f32>,
    {
        if tempo > 0. {
            if let active::Event::Hold(onset) = event {
                return Self::read_grain(tempo, width, onset, buffer, channels);
            } else if let active::Event::Loop(onset, len) = event {
                let wav = &mut onset.wav;
                let pos = wav.pos()?;
                let end = onset.start + (f32::from(*len) * wav.len as f32 / wav.steps as f32) as u64;
                if end < pos || pos < onset.start && end < pos + wav.len {
                    wav.seek(onset.start as i64)?;
                }
                return Self::read_grain(tempo, width, onset, buffer, channels);
            }
        }
        for _ in 0..GRAIN_LEN {
            buffer.fill(T::EQUILIBRIUM);
        }
        Ok(())
    }

    pub fn new(input_rx: std::sync::mpsc::Receiver<Cmd>) -> Self {
        Self {
            clock: 0,
            quant: false,
            state: active::State::Input(active::Event::Sync),
            input: None,
            phrase: None,
            record: None,
            pads: Pads::new(),
            bias: 0.,
            roll: 0.,
            drift: 0.,
            width: 1.,
            tempo: 0.,
            input_rx,
        }
    }

    pub fn tick<T>(&mut self, buffer: &mut [T], channels: usize) -> Result<()>
    where
        T: SizedSample + FromSample<f32>,
    {
        while let Ok(cmd) = self.input_rx.try_recv() {
            match cmd {
                Cmd::Start => self.cmd_start()?,
                Cmd::Clock => self.cmd_clock()?,
                Cmd::Quant(quant) => self.cmd_quant(quant)?,
                Cmd::Input(event) => self.cmd_input(event)?,
                Cmd::AssignTempo(tempo) => self.tempo = tempo,
                Cmd::ClearRecord => self.cmd_clear_record(),
                Cmd::Record(recording) => self.cmd_record(recording),
                Cmd::AssignOnset(index, alt, onset) => self.cmd_assign_onset(index, alt, onset),
                Cmd::AssignPhrase(index) => self.cmd_assign_phrase(index),
                Cmd::ClearGhost => self.cmd_clear_ghost(),
                Cmd::PushGhost(index) => self.cmd_push_ghost(index),
                Cmd::ClearSequence => self.cmd_clear_sequence(),
                Cmd::PushSequence(index) => self.cmd_push_sequence(index)?,
                Cmd::AssignGlobal(global) => self.cmd_assign_global(global),
            }
        }
        let mut event = &mut active::Event::Sync;
        if let active::State::Input(e) | active::State::Ghost(e, ..) = &mut self.state {
            event = e;
        } else if let Some(e) = self.phrase.as_mut().unwrap().event.as_mut() {
            event = e;
        }
        Self::handle_event(self.tempo, self.width, event, buffer, channels)?;
        Ok(())
    }

    fn cmd_start(&mut self) -> Result<()> {
        self.quant = true;
        self.clock = 0;
        Ok(())
    }

    fn cmd_clock(&mut self) -> Result<()> {
        self.clock += 1;
        if let Some(input) = self.input.take() {
            self.process_input(input)?;
        }
        self.advance_active()?;
        Ok(())
    }

    fn cmd_quant(&mut self, quant: bool) -> Result<()> {
        self.quant = quant;
        if !quant {
            if let Some(input) = self.input.take() {
                self.process_input(input)?;
            }
        }
        Ok(())
    }

    fn cmd_input(&mut self, event: Event) -> Result<()> {
        if self.quant {
            self.input = Some(event);
        } else {
            self.process_input(event)?;
        }
        Ok(())
    }

    fn cmd_clear_record(&mut self) {
        self.record = None;
    }

    fn cmd_record(&mut self, recording: bool) {
        if recording {
            self.record = Some(active::Record::default());
        } else if let Some(active::Record { stopped, .. }) = self.record.as_mut() {
            *stopped = true;
        }
    }

    fn cmd_assign_onset(&mut self, index: u8, alt: bool, onset: Onset) {
        self.pads.inner[index as usize].onsets[alt as usize] = Some(onset);
    }

    fn cmd_assign_phrase(&mut self, index: u8) {
        if let Some(active::Record { phrase, .. }) = self.record.take() {
            self.pads.inner[index as usize].phrase = phrase;
        }
    }

    fn cmd_clear_ghost(&mut self) {
        for Pad { onset_weight, .. } in self.pads.inner.iter_mut() {
            *onset_weight = 0;
        }
    }

    fn cmd_push_ghost(&mut self, index: u8) {
        let weight = &mut self.pads.inner[index as usize].onset_weight;
        *weight = (*weight + 1).min(15);
    }

    fn cmd_clear_sequence(&mut self) {
        for Pad { phrase_weight, .. } in self.pads.inner.iter_mut() {
            *phrase_weight = 0;
        }
        if let Some(active::Phrase { event_rem, phrase_rem, .. }) = self.phrase.as_mut() {
            *phrase_rem = *event_rem;
        }
    }

    fn cmd_push_sequence(&mut self, index: u8) -> Result<()> {
        let weight = &mut self.pads.inner[index as usize].phrase_weight;
        *weight = (*weight + 1).min(15);
        if self.phrase.is_none() {
            self.phrase = self.generate_phrase()?;
        }
        Ok(())
    }

    fn cmd_assign_global(&mut self, global: Global) {
        match global {
            Global::Bias(value) => self.bias = value as f32 / 127.,
            Global::Roll(value) => self.roll = value as f32 / 127.,
            Global::Drift(value) => self.drift = value as f32 / 127.,
            Global::Width(value) => self.width = value as f32 / 127.,
        }
    }

    fn process_input(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Sync => {
                match &mut self.phrase {
                    Some(active::Phrase {
                        event_rem,
                        event: Some(active::Event::Hold(onset)),
                        ..
                    }) => {
                        // catch up to phrase
                        let offset = *event_rem as u64 * onset.wav.len / onset.wav.steps as u64;
                        onset.wav.seek(onset.start as i64 + offset as i64)?;
                        self.state = active::State::Phrase;
                    }
                    Some(active::Phrase {
                        event_rem,
                        event: Some(active::Event::Loop(onset, len)),
                        ..
                    }) => {
                        // catch up to phrase
                        let offset = *event_rem as f32 % f32::from(*len)
                            * onset.wav.len as f32
                            / onset.wav.steps as f32;
                        onset.wav.seek(onset.start as i64 + offset as i64)?;
                        self.state = active::State::Phrase;
                    }
                    _ => {
                        // generate ghost?
                        if let Some((input, rem)) = self.generate_ghost()? {
                            self.state = active::State::Ghost(input, rem);
                        } else {
                            self.state = active::State::Input(active::Event::Sync);
                        }
                    }
                }
            }
            Event::Hold { index } => {
                if let active::State::Input(active::Event::Loop(onset, ..)) = &mut self.state {
                    // downcast input variant with same active::Onset
                    let uninit: &mut MaybeUninit<active::Onset> = unsafe { std::mem::transmute(onset) };
                    let mut onset = unsafe { std::mem::replace(uninit, MaybeUninit::uninit()).assume_init() };
                    // i don't know either, girl
                    onset.wav.file = onset.wav.file.try_clone()?;
                    self.state = active::State::Input(active::Event::Hold(onset));
                } else if let Some(alt) = self.generate_alt(index) {
                    let onset = self.pads.onset_seek(index, alt, self.generate_pan(index))?;
                    self.state = active::State::Input(active::Event::Hold(onset));
                }
            }
            Event::Loop { index, len } => {
                match &mut self.state {
                    active::State::Input(
                        active::Event::Hold(onset) | active::Event::Loop(onset, ..),
                    ) if onset.index == index => {
                        // recast event variant with same active::Onset
                        let uninit: &mut MaybeUninit<active::Onset> = unsafe { std::mem::transmute(onset) };
                        let mut onset = unsafe { std::mem::replace(uninit, MaybeUninit::uninit()).assume_init() };
                        // i don't know either, girl
                        onset.wav.file = onset.wav.file.try_clone()?;
                        self.state = active::State::Input(active::Event::Loop(onset, len));
                    }
                    _ => {
                        if let Some(alt) = self.generate_alt(index) {
                            let onset = self.pads.onset(index, alt, self.generate_pan(index))?;
                            self.state = active::State::Input(active::Event::Loop(onset, len));
                        }
                    }
                }
            }
        }
        if let Some(active::Record { stopped: false, phrase }) = self.record.as_mut() {
            phrase.events.push(Stamped { event, steps: 0 });
        }
        Ok(())
    }

    fn advance_active(&mut self) -> Result<()> {
        self.try_advance_phrase()?;
        self.try_advance_ghost()?;
        self.try_advance_record();
        Ok(())
    }

    /// advance phrase, if any
    fn try_advance_phrase(&mut self) -> Result<()> {
        if let Some(active::Phrase {
            index,
            next,
            event_rem,
            phrase_rem,
            ..
        }) = self.phrase.as_mut() {
            *event_rem = event_rem.saturating_sub(1);
            *phrase_rem = phrase_rem.saturating_sub(1);
            if *phrase_rem == 0 {
                // generate phrase
                self.clock = 0;
                let phrase = self.generate_phrase()?;
                self.phrase = phrase;
            } else if *event_rem == 0 {
                // generate event
                let index = *index;
                let next = *next + 1;
                let phrase_rem = *phrase_rem;
                let (event, event_rem) = self.generate_stamped(index, next)?;
                self.phrase = Some(active::Phrase {
                    index,
                    next,
                    event_rem,
                    phrase_rem,
                    event: Some(event),
                });
            }
            // process phrase event, if any
            if !matches!(&self.state, active::State::Input(event) if !matches!(event, active::Event::Sync)) {
                if matches!(&self.phrase, Some(active::Phrase { event: Some(event), .. }) if !matches!(event, active::Event::Sync)) {
                    self.state = active::State::Phrase;
                } else if matches!(&self.state, active::State::Phrase) {
                    // generate ghost?
                    if let Some((event, rem)) = self.generate_ghost()? {
                        self.state = active::State::Ghost(event, rem);
                    } else {
                        self.state = active::State::Input(active::Event::Sync);
                    }
                }
            }
        }
        Ok(())
    }

    /// advance ghost, if any
    fn try_advance_ghost(&mut self) -> Result<()> {
        // advance ghost, if any
        if let active::State::Ghost(_, rem) = &mut self.state {
            *rem = rem.saturating_sub(1);
            if *rem == 0 {
                // generate ghost?
                if let Some((event, rem)) = self.generate_ghost()? {
                    self.state = active::State::Ghost(event, rem);
                } else {
                    self.state = active::State::Input(active::Event::Sync);
                }
            }
        }
        Ok(())
    }

    /// advance record, if any
    fn try_advance_record(&mut self) {
        if let Some(active::Record {
            stopped: false,
            phrase: Phrase { offset, events },
        }) = self.record.as_mut() {
            if let Some(Stamped { steps, .. }) = events.last_mut() {
                *steps += 1;
            } else {
                *offset += 1;
            }
        }
    }

    fn generate_alt(&self, index: impl Into<usize>) -> Option<bool> {
        match self.pads.inner[index.into()].onsets {
            [None, None] => None,
            [Some(_), None] => Some(false),
            [None, Some(_)] => Some(true),
            [Some(_), Some(_)] => Some(rand::random_bool(self.bias as f64)),
        }
    }

    fn generate_pan(&self, index: impl Into<usize>) -> f32 {
        index.into() as f32 / 8. - 0.5
    }

    fn generate_ghost(&mut self) -> Result<Option<(active::Event, u16)>> {
        let mut weights = self.pads.onset_weights();
        let sum = weights.iter().fold(0, |acc, v| acc + *v as u16);
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
        let (input, rem) = if let Some(alt) = self.generate_alt(index) {
            let steps = self.pads.inner[index].onsets[alt as usize].as_ref().unwrap().steps;
            let onset = self.pads.onset_seek(index, alt, self.generate_pan(index))?;
            if rand::random_bool(self.roll as f64) {
                let steps = rand::random_range(1..=STEP_DIV) as u16;
                let len = Fraction::new(rand::random_range(1..=steps as u8 * LOOP_DIV), LOOP_DIV);
                (active::Event::Loop(onset, len), steps)
            } else {
                (active::Event::Hold(onset), steps)
            }
        } else {
            let steps = rand::random_range(0..STEP_DIV) as u16;
            (active::Event::Sync, steps)
        };
        Ok(Some((input, rem)))
    }

    fn generate_stamped(&mut self, index: u8, next: usize) -> Result<(active::Event, u16)> {
        let Phrase { events, .. } = &self.pads.inner[index as usize].phrase;
        let drift = rand::random_range(0..=((self.drift * events.len() as f32 - 1.).round()) as usize);
        let index = (next + drift) % events.len();
        let Stamped { event, steps } = &events[index];
        let mut active = active::Event::Sync;
        if let Event::Hold { index } = event {
            if let Some(alt) = self.generate_alt(*index) {
                let onset = self.pads.onset_seek(*index, alt, self.generate_pan(*index))?;
                active = active::Event::Hold(onset);
            }
        } else if let Event::Loop { index, len } = event {
            if let Some(alt) = self.generate_alt(*index) {
                let onset = self.pads.onset(*index, alt, self.generate_pan(*index))?;
                active = active::Event::Loop(onset, *len);
            }
        }
        Ok((active, *steps))
    }

    fn generate_phrase(&mut self) -> Result<Option<active::Phrase>> {
        let mut weights = self.pads.phrase_weights();
        let sum = self.pads.inner.iter().fold(0, |acc, v| {
            acc + v.phrase_weight as u16 * !v.phrase.is_empty() as u16
        });
        if sum == 0 {
            return Ok(None);
        }
        let mut offset = rand::random_range(1..=sum);
        let mut index = 0;
        while offset > 0 {
            while weights[index] == 0 || self.pads.inner[index].phrase.is_empty() {
                index += 1;
            }
            weights[index] -= 1;
            offset -= 1;
        }
        let phrase = &self.pads.inner[index].phrase;
        let phrase_rem = phrase.offset + phrase.events.iter().fold(0, |acc, v| acc + v.steps);
        if phrase.offset == 0 {
            let (input, event_rem) = self.generate_stamped(index as u8, 0)?;
            Ok(Some(active::Phrase {
                index: index as u8,
                next: 1,
                event_rem,
                phrase_rem,
                event: Some(input),
            }))
        } else {
            Ok(Some(active::Phrase {
                index: index as u8,
                next: 0,
                event_rem: phrase.offset,
                phrase_rem,
                event: None,
            }))
        }
    }
}
