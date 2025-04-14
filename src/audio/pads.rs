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
pub struct Pads<const N: usize> {
    #[serde(with = "serde_arrays")]
    pub inner: [Pad; N],
}

impl<const N: usize> Pads<N> {
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
    pub kits: [Pads<N>; N],
}

impl<const N: usize> Scene<N> {
    pub fn new() -> Self {
        Self { kits: core::array::from_fn(|_| Pads::<N>::new()) }
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

    scene: Scene<N>,
    pads: Pads<N>,
    input: active::Input,
    record: active::Record,
    // FIXME: second phrsae field for lower pool
    pool: active::Pool,

    cmd_rx: std::sync::mpsc::Receiver<super::Cmd<N>>,
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

    pub fn new(cmd_rx: std::sync::mpsc::Receiver<super::Cmd<N>>) -> Self {
        Self {
            quant: false,
            clock: 0.,
            tempo: 0.,

            bias: 0.,
            drift: 0.,
            speed: Mod::new(1., 1.),
            width: 1.,

            scene: Scene::new(),
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
                super::Cmd::Clock => self.clock()?,
                super::Cmd::Stop => self.stop(),
                super::Cmd::AssignTempo(v) => self.assign_tempo(v),

                super::Cmd::AssignSpeed(v) => self.assign_speed(v),
                super::Cmd::OffsetSpeed(v) => self.offset_speed(v),
                super::Cmd::AssignDrift(v) => self.assign_drift(v),
                super::Cmd::AssignBias(v) => self.assign_bias(v),
                super::Cmd::AssignWidth(v) => self.assign_width(v),

                super::Cmd::AssignKit(index) => self.assign_kit(index),
                super::Cmd::LoadKit(index) => self.load_kit(index),
                super::Cmd::SaveScene(file) => self.save_scene(file)?,
                super::Cmd::LoadScene(scene) => self.load_scene(*scene),

                super::Cmd::AssignOnset(index, alt, onset) => self.assign_onset(index, alt, *onset)?,
                super::Cmd::ForceSync => self.force_sync()?,
                super::Cmd::Input(event) => self.input(event)?,
                super::Cmd::TakeRecord(index) => self.take_record(index),
                super::Cmd::BakeRecord(len) => self.bake_record(len)?,
                super::Cmd::ClearPool => self.clear_pool(),
                super::Cmd::PushPool(index) => self.push_pool(index),
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
            if let active::Event::Hold(onset, ..) = active {
                return Self::read_grain(onset, self.tempo, self.speed.get(), self.width, buffer, channels);
            } else if let active::Event::Loop(onset, _, len) = active {
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

    fn clock(&mut self) -> Result<()> {
        self.quant = true;
        self.clock += 1.;
        if let Some(input) = self.input.buffer.take() {
            self.process_input(input)?;
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
                        if wav.tempo.is_some() {
                            let offset = (wav.len as f32 / wav.steps as f32 * (self.clock - *step as f32)) as i64 & !1;
                            wav.seek(onset.start as i64 + offset)?;
                        }
                    }
                    active::Event::Loop(onset, step, len) => {
                        let wav = &mut onset.wav;
                        if wav.tempo.is_some() {
                            let offset = (wav.len as f32 / wav.steps as f32 * ((self.clock - *step as f32) % f32::from(*len))) as i64 & !1;
                            wav.seek(onset.start as i64 + offset)?;
                        }
                    }
                    _ => (),
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

    fn assign_kit(&mut self, index: u8) {
        self.scene.kits[index as usize] = self.pads.clone();
    }

    fn load_kit(&mut self, index: u8) {
        self.pads = self.scene.kits[index as usize].clone();
    }

    fn save_scene(&mut self, mut file: std::fs::File) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.scene)?;
        write!(file, "{}", json)?;
        Ok(())
    }

    fn load_scene(&mut self, scene: Scene<N>) {
        self.scene = scene;
    }

    fn assign_onset(&mut self, index: u8, alt: bool, onset: super::Onset) -> Result<()> {
        self.pads.inner[index as usize].onsets[alt as usize] = Some(onset);
        self.input.active.trans(&super::Event::Hold { index }, self.clock as u16, self.bias, &self.pads)?;
        Ok(())
    }

    fn force_sync(&mut self) -> Result<()> {
        self.input.active.trans(&super::Event::Sync, self.clock as u16, self.bias, &self.pads)?;
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

    fn take_record(&mut self, index: Option<u8>) {
        if let Some((phrase, active)) = self.record.take() {
            if let Some(index) = index {
                self.pads.inner[index as usize].phrase = Some(phrase);
                self.pool.next = 1;
                self.pool.phrases.push(index);
                self.pool.index = Some(index);
                self.pool.active = Some(active);
            }
        }
    }

    fn bake_record(&mut self, len: u16) -> Result<()> {
        if self.record.active.is_none() {
            self.record.bake(self.clock as u16);
        }
        self.record.trim(len);
        self.record.generate_phrase(self.clock as u16, self.bias, self.drift, &self.pads)?;
        Ok(())
    }

    fn clear_pool(&mut self) {
        self.pool.next = 0;
        self.pool.phrases.clear();
        if let Some(active) = self.pool.active.as_mut() {
            active.phrase_rem = 0;
        }
    }

    fn push_pool(&mut self, index: u8) {
        self.pool.phrases.push(index);
    }

    fn process_input(&mut self, event: super::Event) -> Result<()> {
        self.input.active.trans(&event, self.clock as u16, self.bias, &self.pads)?;
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
        }) = self.record.active.as_mut() {
            *event_rem = event_rem.saturating_sub(1);
            *phrase_rem = phrase_rem.saturating_sub(1);
            if *phrase_rem == 0 {
                // generate next phrase from record
                self.record.generate_phrase(self.clock as u16, self.bias, self.drift, &self.pads)?;
            } else if *event_rem == 0 {
                // generate next event from record
                if let Some(phrase) = self.record.phrase.as_mut() {
                    if let Some(rem) = phrase.generate_stamped(active, *next, self.clock as u16, self.bias, self.drift, &self.pads)? {
                        *next += 1;
                        *event_rem = rem;
                    }
                }
            }
        }
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
                self.pool.generate_phrase(self.clock as u16, self.bias, self.drift, &self.pads)?;
            } else if *event_rem == 0 {
                // generate next event from pool
                if let Some(phrase) = self.pool.index.and_then(|v| self.pads.inner[v as usize].phrase.as_ref()) {
                    if let Some(rem) = phrase.generate_stamped(active, *next, self.clock as u16, self.bias, self.drift, &self.pads)? {
                        *next += 1;
                        *event_rem = rem;
                    }
                }
            }
        } else if !self.pool.phrases.is_empty() {
            // generate first phrase from pool
            self.pool.generate_phrase(self.clock as u16, self.bias, self.drift, &self.pads)?;
        }
        Ok(())
    }
}
