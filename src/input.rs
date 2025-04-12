use crate::{audio, tui};
use audio::PAD_COUNT;

use color_eyre::Result;
use midly::{live::LiveEvent, MidiMessage};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::Sender,
};

const KEY_FS_DEC: u8 = 57;
const KEY_FS_INTO: u8 = 58;
const KEY_FS_INC: u8 = 59;

const KEY_RECORD: u8 = 60;
const KEY_POOL: u8 = 62;
const KEY_HOLD: u8 = 64;

const PAD_OFFSET: u8 = 65;

const CTRL_SPEED: u8 = 105;
const CTRL_DRIFT: u8 = 106;
const CTRL_BIAS: u8 = 29;
const CTRL_WIDTH: u8 = 26;

enum State {
    Pads,
    Fs {
        /// all paths in dir
        paths: Vec<Box<Path>>,
        /// current file index
        index: usize,
    },
    AssignOnset {
        /// audio filepath
        path: Box<Path>,
        /// file index in parent dir
        index: usize,
        rd: audio::Rd,
        onset: usize,
    },
    BakeRecord,
    Pool(bool),
}

pub struct InputHandler {
    clock: u8,
    last_step: Option<std::time::Instant>,

    alt: bool,
    hold: bool,
    downs: Vec<u8>,
    
    state: State,
    pads_tx: Sender<audio::Cmd>,
    tui_tx: Sender<tui::Cmd>,
}

enum FsCmd {
    Dec,
    Inc,
    Into,
}

impl InputHandler {
    pub fn new(tui_tx: Sender<tui::Cmd>, pads_tx: Sender<audio::Cmd>) -> Result<Self> {
        Ok(Self {
            clock: 0,
            last_step: None,
            downs: Vec::new(),
            state: State::Pads,
            alt: false,
            hold: false,
            pads_tx,
            tui_tx,
        })
    }

    pub fn push(&mut self, message: &[u8]) -> Result<()> {
        match LiveEvent::parse(message)? {
            LiveEvent::Midi { message, .. } => {
                match message {
                    MidiMessage::NoteOff { key, .. } => match key.as_int() {
                        KEY_RECORD => {
                            // take record
                            self.state = State::Pads;
                            // FIXME: hold reversion for lower pool support via alt
                            self.pads_tx.send(audio::Cmd::TakeRecord(self.downs.first().copied()))?;
                            self.tui_tx.send(tui::Cmd::Clear)?;
                        }
                        KEY_POOL => {
                            if let State::Pool(false) = self.state {
                                self.pads_tx.send(audio::Cmd::ClearPool)?;
                            }
                            self.state = State::Pads;
                            self.tui_tx.send(tui::Cmd::Clear)?;
                        }
                        KEY_HOLD => {
                            self.alt = false;
                            self.tui_tx.send(tui::Cmd::Alt(false))?;
                        }
                        key if (PAD_OFFSET..PAD_OFFSET + PAD_COUNT as u8).contains(&key) => {
                            // handle_pad_up
                            let index = key - PAD_OFFSET;
                            self.downs.retain(|&v| v != index);
                            self.tui_tx.send(tui::Cmd::Pad(index, false))?;
                            match self.state {
                                State::Pads | State::AssignOnset { .. } => if !self.hold {
                                    self.handle_pad_input()?;
                                }
                                State::Fs { .. } => {
                                    self.state = State::Pads;
                                    self.tui_tx.send(tui::Cmd::Clear)?;
                                    if !self.hold {
                                        self.handle_pad_input()?;
                                    }
                                }
                                _ => (),
                            }
                        }
                        _ => (),
                    }
                    MidiMessage::NoteOn { key, .. } => match key.as_int() {
                        KEY_FS_DEC => self.handle_fs_event(FsCmd::Dec)?,
                        KEY_FS_INTO => self.handle_fs_event(FsCmd::Into)?,
                        KEY_FS_INC => self.handle_fs_event(FsCmd::Inc)?,
                        KEY_RECORD => {
                            // bake record over max len
                            self.state = State::BakeRecord;
                            self.pads_tx.send(audio::Cmd::BakeRecord(2u16.pow(PAD_COUNT as u32 - 1)))?;
                            self.tui_tx.send(tui::Cmd::BakeRecord(None, 2u16.pow(PAD_COUNT as u32 - 1)))?;
                        }
                        KEY_POOL => {
                            self.state = State::Pool(false);
                            self.tui_tx.send(tui::Cmd::Pool)?;
                        }
                        KEY_HOLD => {
                            self.alt = true;
                            self.tui_tx.send(tui::Cmd::Alt(true))?;
                            self.hold = !self.hold;
                            self.tui_tx.send(tui::Cmd::Hold(self.hold))?;
                        }
                        key if (PAD_OFFSET..PAD_OFFSET + PAD_COUNT as u8).contains(&key) => {
                            // handle pad down
                            let index = key - PAD_OFFSET;
                            self.downs.push(index);
                            self.tui_tx.send(tui::Cmd::Pad(index, true))?;
                            match self.state {
                                State::Pads => self.handle_pad_input()?,
                                State::Fs { .. } => {
                                    self.state = State::Pads;
                                    self.tui_tx.send(tui::Cmd::Clear)?;
                                    self.handle_pad_input()?;
                                }
                                State::AssignOnset { .. } => {
                                    if let State::AssignOnset {
                                        ref path,
                                        ref rd,
                                        ref onset,
                                        ..
                                    } = self.state {
                                        let len = std::fs::metadata(path)?.len() - 44;
                                        let start = rd.onsets[*onset];
                                        let wav = audio::Wav {
                                            rd: rd.clone(),
                                            path: path.clone(),
                                            len,
                                        };
                                        let onset = audio::Onset { wav, start };
                                        self.pads_tx.send(audio::Cmd::AssignOnset(index, self.alt, onset))?;
                                    }
                                }
                                State::BakeRecord { .. } => {
                                    if self.downs.len() > 1 {
                                        let index = self.downs[0];
                                        let len = self.downs.iter().skip(1).map(|v| {
                                            v.checked_sub(index + 1).unwrap_or(v + PAD_COUNT as u8 - 1 - index)
                                        })
                                        .fold(0u8, |acc, v| acc | (1 << v)) as u16;
                                        self.pads_tx.send(audio::Cmd::BakeRecord(len))?;
                                        self.tui_tx.send(tui::Cmd::BakeRecord(self.downs.first().copied(), len))?;
                                    }
                                }
                                State::Pool(ref mut cleared) => {
                                    if !*cleared {
                                        *cleared = true;
                                        self.pads_tx.send(audio::Cmd::ClearPool)?;
                                    }
                                    self.pads_tx.send(audio::Cmd::PushPool(index))?;
                                }
                            }
                        }
                        _ => (),
                    }
                    MidiMessage::Controller { controller, value } => match controller.as_int() {
                        CTRL_BIAS => {
                            self.pads_tx.send(audio::Cmd::AssignBias(value.as_int() as f32 / 127.))?;
                            self.tui_tx.send(tui::Cmd::AssignBias(value.as_int()))?;
                        }
                        CTRL_DRIFT => {
                            self.pads_tx.send(audio::Cmd::AssignDrift(value.as_int() as f32 / 127.))?;
                            self.tui_tx.send(tui::Cmd::AssignDrift(value.as_int()))?;
                        }
                        CTRL_SPEED => self.pads_tx.send(audio::Cmd::AssignSpeed(value.as_int() as f32 / 127. * 2.))?,
                        CTRL_WIDTH => self.pads_tx.send(audio::Cmd::AssignWidth(value.as_int() as f32 / 127.))?,
                        _ => (),
                    }
                    MidiMessage::PitchBend { bend } => {
                        self.pads_tx.send(audio::Cmd::OffsetSpeed(bend.as_f32() + 1.))?;
                    }
                    _ => (),
                }
            }
            LiveEvent::Realtime(midly::live::SystemRealtime::Start) => {
                self.last_step = None;
                self.clock = 0;
                self.pads_tx.send(audio::Cmd::Start)?;
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

    fn handle_fs_event(&mut self, cmd: FsCmd) -> Result<()> {
        macro_rules! send_fs {
            ($dir:expr,$index:expr) => {
                {
                    // FIXME: check both rd and wav exist, or handle when wav only
                    let mut paths = std::fs::read_dir($dir)?
                        .flat_map(|v| Some(v.ok()?.path().into_boxed_path()))
                        .filter(|v| v.extension().unwrap() == "wav" || v.is_dir())
                        .collect::<Vec<_>>();
                    paths.sort();
                    resend_fs!(paths, $index);
                    paths
                }
            }
        }
        macro_rules! resend_fs {
            ($paths:expr,$index:expr) => {
                if $paths.is_empty() {
                    self.tui_tx.send(tui::Cmd::Fs(None))?;
                } else {
                    let mut strings = [const { String::new() }; tui::FILE_COUNT];
                    for i in 0..tui::FILE_COUNT {
                        let index = ($index as isize + i as isize - tui::FILE_COUNT as isize / 2).rem_euclid($paths.len() as isize) as usize;
                        strings[i] = $paths[index]
                            .file_stem()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_string();
                    }
                    self.tui_tx.send(tui::Cmd::Fs(Some(strings)))?;
                }
            }
        }
        if let State::Fs { ref mut paths, ref mut index } = self.state {
            match cmd {
                FsCmd::Dec => {
                    *index = (*index as isize - 1).rem_euclid(paths.len() as isize) as usize;
                    resend_fs!(paths, *index);
                }
                FsCmd::Inc => {
                    *index = (*index as isize + 1).rem_euclid(paths.len() as isize) as usize;
                    resend_fs!(paths, *index);
                }
                FsCmd::Into => {
                    if !paths.is_empty() {
                        let path = paths[*index].clone();
                        if path.is_dir() {
                            // into dir (maybe parent)
                            *paths = if path.parent().unwrap() == PathBuf::from("") {
                                // in assets; don't include ".."
                                Vec::new()
                            } else {
                                // in subdirectory; include ".."
                                vec![path.parent().unwrap().into()]
                            };
                            paths.extend(send_fs!(path, 0));
                            *index = 0;
                        } else {
                            // into audio file
                            let rd_string = std::fs::read_to_string(path.with_extension("rd"))?;
                            let rd: audio::Rd = miniserde::json::from_str(&rd_string)?;
                            let name = path.file_stem().unwrap().to_str().unwrap().to_string();
                            self.tui_tx.send(tui::Cmd::AssignOnset {
                                name,
                                index: 1,
                                count: rd.onsets.len(),
                            })?;
                            // treat hold as alt while AssignOnset
                            self.hold = false;
                            self.tui_tx.send(tui::Cmd::Hold(false))?;
                            self.state = State::AssignOnset {
                                path,
                                index: *index,
                                rd,
                                onset: 0,
                            };
                        }
                    }
                }
            }
        } else if let State::AssignOnset { ref path, ref mut rd, index, ref mut onset } = self.state {
            match cmd {
                FsCmd::Dec => {
                    *onset = (*onset as isize - 1).rem_euclid(rd.onsets.len() as isize) as usize;
                    let name = path.file_stem().unwrap().to_str().unwrap().to_string();
                    self.tui_tx.send(tui::Cmd::AssignOnset {
                        name,
                        index: *onset + 1,
                        count: rd.onsets.len(),
                    })?;
                }
                FsCmd::Inc => {
                    *onset = (*onset + 1).rem_euclid(rd.onsets.len());
                    let name = path.file_stem().unwrap().to_str().unwrap().to_string();
                    self.tui_tx.send(tui::Cmd::AssignOnset {
                        name,
                        index: *onset + 1,
                        count: rd.onsets.len(),
                    })?;
                }
                FsCmd::Into => {
                    let paths = send_fs!(path.parent().unwrap(), index);
                    self.state = State::Fs {
                        paths,
                        index,
                    }
                }
            }
        } else {
            let paths = send_fs!("assets", 0);
            self.state = State::Fs { paths, index: 0 };
        }
        Ok(())
    }
}
