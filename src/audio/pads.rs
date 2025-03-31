use super::*;
use std::{fs::File, io::Read, mem::MaybeUninit};
use cpal::{FromSample, SizedSample};

#[derive(Default)]
struct Pad {
    onsets: [Option<Onset>; 2],
    onset_weight: u8,
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
}

struct Mod<T: Copy + std::ops::Mul> {
    base: T,
    offset: T,
}

impl<T: Copy + std::ops::Mul> Mod<T> {
    pub fn new(base: T, offset: T) -> Self {
        Self { base, offset }
    }

    pub fn get(&self) -> T::Output {
        self.base * self.offset
    }
}

pub struct PadsHandler<const N: usize> {
    clock: f32,
    quant: bool,
    state: active::State,
    input: Option<Event>,
    pads: Pads<N>,
    bias: f32,
    roll: f32,
    drift: f32,
    width: f32,
    tempo: f32,
    speed: Mod<f32>,
    input_rx: std::sync::mpsc::Receiver<Cmd>,
}

impl<const N: usize> PadsHandler<N> {
    fn read_grain<T>(
        tempo: f32,
        speed: f32,
        width: f32,
        onset: &mut active::Onset,
        buffer: &mut [T],
        channels: usize,
    ) -> Result<()>
    where
        T: SizedSample + FromSample<f32>,
    {
        let speed = if let Some(t) = onset.wav.tempo {
            tempo * STEP_DIV as f32 / t * speed
        } else {
            speed
        };
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
        speed: f32,
        width: f32,
        event: &mut active::Event,
        buffer: &mut [T],
        channels: usize,
    ) -> Result<()>
    where
        T: SizedSample + FromSample<f32>,
    {
        // FIXME: init tempo of 0. means no playback until clock start (then can stop) to init tempo to nonzero
        if tempo > 0. {
            if let active::Event::Hold(onset) = event {
                return Self::read_grain(tempo, speed, width, onset, buffer, channels);
            } else if let active::Event::Loop(onset, len) = event {
                let wav = &mut onset.wav;
                let pos = wav.pos()?;
                let end = onset.start + (f32::from(*len) * wav.len as f32 / wav.steps as f32) as u64;
                if end < pos || pos < onset.start && end < pos + wav.len {
                    wav.seek(onset.start as i64)?;
                }
                return Self::read_grain(tempo, speed, width, onset, buffer, channels);
            }
        }
        for _ in 0..GRAIN_LEN {
            buffer.fill(T::EQUILIBRIUM);
        }
        Ok(())
    }

    pub fn new(input_rx: std::sync::mpsc::Receiver<Cmd>) -> Self {
        Self {
            clock: 0.,
            quant: false,
            state: active::State::Input(active::Event::Sync),
            input: None,
            pads: Pads::new(),
            bias: 0.,
            roll: 0.,
            drift: 0.,
            width: 1.,
            tempo: 0.,
            speed: Mod::new(1., 1.),
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
                Cmd::Stop => self.cmd_stop()?,
                Cmd::Input(event) => self.cmd_input(event)?,
                Cmd::AssignTempo(tempo) => self.tempo = tempo,
                Cmd::AssignSpeed(speed) => self.speed.base = speed,
                Cmd::OffsetSpeed(speed) => self.speed.offset = speed,
                Cmd::AssignOnset(index, alt, onset) => self.cmd_assign_onset(index, alt, onset)?,
                Cmd::ClearGhost => self.cmd_clear_ghost(),
                Cmd::PushGhost(index) => self.cmd_push_ghost(index),
                Cmd::AssignGlobal(global) => self.cmd_assign_global(global),
            }
        }
        let event = match &mut self.state {
            active::State::Input(event) => event,
            active::State::Ghost(event, ..) => event,
        };
        Self::handle_event(self.tempo, self.speed.get(), self.width, event, buffer, channels)?;
        Ok(())
    }

    fn cmd_start(&mut self) -> Result<()> {
        self.quant = true;
        self.clock = 0.;
        Ok(())
    }

    fn cmd_clock(&mut self) -> Result<()> {
        self.quant = true;
        if let Some(input) = self.input.take() {
            self.process_input(input)?;
        } else {
            self.clock += 1.;
            // sync with clock
            let event = match &mut self.state {
                active::State::Input(event) => event,
                active::State::Ghost(event, ..) => event,
            };
            if let active::Event::Hold(onset) = event {
                let wav = &mut onset.wav;
                if wav.tempo.is_some() {
                    let offset = (wav.len as f32 / wav.steps as f32 * self.clock) as i64 & !1;
                    wav.seek(onset.start as i64 + offset)?;
                }
            } else if let active::Event::Loop(onset, len) = event {
                let wav = &mut onset.wav;
                if wav.tempo.is_some() {
                    let offset = (wav.len as f32 / wav.steps as f32 * (self.clock % f32::from(*len))) as i64 & !1;
                    wav.seek(onset.start as i64 + offset)?;
                }
            }
        }
        self.advance_active()?;
        Ok(())
    }

    fn cmd_stop(&mut self) -> Result<()> {
        self.quant = false;
        self.clock = 0.;
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

    fn cmd_assign_onset(&mut self, index: u8, alt: bool, onset: Onset) -> Result<()> {
        self.pads.inner[index as usize].onsets[alt as usize] = Some(onset);
        let onset = self.pads.onset_seek(index, alt, self.generate_pan(index))?;
        self.state = active::State::Input(active::Event::Hold(onset));
        self.clock = 0.;
        Ok(())
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
                // generate ghost?
                if let Some((input, rem)) = self.generate_ghost()? {
                    self.state = active::State::Ghost(input, rem);
                } else {
                    self.state = active::State::Input(active::Event::Sync);
                }
                self.clock = 0.;
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
                    self.clock = 0.;
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
                        self.clock += f32::from(len);
                    }
                    _ => {
                        if let Some(alt) = self.generate_alt(index) {
                            let onset = self.pads.onset(index, alt, self.generate_pan(index))?;
                            self.state = active::State::Input(active::Event::Loop(onset, len));
                        }
                        self.clock = 0.;
                    }
                }
            }
        }
        Ok(())
    }

    fn advance_active(&mut self) -> Result<()> {
        self.try_advance_ghost()?;
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
}
