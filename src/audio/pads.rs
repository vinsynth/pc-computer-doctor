use crate::input::Bank;
use super::active;

use std::{fs::File, io::{Read, Write}};
use cpal::{FromSample, SizedSample};
use color_eyre::Result;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Pad {
    pub onsets: [Option<super::Onset>; 2],
    pub phrase: Option<super::Phrase>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Kit<const N: usize> {
    #[serde(with = "serde_arrays")]
    pub inner: [Pad; N],
}

impl<const N: usize> Kit<N> {
    pub fn generate_pan(index: impl Into<usize>) -> f32 {
        index.into() as f32 / N as f32 - 0.5
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
            tempo: wav.tempo,
            steps: wav.steps,
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
            tempo: wav.tempo,
            steps: wav.steps,
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

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Scene<const N: usize> {
    #[serde(with = "serde_arrays")]
    pub kit_a: [Kit<N>; N],
    #[serde(with = "serde_arrays")]
    pub kit_b: [Kit<N>; N],
}

impl<const N: usize> Scene<N> {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            kit_a: core::array::from_fn(|_| Kit::<N>::new()),
            kit_b: core::array::from_fn(|_| Kit::<N>::new()),
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

    pub fn net(&self) -> T::Output {
        self.base * self.offset
    }
}

struct BankHandler<const N: usize> {
    speed: Mod<f32>,
    drift: f32,
    bias: f32,
    width: f32,
    reverse: Option<f32>,

    kit: Kit<N>,
    input: active::Input,
    record: active::Record,
    pool: active::Pool,
}

impl<const N: usize> BankHandler<N> {
    fn new() -> Self {
        Self {
            speed: Mod::new(1., 1.),
            drift: 0.,
            bias: 0.,
            width: 1.,
            reverse: None,

            kit: Kit::new(),
            input: active::Input::new(),
            record: active::Record::new(),
            pool: active::Pool::new(),
        }
    }

    fn read_attenuated<T>(&mut self, tempo: f32, gain: f32, buffer: &mut [T], channels: usize) -> Result<()>
    where
        T: SizedSample + FromSample<f32>,
    {
        let active = if !matches!(self.input.active, active::Event::Sync) {
            &mut self.input.active
        } else if self.record.active.as_ref().is_some_and(|v| !matches!(v.active, active::Event::Sync)) {
            &mut self.record.active.as_mut().unwrap().active
        } else if self.pool.active.as_ref().is_some_and(|v| !matches!(v.active, active::Event::Sync)) {
            &mut self.pool.active.as_mut().unwrap().active
        } else {
            &mut active::Event::Sync
        };
        if tempo > 0. {
            if let active::Event::Hold(onset, ..) = active {
                return Self::read_grain(onset, self.speed.net(), self.width, self.reverse.is_some(), tempo, gain, buffer, channels);
            } else if let active::Event::Loop(onset, _, len) = active {
                let wav = &mut onset.wav;
                let pos = wav.pos()?;
                let len = if let Some(steps) = wav.steps {
                    (f32::from(*len) * wav.len as f32 / steps as f32) as u64 & !1
                } else {
                    (f32::from(*len) * super::SAMPLE_RATE as f32 * 60. / tempo * super::LOOP_DIV as f32) as u64 & !1
                };
                let end = onset.start + len;
                if pos > end || pos < onset.start && pos + wav.len > end {
                    if self.reverse.is_some() {
                        wav.seek(end as i64)?;
                    } else {
                        wav.seek(onset.start as i64)?;
                    }
                }
                return Self::read_grain(onset, self.speed.net(), self.width, self.reverse.is_some(), tempo, gain, buffer, channels);
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn read_grain<T>(onset: &mut active::Onset, speed: f32, width: f32, reverse: bool, tempo: f32, gain: f32, buffer: &mut [T], channels: usize) -> Result<()>
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
        if reverse {
            wav.seek(pos as i64 - rem as i64 * 2 - 2)?;
        } else {
            wav.seek(pos as i64 - 2)?;
        }
        // resample via linear interpolation
        for i in 0..buffer.len() / channels {
            let read_idx = if reverse {
                (rem / 2 - 1) as f32- i as f32 * speed
            } else {
                i as f32 * speed
            };
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
            let l = sample * (1. + width * ((onset.pan - 0.5).abs() - 1.)) * gain;
            let r = sample * (1. + width * ((onset.pan + 0.5).abs() - 1.)) * gain;
            buffer[i * channels] = buffer[i * channels].add_amp(T::from_sample(l).to_signed_sample());
            buffer[i * channels + 1] = buffer[i * channels + 1].add_amp(T::from_sample(r).to_signed_sample());
        }
        Ok(())
    }

    fn cmd(&mut self, quant: bool, clock: f32, kits: &mut [Kit<N>; N], cmd: super::BankCmd) -> Result<()> {
        match cmd {
            super::BankCmd::AssignSpeed(v) => self.speed.base = v,
            super::BankCmd::AssignDrift(v) => self.drift = v,
            super::BankCmd::AssignBias(v) => self.bias = v,
            super::BankCmd::AssignWidth(v) => self.width = v,
            super::BankCmd::AssignReverse(v) => self.assign_reverse(clock, v),
            super::BankCmd::AssignKit(index) => kits[index as usize] = self.kit.clone(),
            super::BankCmd::LoadKit(index) => self.kit = kits[index as usize].clone(),
            super::BankCmd::AssignOnset(index, alt, onset) => self.assign_onset(clock, index, alt, *onset)?,
            super::BankCmd::ForceEvent(event) => self.force_event(clock, event)?,
            super::BankCmd::PushEvent(event) => self.push_event(quant, clock, event)?,
            super::BankCmd::TakeRecord(index) => self.take_record(index),
            super::BankCmd::BakeRecord(len) => self.bake_record(clock, len)?,
            super::BankCmd::ClearPool => self.clear_pool(),
            super::BankCmd::PushPool(index) => self.pool.phrases.push(index),
        }
        Ok(())
    }

    fn assign_reverse(&mut self, clock: f32, reverse: bool) {
        if reverse {
            self.reverse = Some(clock);
        } else {
            self.reverse = None;
        }
    }

    fn assign_onset(&mut self, clock: f32, index: u8, alt: bool, onset: super::Onset) -> Result<()> {
        self.kit.inner[index as usize].onsets[alt as usize] = Some(onset);
        self.input.active.trans(&super::Event::Hold { index }, clock as u16, self.bias, &self.kit)?;
        Ok(())
    }

    fn clock(&mut self, clock: f32) -> Result<()> {
        if let Some(input) = self.input.buffer.take() {
            self.process_input(clock, input)?;
        } else {
            // sync all actives with clock
            let actives = [
                Some(&mut self.input.active),
                self.record.active.as_mut().map(|v| &mut v.active),
                self.pool.active.as_mut().map(|v| &mut v.active),
            ];
            for active in actives.into_iter().flatten() {
                match active {
                    active::Event::Hold(onset, step) => {
                        let wav = &mut onset.wav;
                        if let Some(steps) = wav.steps {
                            let clock = self.reverse.unwrap_or(clock);
                            let offset = (wav.len as f32 / steps as f32 * (clock - *step as f32)) as i64 & !1;
                            wav.seek(onset.start as i64 + offset)?;
                        }
                    }
                    active::Event::Loop(onset, step, len) => {
                        let wav = &mut onset.wav;
                        if let Some(steps) = wav.steps {
                            let clock = self.reverse.unwrap_or(clock);
                            let offset = (wav.len as f32 / steps as f32 * ((clock - *step as f32).rem_euclid(f32::from(*len)))) as i64 & !1;
                            wav.seek(onset.start as i64 + offset)?;
                        }
                    }
                    _ => (),
                }
            }
        }
        self.tick_phrases(clock)?;
        if let Some(clock) = self.reverse.as_mut() {
            *clock -= 1.;
        }
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(clock) = self.reverse.as_mut() {
            *clock = 0.;
        }
    }

    fn offset_speed(&mut self, v: f32) {
        self.speed.offset = v;
    }

    fn force_event(&mut self, clock: f32, event: super::Event) -> Result<()> {
        self.input.active.trans(&event, clock as u16, self.bias, &self.kit)?;
        Ok(())
    }

    fn push_event(&mut self, quant: bool, clock: f32, event: super::Event) -> Result<()> {
        if quant {
            self.input.buffer = Some(event);
        } else {
            self.process_input(clock, event)?;
        }
        Ok(())
    }

    fn take_record(&mut self, index: Option<u8>) {
        if let Some((phrase, active)) = self.record.take() {
            if let Some(index) = index {
                self.kit.inner[index as usize].phrase = Some(phrase);
                self.pool.next = 1;
                self.pool.phrases.clear();
                self.pool.phrases.push(index);
                self.pool.index = Some(index);
                self.pool.active = Some(active);
            }
        }
    }

    fn bake_record(&mut self, clock: f32, len: u16) -> Result<()> {
        if self.record.active.is_none() {
            self.record.bake(clock as u16);
        }
        self.record.trim(len);
        self.record.generate_phrase(clock as u16, self.bias, self.drift, &self.kit)?;
        Ok(())
    }

    fn clear_pool(&mut self) {
        self.pool.next = 0;
        self.pool.phrases.clear();
        if let Some(active) = self.pool.active.as_mut() {
            active.phrase_rem = 0;
        }
    }

    fn process_input(&mut self, clock: f32, event: super::Event) -> Result<()> {
        self.input.active.trans(&event, clock as u16, self.bias, &self.kit)?;
        self.record.push(event, clock as u16);
        if let Some(reverse) = &mut self.reverse {
            *reverse = clock;
        }
        Ok(())
    }

    fn tick_phrases(&mut self, clock: f32) -> Result<()> {
        // advance record phrase, if any
        if let Some(active::Phrase {
            next,
            event_rem,
            phrase_rem,
            active,
        }) = self.record.active.as_mut() {
            *event_rem = event_rem.saturating_sub(1);
            *phrase_rem = phrase_rem.saturating_sub(1);
            if *phrase_rem == 0 {
                // generate next phrase from record
                self.record.generate_phrase(clock as u16, self.bias, self.drift, &self.kit)?;
            } else if *event_rem == 0 {
                // generate next event from record
                if let Some(phrase) = self.record.phrase.as_mut() {
                    if let Some(rem) = phrase.generate_stamped(active, *next, clock as u16, self.bias, self.drift, &self.kit)? {
                        *next += 1;
                        *event_rem = rem;
                    }
                }
            }
        }
        // advance pool phrase, if any
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
                self.pool.generate_phrase(clock as u16, self.bias, self.drift, &self.kit)?;
            } else if *event_rem == 0 {
                // generate next event from pool
                if let Some(phrase) = self.pool.index.and_then(|v| self.kit.inner[v as usize].phrase.as_ref()) {
                    if let Some(rem) = phrase.generate_stamped(active, *next, clock as u16, self.bias, self.drift, &self.kit)? {
                        *next += 1;
                        *event_rem = rem;
                    }
                }
            }
        } else if !self.pool.phrases.is_empty() {
            // generate first phrase from pool
            self.pool.generate_phrase(clock as u16, self.bias, self.drift, &self.kit)?;
        }
        Ok(())
    }
}

pub struct AudioHandler<const N: usize> {
    quant: bool,
    clock: f32,
    tempo: f32,
    scene: Scene<N>,

    blend: f32,
    bank_a: BankHandler<N>,
    bank_b: BankHandler<N>,

    cmd_rx: std::sync::mpsc::Receiver<super::Cmd<N>>,
}

impl<const N: usize> AudioHandler<N> {
    pub fn new(cmd_rx: std::sync::mpsc::Receiver<super::Cmd<N>>) -> Self {
        Self {
            quant: false,
            clock: 0.,
            tempo: 0.,
            scene: Scene::new(),

            blend: 0.5,
            bank_a: BankHandler::new(),
            bank_b: BankHandler::new(),

            cmd_rx,
        }
    }

    pub fn tick<T>(&mut self, buffer: &mut [T], channels: usize) -> Result<()>
    where
        T: SizedSample + FromSample<f32>,
    {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            match cmd {
                super::Cmd::Clock => self.clock()?,
                super::Cmd::Stop => self.stop(),
                super::Cmd::AssignTempo(v) => self.tempo = v,
                super::Cmd::AssignBlend(v) => self.blend = v,
                super::Cmd::OffsetSpeed(v) => self.offset_speed(v),
                super::Cmd::SaveScene(v) => self.save_scene(v)?,
                super::Cmd::LoadScene(v) => self.scene = *v,
                super::Cmd::Bank(bank, cmd) => match bank {
                    Bank::A => self.bank_a.cmd(self.quant, self.clock, &mut self.scene.kit_a, cmd)?,
                    Bank::B => self.bank_b.cmd(self.quant, self.clock, &mut self.scene.kit_b, cmd)?,
                }
            }
        }
        buffer.fill(T::EQUILIBRIUM);
        self.bank_a.read_attenuated(self.tempo, 1. - self.blend, buffer, channels)?;
        self.bank_b.read_attenuated(self.tempo, self.blend, buffer, channels)?;
        Ok(())
    }

    fn clock(&mut self) -> Result<()> {
        self.quant = true;
        self.bank_a.clock(self.clock)?;
        self.bank_b.clock(self.clock)?;
        self.clock += 1.;
        Ok(())
    }

    fn stop(&mut self) {
        self.quant = false;
        self.bank_a.stop();
        self.bank_b.stop();
        self.clock = 0.;
    }

    fn offset_speed(&mut self, v: f32) {
        self.bank_a.offset_speed(v);
        self.bank_b.offset_speed(v);
    }

    fn save_scene(&mut self, mut file: std::fs::File) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.scene)?;
        write!(file, "{}", json)?;
        Ok(())
    }
}
