use crate::{audio, tui};
use audio::PAD_COUNT;

use color_eyre::Result;
use midly::{live::LiveEvent, MidiMessage};
use std::{
    path::{Path, PathBuf},
    sync::mpsc::Sender,
};

const KEY_ALT: u8 = 60;
const KEY_FS_INC: u8 = 61;
const KEY_GHOST: u8 = 62;
const KEY_FS_INTO: u8 = 63;
const KEY_RECORD: u8 = 64;
const PAD_OFFSET: u8 = 65;
const CTRL_BIAS: u8 = 105;
const CTRL_ROLL: u8 = 106;
const CTRL_DRIFT: u8 = 29;
const CTRL_WIDTH: u8 = 26;

/// global pot variant
#[derive(Copy, Clone)]
pub enum Global {
    Bias(u8),
    Roll(u8),
    Drift(u8),
    Width(u8),
}

enum State {
    Onset,
    Dir {
        /// all paths in dir
        paths: Vec<Box<Path>>,
        /// current file index
        index: usize,
    },
    File {
        /// audio filepath
        path: Box<Path>,
        /// file index in parent dir
        index: usize,
        rd: audio::Rd,
        onset: usize,
    },
    Phrase,
    Ghost(bool),
    Sequence(bool),
}

struct Alt {
    logic: bool,
    input: bool,
}

pub struct InputHandler {
    tui_tx: Sender<tui::Cmd>,
    audio_tx: Sender<audio::Cmd>,
    downs: Vec<u8>,
    last_downs_len: u8,
    state: State,
    recording: bool,
    alt: Alt,
}

enum FsCmd {
    Dec,
    Inc,
    Into,
}

impl InputHandler {
    pub fn new(tui_tx: Sender<tui::Cmd>, audio_tx: Sender<audio::Cmd>) -> Result<Self> {
        Ok(Self {
            tui_tx,
            audio_tx,
            downs: Vec::new(),
            last_downs_len: 0,
            state: State::Onset,
            recording: false,
            alt: Alt {
                logic: false,
                input: false,
            },
        })
    }

    pub fn push(&mut self, message: &[u8]) -> Result<()> {
        match LiveEvent::parse(message)? {
            LiveEvent::Midi { message, .. } => {
                match message {
                    MidiMessage::Controller { controller, value } => match controller.as_int() {
                        CTRL_BIAS => self.handle_global_event(Global::Bias(value.as_int()))?,
                        CTRL_ROLL => self.handle_global_event(Global::Roll(value.as_int()))?,
                        CTRL_DRIFT => self.handle_global_event(Global::Drift(value.as_int()))?,
                        CTRL_WIDTH => self.handle_global_event(Global::Width(value.as_int()))?,
                        _ => (),
                    },
                    MidiMessage::NoteOff { key, .. } => match key.as_int() {
                        KEY_ALT => self.handle_alt_input(false),
                        KEY_GHOST => {
                            if self.alt.logic {
                                self.handle_sequence_event(false)?
                            } else {
                                self.handle_ghost_event(false)?
                            }
                        }
                        key if (PAD_OFFSET..PAD_OFFSET + PAD_COUNT as u8).contains(&key) => {
                            self.handle_pad_event(key - PAD_OFFSET, false)?;
                        }
                        _ => (),
                    },
                    MidiMessage::NoteOn { key, .. } => match key.as_int() {
                        KEY_ALT => self.handle_alt_input(true),
                        KEY_FS_INC => {
                            if self.alt.logic {
                                self.handle_fs_event(FsCmd::Dec)?
                            } else {
                                self.handle_fs_event(FsCmd::Inc)?
                            }
                        }
                        KEY_FS_INTO => self.handle_fs_event(FsCmd::Into)?,
                        KEY_GHOST => {
                            if self.alt.logic {
                                self.handle_sequence_event(true)?
                            } else {
                                self.handle_ghost_event(true)?
                            }
                        }
                        KEY_RECORD => self.handle_record_event()?,
                        key if (PAD_OFFSET..PAD_OFFSET + PAD_COUNT as u8).contains(&key) => {
                            self.handle_pad_event(key - PAD_OFFSET, true)?;
                        }
                        _ => (),
                    },
                    _ => (),
                }
                self.handle_alt_logic()?;
            }
            LiveEvent::Realtime(midly::live::SystemRealtime::TimingClock) => {
                todo!("tempo sync")
            }
            _ => (),
        }
        Ok(())
    }

    fn handle_pad_event(&mut self, index: u8, down: bool) -> Result<()> {
        self.tui_tx.send(tui::Cmd::Pad(index, down))?;
        if down {
            self.downs.push(index);

            let mut onset = false;
            match self.state {
                State::Onset => onset = true,
                State::Dir { .. } => {
                    onset = true;
                    self.state = State::Onset;
                }
                State::File { .. } => self.handle_pad_file(index)?,
                State::Phrase => self.handle_pad_phrase(index)?,
                State::Ghost(_) => self.handle_pad_ghost(index)?,
                State::Sequence(_) => self.handle_pad_sequence(index)?,
            }
            if !onset {
                return Ok(());
            }
        } else {
            self.downs.retain(|&v| v != index);

            if matches!(self.state, State::Phrase) {
                self.state = State::Onset;
            }
        }
        self.handle_pad_input()
    }

    fn handle_pad_input(&mut self) -> Result<()> {
        let mut input = audio::Input::Sync;
        if let Some(&index) = self.downs.first() {
            if self.downs.len() > 1 {
                // init loop start
                let numerator = self
                    .downs
                    .iter()
                    .skip(1)
                    .map(|v| {
                        v.checked_sub(index + 1)
                            .unwrap_or(v + PAD_COUNT as u8 - 1 - index)
                    })
                    .fold(0u8, |acc, v| acc | (1 << v));
                let len = audio::Fraction::new(numerator, audio::STEP_DIVISOR);
                input = audio::Input::Loop { index, len };
            } else if self.last_downs_len > 1 {
                // init loop stop
            } else {
                // init jump
                input = audio::Input::Hold { index };
            }
        } else {
            // init sync
        }
        self.last_downs_len = self.downs.len() as u8;

        self.audio_tx.send(audio::Cmd::Input(input))?;
        Ok(())
    }

    fn handle_pad_file(&mut self, index: u8) -> Result<()> {
        if let State::File {
            ref path,
            ref rd,
            ref onset,
            ..
        } = self.state
        {
            let len = std::fs::metadata(path)?.len() - 44;
            let start = rd.onsets[*onset];
            let end = rd.onsets.get(*onset + 1).copied().unwrap_or(len);
            let steps = ((end - start) as f32 * rd.steps as f32 / len as f32).round() as usize;
            let wav = audio::Wav {
                rd: rd.clone(),
                path: path.clone(),
                len,
            };
            let onset = audio::Onset { wav, start, steps };
            self.audio_tx
                .send(audio::Cmd::AssignOnset(index, self.alt.logic, onset))?;
        }
        Ok(())
    }

    fn handle_pad_phrase(&mut self, index: u8) -> Result<()> {
        self.audio_tx.send(audio::Cmd::AssignPhrase(index))?;
        Ok(())
    }

    fn handle_pad_ghost(&mut self, index: u8) -> Result<()> {
        if let State::Ghost(ref mut cleared) = self.state {
            if !*cleared {
                self.audio_tx.send(audio::Cmd::ClearGhost)?;
                *cleared = true;
            }
            self.audio_tx.send(audio::Cmd::PushGhost(index))?;
        }
        Ok(())
    }

    fn handle_pad_sequence(&mut self, index: u8) -> Result<()> {
        if let State::Sequence(ref mut cleared) = self.state {
            if !*cleared {
                self.audio_tx.send(audio::Cmd::ClearSequence)?;
                *cleared = true;
            }
            self.audio_tx.send(audio::Cmd::PushSequence(index))?;
        }
        Ok(())
    }

    fn handle_ghost_event(&mut self, down: bool) -> Result<()> {
        if down {
            if let State::Sequence(false) = self.state {
                self.audio_tx.send(audio::Cmd::ClearSequence)?;
            }
            self.state = State::Ghost(false);
        } else {
            if let State::Ghost(false) = self.state {
                self.audio_tx.send(audio::Cmd::ClearGhost)?;
            }
            self.state = State::Onset;
        }
        self.tui_tx.send(tui::Cmd::Ghost(down))?;
        Ok(())
    }

    fn handle_sequence_event(&mut self, down: bool) -> Result<()> {
        if down {
            if let State::Ghost(false) = self.state {
                self.audio_tx.send(audio::Cmd::ClearGhost)?;
            }
            self.state = State::Sequence(false);
        } else {
            if let State::Sequence(false) = self.state {
                self.audio_tx.send(audio::Cmd::ClearSequence)?;
            }
            self.state = State::Onset;
        }
        self.tui_tx.send(tui::Cmd::Sequence(down))?;
        Ok(())
    }

    fn handle_record_event(&mut self) -> Result<()> {
        self.recording = !self.recording;
        if self.recording {
            self.tui_tx.send(tui::Cmd::Record)?;
        } else {
            self.state = State::Phrase;
            self.tui_tx.send(tui::Cmd::Phrase)?;
        }
        self.audio_tx.send(audio::Cmd::Record(self.recording))?;
        Ok(())
    }

    fn handle_fs_event(&mut self, cmd: FsCmd) -> Result<()> {
        macro_rules! send_fs {
            ($dir:expr) => {{
                // eprintln!("FIXME: check both rd and wav exist, or handle when wav only");
                let mut paths = std::fs::read_dir($dir)?
                    .flat_map(|v| Some(v.ok()?.path().into_boxed_path()))
                    .filter(|v| v.extension().unwrap() == "wav" || v.is_dir())
                    .collect::<Vec<_>>();
                paths.sort();
                resend_fs!(paths, 0);
                paths
            }};
        }
        macro_rules! resend_fs {
            ($paths:expr,$index:expr) => {
                if $paths.is_empty() {
                    self.tui_tx.send(tui::Cmd::Dir(None))?;
                } else {
                    let mut strings = [const { String::new() }; tui::FILE_COUNT];
                    for i in 0..tui::FILE_COUNT {
                        let index = (($index as isize + i as isize - tui::FILE_COUNT as isize / 2)
                            % $paths.len() as isize) as usize;
                        strings[i] = $paths[index]
                            .file_stem()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_string();
                    }
                    self.tui_tx.send(tui::Cmd::Dir(Some(strings)))?;
                }
            };
        }

        if let State::Dir {
            ref mut paths,
            ref mut index,
        } = self.state
        {
            match cmd {
                FsCmd::Dec => {
                    *index = ((*index as isize - 1) % paths.len() as isize) as usize;
                    resend_fs!(paths, *index);
                }
                FsCmd::Inc => {
                    *index = ((*index as isize + 1) % paths.len() as isize) as usize;
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
                            paths.extend(send_fs!(path));
                            *index = 0;
                        } else {
                            // into audio file
                            let rd_string = std::fs::read_to_string(path.with_extension("rd"))?;
                            let rd: audio::Rd = miniserde::json::from_str(&rd_string)?;
                            let name = path.file_stem().unwrap().to_str().unwrap().to_string();
                            self.tui_tx.send(tui::Cmd::File {
                                name,
                                index: 1,
                                count: rd.onsets.len(),
                            })?;
                            self.state = State::File {
                                path,
                                index: *index,
                                rd,
                                onset: 0,
                            };
                        }
                    }
                }
            }
        } else if let State::File {
            ref path,
            ref mut rd,
            ref index,
            ref mut onset,
        } = self.state
        {
            match cmd {
                FsCmd::Dec => {
                    *onset = (*onset as isize - 1).rem_euclid(rd.onsets.len() as isize) as usize;
                    let name = path.file_stem().unwrap().to_str().unwrap().to_string();
                    self.tui_tx.send(tui::Cmd::File {
                        name,
                        index: *onset + 1,
                        count: rd.onsets.len(),
                    })?;
                }
                FsCmd::Inc => {
                    *onset = (*onset + 1) % rd.onsets.len();
                    let name = path.file_stem().unwrap().to_str().unwrap().to_string();
                    self.tui_tx.send(tui::Cmd::File {
                        name,
                        index: *onset + 1,
                        count: rd.onsets.len(),
                    })?;
                }
                FsCmd::Into => {
                    let paths = send_fs!(path.parent().unwrap());
                    self.state = State::Dir {
                        paths,
                        index: *index,
                    }
                }
            }
        } else {
            let paths = send_fs!("assets");
            match self.state {
                State::Ghost(false) => self.audio_tx.send(audio::Cmd::ClearGhost)?,
                State::Sequence(false) => self.audio_tx.send(audio::Cmd::ClearSequence)?,
                _ => {
                    self.state = State::Dir { paths, index: 0 };
                }
            }
        }
        Ok(())
    }

    fn handle_alt_logic(&mut self) -> Result<()> {
        match self.state {
            State::Ghost(..) | State::Sequence(..) => (),
            _ => {
                if self.alt.logic != self.alt.input {
                    self.alt.logic = self.alt.input;
                    self.tui_tx.send(tui::Cmd::Alt(self.alt.logic))?;
                }
            }
        }
        Ok(())
    }

    fn handle_alt_input(&mut self, down: bool) {
        self.alt.input = down;
    }

    fn handle_global_event(&mut self, global: Global) -> Result<()> {
        self.audio_tx.send(audio::Cmd::AssignGlobal(global))?;
        self.tui_tx.send(tui::Cmd::Global(global))?;
        Ok(())
    }
}
