use super::active;

use std::{fs::File, io::Read};
use cpal::{FromSample, SizedSample};
use color_eyre::Result;

#[derive(Default)]
pub struct Pad {
    pub onsets: [Option<super::Onset>; 2],
    pub phrase: Option<super::Phrase>,
}

pub struct Pads<const N: usize> {
    pub inner: [Pad; N],
}

impl<const N: usize> Pads<N> {
    pub fn generate_pan(index: impl Into<usize>) -> f32 {
        index.into() as f32 / super::PAD_COUNT as f32 - 0.5
    }

    pub fn new() -> Self {
        Self { inner: core::array::from_fn(|_| Pad::default()) }
    }

    pub fn onset(
        &self,
        index: impl Into<usize> + Copy,
        alt: bool,
        pan: f32,
    ) -> Result<active::Onset, std::io::Error> {
        let super::Onset { wav, start, .. } = self.inner[index.into()].onsets[alt as usize].as_ref().unwrap();
        let wav = active::Wav {
            tempo: wav.rd.tempo,
            steps: wav.rd.steps,
            file: File::open(wav.path.clone())?,
            len: wav.len,
        };
        Ok(active::Onset {
            index: index.into() as u8,
            pan,
            wav,
            start: *start,
        })
    }

    pub fn onset_seek(
        &self,
        index: impl Into<usize> + Copy,
        alt: bool,
        pan: f32,
    ) -> Result<active::Onset, std::io::Error> {
        let super::Onset { wav, start, .. } = self.inner[index.into()].onsets[alt as usize].as_ref().unwrap();
        let mut wav = active::Wav {
            tempo: wav.rd.tempo,
            steps: wav.rd.steps,
            file: File::open(wav.path.clone())?,
            len: wav.len,
        };
        wav.seek(*start as i64)?;
        Ok(active::Onset {
            index: index.into() as u8,
            pan,
            wav,
            start: *start,
        })
    }

    pub fn generate_alt(&self, index: impl Into<usize>, bias: f32) -> Option<bool> {
        match self.inner[index.into()].onsets {
            [None, None] => None,
            [Some(_), None] => Some(false),
            [None, Some(_)] => Some(true),
            [Some(_), Some(_)] => Some(rand::random_bool(bias as f64)),
        }
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
    quant: bool,
    clock: f32,
    tempo: f32,

    bias: f32,
    drift: f32,
    speed: Mod<f32>,
    width: f32,

    pads: Pads<N>,
    input: active::Input,
    record: active::Record,
    // FIXME: second phrsae field for lower pool
    pool: active::Pool,

    cmd_rx: std::sync::mpsc::Receiver<super::Cmd>,
}

impl<const N: usize> PadsHandler<N> {
    fn read_grain<T>(
        onset: &mut active::Onset,
        tempo: f32,
        speed: f32,
        width: f32,
        buffer: &mut [T],
        channels: usize,
    ) -> Result<()>
    where
        T: SizedSample + FromSample<f32>,
    {
        let speed = if let Some(t) = onset.wav.tempo {
            tempo * super::STEP_DIV as f32 / t * speed
        } else {
            speed
        };
        let rem = (super::GRAIN_LEN as f32 * 2. * speed) as usize & !1;
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
            // FIXME: support alternative channel counts?
            assert!(channels == 2);
            // handle float shenanigans(?)
            let sample = if read_idx as usize * 2 + 4 < read.len() {
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

    pub fn new(cmd_rx: std::sync::mpsc::Receiver<super::Cmd>) -> Self {
        Self {
            quant: false,
            clock: 0.,
            tempo: 0.,

            bias: 0.,
            drift: 0.,
            speed: Mod::new(1., 1.),
            width: 1.,

            pads: Pads::new(),
            input: active::Input::new(),
            record: active::Record::new(),
            pool: active::Pool::new(),

            cmd_rx,
        }
    }

    pub fn tick<T>(&mut self, buffer: &mut [T], channels: usize) -> Result<()>
    where
        T: SizedSample + FromSample<f32>,
    {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                super::Cmd::Start => self.start(),
                super::Cmd::Clock => self.clock()?,
                super::Cmd::Stop => self.stop(),
                super::Cmd::AssignTempo(v) => self.assign_tempo(v),
                super::Cmd::AssignBias(v) => self.assign_bias(v),
                super::Cmd::AssignDrift(v) => self.assign_drift(v),
                super::Cmd::AssignSpeed(v) => self.assign_speed(v),
                super::Cmd::OffsetSpeed(v) => self.offset_speed(v),
                super::Cmd::AssignWidth(v) => self.assign_width(v),
                super::Cmd::AssignOnset(index, alt, onset) => self.assign_onset(index, alt, onset)?,
                super::Cmd::Input(event) => self.input(event)?,
                super::Cmd::BakeRecord(len) => self.bake_record(len)?,
                super::Cmd::TakeRecord(index) => self.take_record(index),
                super::Cmd::PushPool(index) => self.push_pool(index),
                super::Cmd::ClearPool => self.clear_pool(),
            }
        }
        self.handle_read(buffer, channels)?;
        Ok(())
    }

    fn handle_read<T>(&mut self, buffer: &mut [T], channels: usize) -> Result<()>
    where
        T: SizedSample + FromSample<f32>
    {
        // FIXME: init tempo of 0. means no playback until clock start (then can stop to init tempo to nonzero)
        let active = if !matches!(self.input.active, active::Event::Sync) {
            &mut self.input.active
        } else if self.record.active.as_ref().is_some_and(|v| !matches!(v.active, active::Event::Sync)) {
            &mut self.record.active.as_mut().unwrap().active
        } else if self.pool.active.as_ref().is_some_and(|v| !matches!(v.active, active::Event::Sync)) {
            &mut self.pool.active.as_mut().unwrap().active
        } else {
            &mut active::Event::Sync
        };
        if self.tempo > 0. {
            if let active::Event::Hold(onset) = active {
                return Self::read_grain(onset, self.tempo, self.speed.get(), self.width, buffer, channels);
            } else if let active::Event::Loop(onset, len) = active {
                let wav = &mut onset.wav;
                let pos = wav.pos()?;
                let end = onset.start + (f32::from(*len) * wav.len as f32 / wav.steps as f32) as u64;
                if end < pos || pos < onset.start && end < pos + wav.len {
                    wav.seek(onset.start as i64)?;
                }
                return Self::read_grain(onset, self.tempo, self.speed.get(), self.width, buffer, channels);
            }
        }
        buffer.fill(T::EQUILIBRIUM);
        Ok(())
    }

    fn start(&mut self) {
        self.quant = true;
        self.clock = 0.;
    }

    fn clock(&mut self) -> Result<()> {
        self.quant = true;
        self.clock += 1.;
        if let Some(input) = self.input.buffer.take() {
            self.process_input(input)?;
        } else {
            // sync with clock
            let event = if matches!(self.input.active, active::Event::Sync) {
                self.pool.active.as_mut().map(|v| &mut v.active)
            } else {
                Some(&mut self.input.active)
            };
            if let Some(active::Event::Hold(onset)) = event {
                let wav = &mut onset.wav;
                if wav.tempo.is_some() {
                    let offset = (wav.len as f32 / wav.steps as f32 * self.clock) as i64 & !1;
                    wav.seek(onset.start as i64 + offset)?;
                }
            } else if let Some(active::Event::Loop(onset, len)) = event {
                let wav = &mut onset.wav;
                if wav.tempo.is_some() {
                    let offset = (wav.len as f32 / wav.steps as f32 * (self.clock % f32::from(*len))) as i64 & !1;
                    wav.seek(onset.start as i64 + offset)?;
                }
            }
        }
        self.tick_pool()?;
        Ok(())
    }

    fn stop(&mut self) {
        self.quant = false;
        self.clock = 0.;
    }

    fn assign_tempo(&mut self, tempo: f32) {
        self.tempo = tempo;
    }

    fn assign_bias(&mut self, bias: f32) {
        self.bias = bias;
    }

    fn assign_drift(&mut self, drift: f32) {
        self.drift = drift;
    }

    fn assign_speed(&mut self, base: f32) {
        self.speed.base = base;
    }
    
    fn offset_speed(&mut self, offset: f32) {
        self.speed.offset = offset;
    }

    fn assign_width(&mut self, width: f32) {
        self.width = width;
    }

    fn assign_onset(&mut self, index: u8, alt: bool, onset: super::Onset) -> Result<()> {
        self.pads.inner[index as usize].onsets[alt as usize] = Some(onset);
        self.input.active.trans(&super::Event::Hold { index }, self.bias, &self.pads)?;
        // FIXME: ???
        // self.clock = 0.;
        Ok(())
    }

    fn input(&mut self, event: super::Event) -> Result<()> {
        if self.quant {
            self.input.buffer = Some(event);
        } else {
            self.process_input(event)?;
        }
        Ok(())
    }

    fn bake_record(&mut self, len: u16) -> Result<()> {
        if self.record.active.is_none() {
            self.record.bake(self.clock as u16);
        }
        self.record.trim(len, self.bias, self.drift, &self.pads)?;
        Ok(())
    }

    fn take_record(&mut self, index: Option<u8>) {
        if let Some((phrase, active)) = self.record.take() {
            if let Some(index) = index {
                self.pads.inner[index as usize].phrase = Some(phrase);
            }
            self.pool.index = index;
            self.pool.active = Some(active);
        }
    }

    fn push_pool(&mut self, index: u8) {
        self.pool.phrases.push(index);
    }

    fn clear_pool(&mut self) {
        self.pool.phrases.clear();
    }

    fn process_input(&mut self, event: super::Event) -> Result<()> {
        self.input.active.trans(&event, self.bias, &self.pads)?;
        self.record.push(event, self.clock as u16);
        Ok(())
    }

    fn tick_pool(&mut self) -> Result<()> {
        // advance phrase/pool, if any
        if let Some(active::Phrase {
            next,
            event_rem,
            phrase_rem,
            active,
        }) = self.pool.active.as_mut() {
            *event_rem = event_rem.saturating_sub(1);
            *phrase_rem = phrase_rem.saturating_sub(1);
            if *phrase_rem == 0 {
                // generate next phrase from pool
                self.pool.index = self.pool.generate_phrase(self.bias, self.drift, &self.pads)?;
                self.pool.next += 1;
                // FIXME: ???
                // self.clock = self.clock.fract();
            } else if *event_rem == 0 {
                // generate next event from pool
                if let Some(phrase) = self.pool.index.and_then(|v| self.pads.inner[v as usize].phrase.as_ref()) {
                    if let Some(rem) = phrase.generate_stamped(active, self.bias, self.drift, &self.pads)? {
                        *next += 1;
                        *event_rem = rem;
                    }
                }
            }
        }
        Ok(())
    }
}

// impl<const N: usize> PadsHandler<N> {
//     fn read_grain<T>(
//         tempo: f32,
//         speed: f32,
//         width: f32,
//         onset: &mut active::Onset,
//         buffer: &mut [T],
//         channels: usize,
//     ) -> Result<()>
//     where
//         T: SizedSample + FromSample<f32>,
//     {
//         let speed = if let Some(t) = onset.wav.tempo {
//             tempo * STEP_DIV as f32 / t * speed
//         } else {
//             speed
//         };
//         let rem = (GRAIN_LEN as f32 * 2. * speed) as usize & !1;
//         let mut read = vec![0u8; rem + 2];
//         let mut slice = &mut read[..];
//         let wav = &mut onset.wav;
//         // read grain
//         while !slice.is_empty() {
//             let n = wav.file.read(slice)?;
//             if n == 0 {
//                 wav.seek(0)?;
//             }
//             slice = &mut slice[n..];
//         }
//         // resync from reading extra word for interpolation
//         let pos = wav.pos()?;
//         wav.seek(pos as i64 - 2)?;
//         // resample via linear interpolation
//         for i in 0..buffer.len() / channels {
//             let read_idx = i as f32 * speed;
//             let mut i16_buffer = [0u8; 2];

//             // TODO: support alternative channel counts?
//             assert!(channels == 2);
//             // handle float shenanigans(?)
//             let sample = if read_idx as usize * 2  + 4 < read.len() {
//                 i16_buffer.copy_from_slice(&read[read_idx as usize * 2..][0..2]);
//                 let word_a = i16::from_le_bytes(i16_buffer) as f32 / i16::MAX as f32 * read_idx.fract();
//                 i16_buffer.copy_from_slice(&read[read_idx as usize * 2..][2..4]);
//                 let word_b = i16::from_le_bytes(i16_buffer) as f32 / i16::MAX as f32 * (1. - read_idx.fract());
//                 word_a + word_b
//             } else {
//                 i16_buffer.copy_from_slice(&read[read_idx as usize * 2..][0..2]);
//                 i16::from_le_bytes(i16_buffer) as f32 / i16::MAX as f32
//             };

//             let l = sample * (1. + width * ((onset.pan - 0.5).abs() - 1.));
//             let r = sample * (1. + width * ((onset.pan + 0.5).abs() - 1.));
//             buffer[i * channels] = T::from_sample(l);
//             buffer[i * channels + 1] = T::from_sample(r);
//         }
//         Ok(())
//     }

//     fn handle_event<T>(
//         tempo: f32,
//         speed: f32,
//         width: f32,
//         event: &mut active::Event,
//         buffer: &mut [T],
//         channels: usize,
//     ) -> Result<()>
//     where
//         T: SizedSample + FromSample<f32>,
//     {
//         // FIXME: init tempo of 0. means no playback until clock start (then can stop) to init tempo to nonzero
//         if tempo > 0. {
//             if let active::Event::Hold(onset) = event {
//                 return Self::read_grain(tempo, speed, width, onset, buffer, channels);
//             } else if let active::Event::Loop(onset, len) = event {
//                 let wav = &mut onset.wav;
//                 let pos = wav.pos()?;
//                 let end = onset.start + (f32::from(*len) * wav.len as f32 / wav.steps as f32) as u64;
//                 if end < pos || pos < onset.start && end < pos + wav.len {
//                     wav.seek(onset.start as i64)?;
//                 }
//                 return Self::read_grain(tempo, speed, width, onset, buffer, channels);
//             }
//         }
//         for _ in 0..GRAIN_LEN {
//             buffer.fill(T::EQUILIBRIUM);
//         }
//         Ok(())
//     }

//     pub fn new(input_rx: std::sync::mpsc::Receiver<Cmd>) -> Self {
//         Self {
//             clock: 0.,
//             quant: false,
//             state: active::State::Input(active::Event::Sync),
//             input: None,
//             pads: Pads::new(),
//             bias: 0.,
//             drift: 0.,
//             width: 1.,
//             speed: Mod::new(1., 1.),
//             tempo: 0.,
//             input_rx,
//         }
//     }

//     pub fn tick<T>(&mut self, buffer: &mut [T], channels: usize) -> Result<()>
//     where
//         T: SizedSample + FromSample<f32>,
//     {
//         while let Ok(cmd) = self.input_rx.try_recv() {
//             match cmd {
//                 Cmd::Start => self.cmd_start()?,
//                 Cmd::Clock => self.cmd_clock()?,
//                 Cmd::Stop => self.cmd_stop()?,
//                 Cmd::Input(event) => self.cmd_input(event)?,
//                 Cmd::AssignTempo(tempo) => self.tempo = tempo,
//                 Cmd::AssignSpeed(speed) => self.speed.base = speed,
//                 Cmd::OffsetSpeed(speed) => self.speed.offset = speed,
//                 Cmd::AssignOnset(index, alt, onset) => self.cmd_assign_onset(index, alt, onset)?,
//                 Cmd::ClearGhost => self.cmd_clear_ghost(),
//                 Cmd::PushGhost(index) => self.cmd_push_ghost(index),
//                 Cmd::AssignGlobal(global) => self.cmd_assign_global(global),
//             }
//         }
//         let event = match &mut self.state {
//             active::State::Input(event) => event,
//             active::State::Ghost(event, ..) => event,
//         };
//         Self::handle_event(self.tempo, self.speed.get(), self.width, event, buffer, channels)?;
//         Ok(())
//     }

//     fn cmd_start(&mut self) -> Result<()> {
//         self.quant = true;
//         self.clock = 0.;
//         Ok(())
//     }

//     fn cmd_clock(&mut self) -> Result<()> {
//         self.quant = true;
//         if let Some(input) = self.input.take() {
//             self.process_input(input)?;
//         } else {
//             self.clock += 1.;
//             // sync with clock
//             let event = match &mut self.state {
//                 active::State::Input(event) => event,
//                 active::State::Ghost(event, ..) => event,
//             };
//             if let active::Event::Hold(onset) = event {
//                 let wav = &mut onset.wav;
//                 if wav.tempo.is_some() {
//                     let offset = (wav.len as f32 / wav.steps as f32 * self.clock) as i64 & !1;
//                     wav.seek(onset.start as i64 + offset)?;
//                 }
//             } else if let active::Event::Loop(onset, len) = event {
//                 let wav = &mut onset.wav;
//                 if wav.tempo.is_some() {
//                     let offset = (wav.len as f32 / wav.steps as f32 * (self.clock % f32::from(*len))) as i64 & !1;
//                     wav.seek(onset.start as i64 + offset)?;
//                 }
//             }
//         }
//         self.advance_active()?;
//         Ok(())
//     }

//     fn cmd_stop(&mut self) -> Result<()> {
//         self.quant = false;
//         self.clock = 0.;
//         Ok(())
//     }

//     fn cmd_input(&mut self, event: Event) -> Result<()> {
//         if self.quant {
//             self.input = Some(event);
//         } else {
//             self.process_input(event)?;
//         }
//         Ok(())
//     }

//     fn cmd_assign_onset(&mut self, index: u8, alt: bool, onset: Onset) -> Result<()> {
//         self.pads.inner[index as usize].onsets[alt as usize] = Some(onset);
//         let onset = self.pads.onset_seek(index, alt, self.generate_pan(index))?;
//         self.state = active::State::Input(active::Event::Hold(onset));
//         self.clock = 0.;
//         Ok(())
//     }

//     fn cmd_clear_ghost(&mut self) {
//         for Pad { onset_weight, .. } in self.pads.inner.iter_mut() {
//             *onset_weight = 0;
//         }
//     }

//     fn cmd_push_ghost(&mut self, index: u8) {
//         let weight = &mut self.pads.inner[index as usize].onset_weight;
//         *weight = (*weight + 1).min(15);
//     }

//     fn cmd_assign_global(&mut self, global: Global) {
//         match global {
//             Global::Bias(value) => self.bias = value as f32 / 127.,
//             Global::Roll(value) => self.roll = value as f32 / 127.,
//             Global::Drift(value) => self.drift = value as f32 / 127.,
//             Global::Width(value) => self.width = value as f32 / 127.,
//         }
//     }

//     fn process_input(&mut self, event: Event) -> Result<()> {
//         match event {
//             Event::Sync => {
//                 // generate ghost?
//                 if let Some((input, rem)) = self.generate_ghost()? {
//                     self.state = active::State::Ghost(input, rem);
//                 } else {
//                     self.state = active::State::Input(active::Event::Sync);
//                 }
//                 self.clock = 0.;
//             }
//             Event::Hold { index } => {
//                 if let active::State::Input(active::Event::Loop(onset, ..)) = &mut self.state {
//                     // downcast input variant with same active::Onset
//                     let uninit: &mut MaybeUninit<active::Onset> = unsafe { std::mem::transmute(onset) };
//                     let mut onset = unsafe { std::mem::replace(uninit, MaybeUninit::uninit()).assume_init() };
//                     // i don't know either, girl
//                     onset.wav.file = onset.wav.file.try_clone()?;
//                     self.state = active::State::Input(active::Event::Hold(onset));
//                 } else if let Some(alt) = self.generate_alt(index) {
//                     let onset = self.pads.onset_seek(index, alt, self.generate_pan(index))?;
//                     self.state = active::State::Input(active::Event::Hold(onset));
//                     self.clock = 0.;
//                 }
//             }
//             Event::Loop { index, len } => {
//                 match &mut self.state {
//                     active::State::Input(
//                         active::Event::Hold(onset) | active::Event::Loop(onset, ..),
//                     ) if onset.index == index => {
//                         // recast event variant with same active::Onset
//                         let uninit: &mut MaybeUninit<active::Onset> = unsafe { std::mem::transmute(onset) };
//                         let mut onset = unsafe { std::mem::replace(uninit, MaybeUninit::uninit()).assume_init() };
//                         // i don't know either, girl
//                         onset.wav.file = onset.wav.file.try_clone()?;
//                         self.state = active::State::Input(active::Event::Loop(onset, len));
//                         self.clock += f32::from(len);
//                     }
//                     _ => {
//                         if let Some(alt) = self.generate_alt(index) {
//                             let onset = self.pads.onset(index, alt, self.generate_pan(index))?;
//                             self.state = active::State::Input(active::Event::Loop(onset, len));
//                         }
//                         self.clock = 0.;
//                     }
//                 }
//             }
//         }
//         Ok(())
//     }

//     fn advance_active(&mut self) -> Result<()> {
//         self.try_advance_ghost()?;
//         Ok(())
//     }

//     /// advance ghost, if any
//     fn try_advance_ghost(&mut self) -> Result<()> {
//         // advance ghost, if any
//         if let active::State::Ghost(_, rem) = &mut self.state {
//             *rem = rem.saturating_sub(1);
//             if *rem == 0 {
//                 // generate ghost?
//                 if let Some((event, rem)) = self.generate_ghost()? {
//                     self.state = active::State::Ghost(event, rem);
//                 } else {
//                     self.state = active::State::Input(active::Event::Sync);
//                 }
//             }
//         }
//         Ok(())
//     }

//     fn generate_alt(&self, index: impl Into<usize>) -> Option<bool> {
//         match self.pads.inner[index.into()].onsets {
//             [None, None] => None,
//             [Some(_), None] => Some(false),
//             [None, Some(_)] => Some(true),
//             [Some(_), Some(_)] => Some(rand::random_bool(self.bias as f64)),
//         }
//     }

//     fn generate_pan(&self, index: impl Into<usize>) -> f32 {
//         index.into() as f32 / 8. - 0.5
//     }

//     fn generate_ghost(&mut self) -> Result<Option<(active::Event, u16)>> {
//         let mut weights = self.pads.onset_weights();
//         let sum = weights.iter().fold(0, |acc, v| acc + *v as u16);
//         if sum == 0 {
//             return Ok(None);
//         }
//         let mut offset = rand::random_range(1..=sum);
//         let mut index = 0;
//         while offset > 0 {
//             while weights[index] == 0 {
//                 index += 1;
//             }
//             weights[index] -= 1;
//             offset -= 1;
//         }
//         let (input, rem) = if let Some(alt) = self.generate_alt(index) {
//             let steps = self.pads.inner[index].onsets[alt as usize].as_ref().unwrap().steps;
//             let onset = self.pads.onset_seek(index, alt, self.generate_pan(index))?;
//             if rand::random_bool(self.roll as f64) {
//                 let steps = rand::random_range(1..=STEP_DIV) as u16;
//                 let len = Fraction::new(rand::random_range(1..=steps as u8 * LOOP_DIV), LOOP_DIV);
//                 (active::Event::Loop(onset, len), steps)
//             } else {
//                 (active::Event::Hold(onset), steps)
//             }
//         } else {
//             let steps = rand::random_range(0..STEP_DIV) as u16;
//             (active::Event::Sync, steps)
//         };
//         Ok(Some((input, rem)))
//     }
// }
