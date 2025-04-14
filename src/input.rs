use crate::{audio, tui};
use audio::PAD_COUNT;

use color_eyre::Result;
use midly::{live::LiveEvent, MidiMessage};
use std::{path::{Path, PathBuf}, sync::mpsc::Sender};

enum KeyCode {
    FsDec = 57,
    FsInto = 58,
    FsInc = 59,
    Kit = 60,
    Record = 62,
    Pool = 63,
    Hold = 64,
    PadOffset = 65,
}

enum CtrlCode {
    Speed = 105,
    Drift = 106,
    Bias = 29,
    Width = 26,
}

enum State {
    LoadOnset,
    LoadKit,
    AssignKit,
    LoadScene {
        paths: Vec<Box<Path>>,
        file_index: usize,
    },
    LoadWav {
        paths: Vec<Box<Path>>,
        file_index: usize,
    },
    AssignOnset {
        paths: Vec<Box<Path>>,
        file_index: usize,
        rd: audio::Rd,
        onset_index: usize,
    },
    BakeRecord,
    Pool { cleared: bool },
}

pub struct InputHandler {
    clock: u8,
    last_step: Option<std::time::Instant>,

    alt: bool,
    hold: bool,
    downs: Vec<u8>,
    
    state: State,
    pads_tx: Sender<audio::Cmd<PAD_COUNT>>,
    tui_tx: Sender<tui::Cmd>,
}

impl InputHandler {
    pub fn new(tui_tx: Sender<tui::Cmd>, pads_tx: Sender<audio::Cmd<PAD_COUNT>>) -> Result<Self> {
        Ok(Self {
            clock: 0,
            last_step: None,
            downs: Vec::new(),
            state: State::LoadOnset,
            alt: false,
            hold: false,
            pads_tx,
            tui_tx,
        })
    }

    pub fn push(&mut self, message: &[u8]) -> Result<()> {
        macro_rules! to_fs_at {
            ($paths:expr,$index:expr) => {
                {
                    let mut strings = [const { String::new() }; tui::FILE_COUNT];
                    if !$paths.is_empty() {
                        for i in 0..tui::FILE_COUNT {
                            let index = ($index as isize + i as isize - tui::FILE_COUNT as isize / 2).rem_euclid($paths.len() as isize) as usize;
                            strings[i] = $paths[index]
                                .file_stem()
                                .unwrap()
                                .to_str()
                                .unwrap()
                                .to_string();
                        }
                    }
                    strings
                }
            }
        }
        match LiveEvent::parse(message)? {
            LiveEvent::Midi { message, .. } => match message {
                MidiMessage::NoteOff { key, .. } => match key.as_int() {
                    v if v == KeyCode::Kit as u8 => {
                        self.state = State::LoadOnset;
                        self.tui_tx.send(tui::Cmd::LoadOnset)?;
                    },
                    v if v == KeyCode::Record as u8 => {
                        // take record
                        self.state = State::LoadOnset;
                        self.pads_tx.send(audio::Cmd::TakeRecord(self.downs.first().copied()))?;
                        self.tui_tx.send(tui::Cmd::LoadOnset)?;
                    }
                    v if v == KeyCode::Pool as u8 => {
                        if let State::Pool { cleared: false } = self.state {
                            self.pads_tx.send(audio::Cmd::ClearPool)?;
                        }
                        self.state = State::LoadOnset;
                        self.tui_tx.send(tui::Cmd::LoadOnset)?;
                    }
                    v if v == KeyCode::Hold as u8 => {
                        self.alt = false;
                        self.tui_tx.send(tui::Cmd::Alt(false))?;
                    }
                    v if (KeyCode::PadOffset as u8..KeyCode::PadOffset as u8 + PAD_COUNT as u8).contains(&v) => {
                        let index = v - KeyCode::PadOffset as u8;
                        self.downs.retain(|&v| v != index);
                        self.tui_tx.send(tui::Cmd::Pad(index, false))?;
                        match self.state {
                            State::LoadOnset => if !self.hold {
                                self.handle_pad_input()?;
                            }
                            State::AssignOnset { .. } => self.pads_tx.send(audio::Cmd::ForceSync)?,
                            State::BakeRecord => {
                                let len = if self.downs.len() > 1 {
                                    let index = self.downs[0];
                                    self.downs.iter().skip(1).map(|v| {
                                        v.checked_sub(index + 1).unwrap_or(v + PAD_COUNT as u8 - 1 - index)
                                    })
                                    .fold(0u8, |acc, v| acc | (1 << v)) as u16
                                } else {
                                    audio::MAX_PHRASE_LEN
                                };
                                self.pads_tx.send(audio::Cmd::BakeRecord(len))?;
                                self.tui_tx.send(tui::Cmd::BakeRecord(self.downs.first().copied(), len))?;
                            }
                            _ => (),
                        }
                    }
                    _ => (),
                }
                MidiMessage::NoteOn { key, .. } => match key.as_int() {
                    v if v == KeyCode::FsDec as u8 => match &mut self.state {
                        State::LoadScene { paths, file_index } => {
                            *file_index = (*file_index as isize - 1).rem_euclid(paths.len() as isize) as usize;
                            self.tui_tx.send(tui::Cmd::LoadScene(to_fs_at!(paths, *file_index)))?;
                        }
                        State::LoadWav { paths, file_index } => {
                            *file_index = (*file_index as isize - 1).rem_euclid(paths.len() as isize) as usize;
                            self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, *file_index)))?;
                        }
                        State::AssignOnset { paths, file_index, rd, onset_index } => {
                            *onset_index = (*onset_index as isize - 1).rem_euclid(rd.onsets.len() as isize) as usize;
                            let name = paths[*file_index].file_stem().unwrap().to_str().unwrap().to_string();
                            self.tui_tx.send(tui::Cmd::AssignOnset { name, index: *onset_index, count: rd.onsets.len() })?;
                        }
                        _ => (),
                    }
                    v if v == KeyCode::FsInc as u8 => match &mut self.state {
                        State::LoadScene { paths, file_index } => {
                            *file_index = (*file_index as isize + 1).rem_euclid(paths.len() as isize) as usize;
                            self.tui_tx.send(tui::Cmd::LoadScene(to_fs_at!(paths, *file_index)))?;
                        }
                        State::LoadWav { paths, file_index } => {
                            *file_index = (*file_index as isize + 1).rem_euclid(paths.len() as isize) as usize;
                            self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, *file_index)))?;
                        }
                        State::AssignOnset { paths, file_index, rd, onset_index } => {
                            *onset_index = (*onset_index as isize + 1).rem_euclid(rd.onsets.len() as isize) as usize;
                            let name = paths[*file_index].file_stem().unwrap().to_str().unwrap().to_string();
                            self.tui_tx.send(tui::Cmd::AssignOnset { name, index: *onset_index, count: rd.onsets.len() })?;
                        }
                        _ => (),
                    }
                    v if v == KeyCode::FsInto as u8 => {
                        match &self.state {
                            State::LoadKit => {
                                let paths = (std::fs::read_dir("scenes")?.flat_map(|v| Some(v.ok()?.path().into_boxed_path()))).collect::<Vec<_>>();
                                self.tui_tx.send(tui::Cmd::LoadScene(to_fs_at!(paths, 0)))?;
                                self.state = State::LoadScene {
                                    paths,
                                    file_index: 0,
                                };
                            }
                            State::LoadScene { paths, file_index } => {
                                if paths.is_empty() {
                                    self.state = State::LoadOnset;
                                    self.tui_tx.send(tui::Cmd::LoadOnset)?;
                                } else {
                                    let path = &paths[*file_index];
                                    if path.is_dir() {
                                        let mut paths = if path.parent().unwrap() == PathBuf::from("") {
                                            // in ./scenes; don't include ".."
                                            Vec::new()
                                        } else {
                                            // in subdirectory; include ".."
                                            vec![path.parent().unwrap().into()]
                                        };
                                        paths.extend(std::fs::read_dir(path)?.flat_map(|v| Some(v.ok()?.path().into_boxed_path())));
                                        self.tui_tx.send(tui::Cmd::LoadScene(to_fs_at!(paths, 0)))?;
                                        self.state = State::LoadScene { paths, file_index: 0 };
                                    } else {
                                        let sd_string = std::fs::read_to_string(&paths[*file_index])?;
                                        let scene: audio::pads::Scene<PAD_COUNT> = serde_json::from_str(&sd_string)?;
                                        let tui_scene: [[tui::Pad; PAD_COUNT]; PAD_COUNT] = core::array::from_fn(|i| {
                                            core::array::from_fn(|j| {
                                                let pad = &scene.kits[i].inner[j];
                                                tui::Pad {
                                                    onsets: [pad.onsets[0].is_some(), pad.onsets[1].is_some()],
                                                    phrase: pad.phrase.is_some(),
                                                }
                                            })
                                        });
                                        self.tui_tx.send(tui::Cmd::AssignScene(Box::new(tui_scene)))?;
                                        self.pads_tx.send(audio::Cmd::LoadScene(Box::new(scene)))?;
                                    }
                                }
                            }
                            State::LoadWav { paths, file_index } => {
                                if paths.is_empty() {
                                    self.state = State::LoadOnset;
                                    self.tui_tx.send(tui::Cmd::LoadOnset)?;
                                } else {
                                    let path = &paths[*file_index];
                                    if path.is_dir() {
                                        let mut paths = if path.parent().unwrap() == PathBuf::from("") {
                                            // in ./onsets; don't include ".."
                                            Vec::new()
                                        } else {
                                            // in subdirectory; include ".."
                                            vec![path.parent().unwrap().into()]
                                        };
                                        paths.extend(std::fs::read_dir(path)?.flat_map(|v| Some(v.ok()?.path().into_boxed_path())));
                                        self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, 0)))?;
                                        self.state = State::LoadWav { paths, file_index: 0 };
                                    } else {
                                        let rd_string = std::fs::read_to_string(path.with_extension("rd"))?;
                                        let rd: audio::Rd = serde_json::from_str(&rd_string)?;
                                        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
                                        self.tui_tx.send(tui::Cmd::AssignOnset { name, index: 0, count: rd.onsets.len() })?;
                                        self.state = State::AssignOnset {
                                            paths: paths.clone(),
                                            file_index: *file_index,
                                            rd,
                                            onset_index: 0,
                                        };
                                    }
                                }
                            }
                            State::AssignOnset { paths, file_index, .. } => {
                                self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, *file_index)))?;
                                self.state = State::LoadWav { paths: paths.clone(), file_index: 0 };
                            }
                            _ => {
                                let paths = (std::fs::read_dir("onsets")?.flat_map(|v| Some(v.ok()?.path().into_boxed_path()))).collect::<Vec<_>>();
                                self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, 0)))?;
                                self.state = State::LoadWav {
                                    paths,
                                    file_index: 0,
                                };
                            }
                        }
                    }
                    v if v == KeyCode::Kit as u8 => {
                        self.state = State::LoadKit;
                        self.tui_tx.send(tui::Cmd::LoadKit(None))?;
                    }
                    v if v == KeyCode::Record as u8 => {
                        match self.state {
                            State::LoadKit => {
                                self.state = State::AssignKit;
                                self.tui_tx.send(tui::Cmd::AssignKit(None))?;
                            }
                            _ => {
                                self.state = State::BakeRecord;
                                self.hold = false;
                                if self.downs.is_empty() {
                                    self.pads_tx.send(audio::Cmd::Input(audio::Event::Sync))?;
                                }
                                self.pads_tx.send(audio::Cmd::BakeRecord(audio::MAX_PHRASE_LEN))?;
                                self.tui_tx.send(tui::Cmd::BakeRecord(None, audio::MAX_PHRASE_LEN))?;
                            }
                        }
                    }
                    v if v == KeyCode::Pool as u8 => {
                        match self.state {
                            State::LoadKit => {
                                let mut index = 0;
                                let mut file = std::fs::File::create_new(format!("scenes/scene{}.sd", index));
                                while file.is_err() {
                                    index += 1;
                                    file = std::fs::File::create_new(format!("scenes/scene{}.sd", index));
                                }
                                self.pads_tx.send(audio::Cmd::SaveScene(file?))?;
                            }
                            _ => {
                                self.state = State::Pool { cleared: false };
                                self.tui_tx.send(tui::Cmd::Pool)?;
                            }
                        }
                    }
                    v if v == KeyCode::Hold as u8 => {
                        if let State::LoadOnset = self.state {
                            self.hold = !self.hold;
                            if !self.hold && self.downs.is_empty() {
                                self.pads_tx.send(audio::Cmd::Input(audio::Event::Sync))?;
                            }
                            self.tui_tx.send(tui::Cmd::Hold(self.hold))?;
                        }
                        self.alt = true;
                        self.tui_tx.send(tui::Cmd::Alt(true))?;
                    }
                    v if (KeyCode::PadOffset as u8..KeyCode::PadOffset as u8 + PAD_COUNT as u8).contains(&v) => {
                        let index = v - KeyCode::PadOffset as u8;
                        self.downs.push(index);
                        self.tui_tx.send(tui::Cmd::Pad(index, true))?;
                        match &self.state {
                            State::LoadOnset => self.handle_pad_input()?,
                            State::LoadKit => {
                                self.pads_tx.send(audio::Cmd::LoadKit(index))?;
                                self.tui_tx.send(tui::Cmd::LoadKit(Some(index)))?;
                            }
                            State::AssignKit => {
                                self.pads_tx.send(audio::Cmd::AssignKit(index))?;
                                self.tui_tx.send(tui::Cmd::AssignKit(Some(index)))?;
                            }
                            State::LoadScene { .. } => {
                                self.state = State::LoadOnset;
                                self.tui_tx.send(tui::Cmd::LoadOnset)?;
                            }
                            State::LoadWav { .. } => {
                                self.state = State::LoadOnset;
                                self.tui_tx.send(tui::Cmd::LoadOnset)?;
                            }
                            State::AssignOnset { paths, file_index, rd, onset_index } => {
                                let len = std::fs::metadata(&paths[*file_index])?.len() - 44;
                                let start = rd.onsets[*onset_index];
                                let wav = audio::Wav {
                                    tempo: rd.tempo,
                                    steps: rd.steps,
                                    path: paths[*file_index].clone(),
                                    len,
                                };
                                let onset = audio::Onset { wav, start };
                                self.pads_tx.send(audio::Cmd::AssignOnset(index, self.alt, Box::new(onset)))?;
                            }
                            State::BakeRecord => {
                                let len = if self.downs.len() > 1 {
                                    let index = self.downs[0];
                                    self.downs.iter().skip(1).map(|v| {
                                        v.checked_sub(index + 1).unwrap_or(v + PAD_COUNT as u8 - 1 - index)
                                    })
                                    .fold(0u8, |acc, v| acc | (1 << v)) as u16
                                } else {
                                    audio::MAX_PHRASE_LEN
                                };
                                self.pads_tx.send(audio::Cmd::BakeRecord(len))?;
                                self.tui_tx.send(tui::Cmd::BakeRecord(self.downs.first().copied(), len))?;
                            }
                            State::Pool { cleared } => {
                                if !cleared {
                                    self.state = State::Pool { cleared: true };
                                    self.pads_tx.send(audio::Cmd::ClearPool)?;
                                }
                                self.pads_tx.send(audio::Cmd::PushPool(index))?;
                            }
                        }
                    }
                    _ => (),
                }
                MidiMessage::Controller { controller, value } => match controller.as_int() {
                    v if v == CtrlCode::Speed as u8 => {
                        self.pads_tx.send(audio::Cmd::AssignSpeed(value.as_int() as f32 / 127. * 2.))?;
                    }
                    v if v == CtrlCode::Drift as u8 => {
                        self.pads_tx.send(audio::Cmd::AssignDrift(value.as_int() as f32 / 127.))?;
                        self.tui_tx.send(tui::Cmd::AssignDrift(value.as_int()))?;
                    }
                    v if v == CtrlCode::Bias as u8 => {
                        self.pads_tx.send(audio::Cmd::AssignBias(value.as_int() as f32 / 127.))?;
                        self.tui_tx.send(tui::Cmd::AssignBias(value.as_int()))?;
                    }
                    v if v == CtrlCode::Width as u8 => {
                        self.pads_tx.send(audio::Cmd::AssignWidth(value.as_int() as f32 / 127.))?;
                    }
                    _ => (),
                }
                MidiMessage::PitchBend { bend } => {
                    self.pads_tx.send(audio::Cmd::OffsetSpeed(bend.as_f32() + 1.))?;
                }
                _ => (),
            }
            LiveEvent::Realtime(midly::live::SystemRealtime::TimingClock) => {
                if self.clock == 0 {
                    let now = std::time::Instant::now();
                    if let Some(delta) = self.last_step {
                        let ioi = now.duration_since(delta);
                        let tempo = 60. / ioi.as_secs_f32() / audio::STEP_DIV as f32;
                        self.pads_tx.send(audio::Cmd::AssignTempo(tempo))?;
                    }
                    self.last_step = Some(now);
                    self.pads_tx.send(audio::Cmd::Clock)?;
                    self.tui_tx.send(tui::Cmd::Clock)?;
                }
                self.clock = (self.clock + 1) % (audio::PPQ / audio::STEP_DIV);
            }
            LiveEvent::Realtime(midly::live::SystemRealtime::Stop) => {
                self.last_step = None;
                self.clock = 0;
                self.pads_tx.send(audio::Cmd::Stop)?;
                self.tui_tx.send(tui::Cmd::Stop)?;
            }
            _ => (),
        }
        Ok(())
    }

    fn handle_pad_input(&mut self) -> Result<()> {
        if let Some(&index) = self.downs.first() {
            if self.downs.len() > 1 {
                // init loop start
                let numerator = self.downs.iter().skip(1).map(|v| {
                    v.checked_sub(index + 1).unwrap_or(v + PAD_COUNT as u8 - 1 - index)
                })
                .fold(0u8, |acc, v| acc | (1 << v));
                let len = audio::Fraction::new(numerator, audio::LOOP_DIV);
                self.pads_tx.send(audio::Cmd::Input(audio::Event::Loop { index, len }))?;
            } else {
                // init loop stop || jump
                self.pads_tx.send(audio::Cmd::Input(audio::Event::Hold { index }))?;
            }
        } else {
            // init sync
            self.pads_tx.send(audio::Cmd::Input(audio::Event::Sync))?;
        }
        Ok(())
    }
}
