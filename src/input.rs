use crate::{audio, tui};
use audio::PAD_COUNT;

use color_eyre::Result;
use midly::{live::LiveEvent, MidiMessage};
use std::{path::{Path, PathBuf}, sync::mpsc::Sender};

#[derive(Copy, Clone)]
pub enum Bank {
    A,
    B,
}

/**
    chords:
        Record\*: bake phrase \*
            first Pad\*: assign phrase to pad
            more Pad\*s: phrase len
            release Record\*: take phrase, assign if first held
        Kit\* + Pad\*: load pad's kit
        Kit\* + Record\* + Pad\*: assign *'s pads to pad's kit
        Pool\* + Pad\* + ...: build phrase pool from pads'
        Reverse\*: reverse \* playback
        Reverse\* + Pool\*: toggle \* hold

        Shift + KitA: open onset fs
            RecordA: decrement
            PoolA: increment
            KitA: into wav/dir
            in wav:
                RecordA: decrement
                PoolA: increment
                Pad\*: assign to first Pad\* onset
                ReverseA + Pad\*: assign to second Pad\* onset
                KitA: exit wav
            release Shift: exit fs
        Shift + PoolA: opene scene fs
            RecordA: decrement
            PoolA: increment
            KitA: load scene / into dir
            release Shift: exit fs
        Shift + RecordA: save active scene to new .sd
*/
enum KeyCode {
    Global = 48,
    
    RecordA = 49,
    KitA = 50,
    PoolA = 51,
    ReverseA = 52,
    BankAOffset = 53,
    
    RecordB = 61,
    KitB = 62,
    PoolB = 63,
    ReverseB = 64,
    BankBOffset = 65,
}

enum CtrlCode {
    Blend = 83,

    SpeedA = 105,
    DriftA = 106,
    BiasA = 29,
    WidthA = 26,
    
    SpeedB = 102,
    DriftB = 103,
    BiasB = 28,
    WidthB = 24,
}

enum GlobalState {
    Yield,
    Prime,
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
        alt: bool,
    }
}

enum BankState {
    LoadOnset,
    LoadKit,
    AssignKit,
    BakeRecord,
    BuildPool { cleared: bool },
}

struct BankHandler {
    bank: Bank,
    hold: bool,
    reverse: bool,
    state: BankState,
    downs: Vec<u8>,
}

macro_rules! audio_bank_cmd {
    ($bank:expr,$cmd:ident) => {
        audio::Cmd::Bank($bank, audio::BankCmd::$cmd)
    };
    ($bank:expr,$cmd:ident,$($params:tt)*) => {
        audio::Cmd::Bank($bank, audio::BankCmd::$cmd($($params)*))
    };
}

macro_rules! tui_bank_cmd {
    ($bank:expr,$cmd:ident) => {
        tui::Cmd::Bank($bank, tui::BankCmd::$cmd)
    };
    ($bank:expr,$cmd:ident,$($params:tt)*) => {
        tui::Cmd::Bank($bank, tui::BankCmd::$cmd($($params)*))
    };
}

impl BankHandler {
    fn new(bank: Bank) -> Self {
        Self {
            bank,
            hold: false,
            reverse: false,
            downs: Vec::new(),
            state: BankState::LoadOnset,
        }
    }

    fn handle_record_up<const N: usize>(&mut self, pads_tx: &mut Sender<audio::Cmd<N>>, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        self.state = BankState::LoadOnset;
        pads_tx.send(audio_bank_cmd!(self.bank, TakeRecord, self.downs.first().copied()))?;
        tui_tx.send(tui_bank_cmd!(self.bank, LoadOnset))?;
        Ok(())
    }

    fn handle_record_down<const N: usize>(&mut self, pads_tx: &mut Sender<audio::Cmd<N>>, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        if let BankState::LoadKit = self.state {
            self.state = BankState::AssignKit;
            tui_tx.send(tui_bank_cmd!(self.bank, AssignKit, None))?;
        } else {
            self.state = BankState::BakeRecord;
            self.hold = false;
            if self.downs.is_empty() {
                pads_tx.send(audio_bank_cmd!(self.bank, PushEvent, audio::Event::Sync))?;
            }
            pads_tx.send(audio_bank_cmd!(self.bank, BakeRecord, audio::MAX_PHRASE_LEN))?;
            tui_tx.send(tui_bank_cmd!(self.bank, BakeRecord, None, audio::MAX_PHRASE_LEN))?;
        }
        Ok(())
    }

    fn handle_kit_up(&mut self, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        self.state = BankState::LoadOnset;
        tui_tx.send(tui_bank_cmd!(self.bank, LoadOnset))?;
        Ok(())
    }

    fn handle_kit_down(&mut self, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        self.state = BankState::LoadKit;
        tui_tx.send(tui_bank_cmd!(self.bank, LoadKit, None))?;
        Ok(())
    }

    fn handle_pool_up<const N: usize>(&mut self, pads_tx: &mut Sender<audio::Cmd<N>>, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        if let BankState::BuildPool { cleared: false } = self.state {
            pads_tx.send(audio_bank_cmd!(self.bank, ClearPool))?;
            tui_tx.send(tui_bank_cmd!(self.bank, ClearPool))?;
        }
        self.state = BankState::LoadOnset;
        tui_tx.send(tui_bank_cmd!(self.bank, LoadOnset))?;
        Ok(())
    }

    fn handle_pool_down<const N: usize>(&mut self, pads_tx: &mut Sender<audio::Cmd<N>>, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        if self.reverse {
            self.hold = !self.hold;
            if !self.hold && self.downs.is_empty() {
                pads_tx.send(audio_bank_cmd!(self.bank, PushEvent, audio::Event::Sync))?;
            }
        } else {
            self.state = BankState::BuildPool { cleared: false };
            tui_tx.send(tui_bank_cmd!(self.bank, BuildPool))?;
        }
        Ok(())
    }

    fn handle_reverse_up(&mut self) {
        self.reverse = false;
    }

    fn handle_reverse_down(&mut self) {
        self.reverse = true;
    }

    fn handle_pad_up<const N: usize>(&mut self, pads_tx: &mut Sender<audio::Cmd<N>>, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        match self.state {
            BankState::LoadOnset => if !self.hold {
                self.handle_pad_input(pads_tx)?;
            }
            BankState::BakeRecord => {
                let len = if self.downs.len() > 1 {
                    let index = self.downs[0];
                    self.downs.iter().skip(1).map(|v| {
                        v.checked_sub(index + 1).unwrap_or(v + PAD_COUNT as u8 - 1 - index)
                    })
                    .fold(0u8, |acc, v| acc | (1 << v)) as u16
                } else {
                    audio::MAX_PHRASE_LEN
                };
                pads_tx.send(audio_bank_cmd!(self.bank, BakeRecord, len))?;
                tui_tx.send(tui_bank_cmd!(self.bank, BakeRecord, self.downs.first().copied(), len))?;
            }
            _ => (),
        }
        Ok(())
    }

    fn handle_pad_down<const N: usize>(&mut self, pads_tx: &mut Sender<audio::Cmd<N>>, tui_tx: &mut Sender<tui::Cmd>) -> Result<()> {
        match self.state {
            BankState::LoadOnset => self.handle_pad_input(pads_tx)?,
            BankState::LoadKit => {
                pads_tx.send(audio_bank_cmd!(self.bank, LoadKit, self.downs[0]))?;
                tui_tx.send(tui_bank_cmd!(self.bank, LoadKit, Some(self.downs[0])))?;
            }
            BankState::AssignKit => {
                pads_tx.send(audio_bank_cmd!(self.bank, AssignKit, self.downs[0]))?;
                tui_tx.send(tui_bank_cmd!(self.bank, AssignKit, Some(self.downs[0])))?;
            }
            BankState::BakeRecord => {
                let len = if self.downs.len() > 1 {
                    let index = self.downs[0];
                    self.downs.iter().skip(1).map(|v| {
                        v.checked_sub(index + 1).unwrap_or(v + PAD_COUNT as u8 - 1 - index)
                    })
                    .fold(0u8, |acc, v| acc | (1 << v)) as u16
                } else {
                    audio::MAX_PHRASE_LEN
                };
                pads_tx.send(audio_bank_cmd!(self.bank, BakeRecord, len))?;
                tui_tx.send(tui_bank_cmd!(self.bank, BakeRecord, self.downs.first().copied(), len))?;
            }
            BankState::BuildPool { cleared } => {
                if !cleared {
                    self.state = BankState::BuildPool { cleared: true };
                    pads_tx.send(audio_bank_cmd!(self.bank, ClearPool))?;
                    tui_tx.send(tui_bank_cmd!(self.bank, ClearPool))?;
                }
                pads_tx.send(audio_bank_cmd!(self.bank, PushPool, self.downs[0]))?;
            }
        }
        Ok(())
    }

    fn handle_pad_input<const N: usize>(&mut self, pads_tx: &mut Sender<audio::Cmd<N>>) -> Result<()> {
        if let Some(&index) = self.downs.first() {
            if self.downs.len() > 1 {
                // init loop start
                let numerator = self.downs.iter().skip(1).map(|v| {
                    v.checked_sub(index + 1).unwrap_or(v + PAD_COUNT as u8 - 1 - index)
                })
                .fold(0u8, |acc, v| acc | (1 << v));
                let len = audio::Fraction::new(numerator, audio::LOOP_DIV);
                pads_tx.send(audio_bank_cmd!(self.bank, PushEvent, audio::Event::Loop { index, len }))?;
            } else {
                // init loop stop || jump
                pads_tx.send(audio_bank_cmd!(self.bank, PushEvent, audio::Event::Hold { index }))?;
            }
        } else {
            // init sync
            pads_tx.send(audio_bank_cmd!(self.bank, PushEvent, audio::Event::Sync))?;
        }
        Ok(())
    }
}

pub struct InputHandler {
    clock: u8,
    last_step: Option<std::time::Instant>,

    state: GlobalState,
    bank_a: BankHandler,
    bank_b: BankHandler,

    pads_tx: Sender<audio::Cmd<PAD_COUNT>>,
    tui_tx: Sender<tui::Cmd>,
}

impl InputHandler {
    pub fn new(tui_tx: Sender<tui::Cmd>, pads_tx: Sender<audio::Cmd<PAD_COUNT>>) -> Result<Self> {
        Ok(Self {
            clock: 0,
            last_step: None,

            state: GlobalState::Yield,
            bank_a: BankHandler::new(Bank::A),
            bank_b: BankHandler::new(Bank::B),

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
            LiveEvent::Midi { message, .. } => {
                match message {
                    MidiMessage::NoteOff{ key, .. } => match key.as_int() {
                        v if v == KeyCode::Global as u8 => {
                            self.state = GlobalState::Yield;
                            self.tui_tx.send(tui::Cmd::Yield)?;
                        }
                        v if v == KeyCode::RecordA as u8 => if let GlobalState::Yield = self.state {
                            self.bank_a.handle_record_up(&mut self.pads_tx, &mut self.tui_tx)?;
                        }
                        v if v == KeyCode::KitA as u8 => if let GlobalState::Yield = self.state {
                            self.bank_a.handle_kit_up(&mut self.tui_tx)?;
                        }
                        v if v == KeyCode::PoolA as u8 => if let GlobalState::Yield = self.state {
                            self.bank_a.handle_pool_up(&mut self.pads_tx, &mut self.tui_tx)?;
                        }
                        v if v == KeyCode::ReverseA as u8 => match &mut self.state {
                            GlobalState::Yield => {
                                self.bank_a.handle_reverse_up();
                            }
                            GlobalState::AssignOnset { paths, file_index, rd, onset_index, alt } => {
                                *alt = false;
                                let name = paths[*file_index].file_stem().unwrap().to_str().unwrap().to_string();
                                self.tui_tx.send(tui::Cmd::AssignOnset { name, index: *onset_index, count: rd.onsets.len(), alt: *alt })?;
                            }
                            _ => (),
                        }
                        v if (KeyCode::BankAOffset as u8..KeyCode::BankAOffset as u8 + PAD_COUNT as u8).contains(&v) => {
                            let index = v - KeyCode::BankAOffset as u8;
                            self.bank_a.downs.retain(|&v| v != index);
                            self.tui_tx.send(tui_bank_cmd!(Bank::A, Pad, index, false))?;
                            match self.state {
                                GlobalState::Yield => {
                                    self.bank_a.handle_pad_up(&mut self.pads_tx, &mut self.tui_tx)?;
                                }
                                GlobalState::AssignOnset { .. } => {
                                    self.pads_tx.send(audio_bank_cmd!(Bank::A, ForceEvent, audio::Event::Sync))?;
                                },
                                _ => (),
                            }
                        }
                        v if v == KeyCode::RecordB as u8 => if let GlobalState::Yield = self.state {
                            self.bank_b.handle_record_up(&mut self.pads_tx, &mut self.tui_tx)?;
                        }
                        v if v == KeyCode::KitB as u8 => if let GlobalState::Yield = self.state {
                            self.bank_b.handle_kit_up(&mut self.tui_tx)?;
                        }
                        v if v == KeyCode::PoolB as u8 => if let GlobalState::Yield = self.state {
                            self.bank_b.handle_pool_up(&mut self.pads_tx, &mut self.tui_tx)?;
                        }
                        v if v == KeyCode::ReverseB as u8 => if let GlobalState::Yield = self.state {
                            self.bank_b.handle_reverse_up();
                        }
                        v if (KeyCode::BankBOffset as u8..KeyCode::BankBOffset as u8 + PAD_COUNT as u8).contains(&v) => {
                            let index = v - KeyCode::BankBOffset as u8;
                            self.bank_b.downs.retain(|&v| v != index);
                            self.tui_tx.send(tui_bank_cmd!(Bank::B, Pad, index, false))?;
                            match self.state {
                                GlobalState::Yield => {
                                    self.bank_b.handle_pad_up(&mut self.pads_tx, &mut self.tui_tx)?;
                                }
                                GlobalState::AssignOnset { .. } => {
                                    self.pads_tx.send(audio_bank_cmd!(Bank::B, ForceEvent, audio::Event::Sync))?;
                                },
                                _ => (),
                            }
                        }
                        _ => (),
                    }
                    MidiMessage::NoteOn { key, .. } => match key.as_int() {
                        v if v == KeyCode::Global as u8 => self.state = GlobalState::Prime,
                        v if v == KeyCode::RecordA as u8 => match &mut self.state {
                            GlobalState::Yield => {
                                self.bank_a.handle_record_down(&mut self.pads_tx, &mut self.tui_tx)?;
                            }
                            GlobalState::Prime => {
                                // save active scene for both banks
                                let mut index = 0;
                                let mut file = std::fs::File::create_new(format!("scenes/scene{}.sd", index));
                                while file.is_err() {
                                    index += 1;
                                    file = std::fs::File::create_new(format!("scenes/scene{}.sd", index));
                                }
                                self.pads_tx.send(audio::Cmd::SaveScene(file?))?;
                                self.tui_tx.send(tui::Cmd::SaveScene(format!("scenes/scene{}.sd", index)))?;
                            }
                            GlobalState::LoadScene { paths, file_index } => {
                                // decrement file index
                                *file_index = (*file_index as isize - 1).rem_euclid(paths.len() as isize) as usize;
                                self.tui_tx.send(tui::Cmd::LoadScene(to_fs_at!(paths, *file_index)))?;
                            }
                            GlobalState::LoadWav { paths, file_index } => {
                                // decrement file index
                                *file_index = (*file_index as isize - 1).rem_euclid(paths.len() as isize) as usize;
                                self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, *file_index)))?;
                            }
                            GlobalState::AssignOnset { paths, file_index, rd, onset_index, alt } => {
                                // decrement onset index
                                *onset_index = (*onset_index as isize - 1).rem_euclid(rd.onsets.len() as isize) as usize;
                                let name = paths[*file_index].file_stem().unwrap().to_str().unwrap().to_string();
                                self.tui_tx.send(tui::Cmd::AssignOnset { name, index: *onset_index, count: rd.onsets.len(), alt: *alt })?;
                            }
                        }
                        v if v == KeyCode::KitA as u8 => match &self.state {
                            GlobalState::Yield => {
                                self.bank_a.handle_kit_down(&mut self.tui_tx)?;
                            }
                            GlobalState::Prime => {
                                // open onset dir
                                let mut paths = std::fs::read_dir("onsets")?
                                    .flat_map(|v| Some(v.ok()?.path().into_boxed_path()))
                                    .filter(|v| v.extension().unwrap() == "wav" || v.is_dir())
                                    .collect::<Vec<_>>();
                                paths.sort();
                                self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, 0)))?;
                                self.state = GlobalState::LoadWav {
                                    paths,
                                    file_index: 0,
                                }
                            }
                            GlobalState::LoadScene { paths, file_index } => {
                                if paths.is_empty() {
                                    self.state = GlobalState::Yield;
                                    self.tui_tx.send(tui::Cmd::Yield)?;
                                } else {
                                    let path = &paths[*file_index];
                                    if path.is_dir() {
                                        // enter subdirectory
                                        let mut paths = if path.parent().unwrap() == PathBuf::from("") {
                                            // in ./scenes; don't include ".."
                                            Vec::new()
                                        } else {
                                            // in subdirectory; include ".."
                                            vec![path.parent().unwrap().into()]
                                        };
                                        paths.extend(std::fs::read_dir(path)?
                                            .flat_map(|v| Some(v.ok()?.path().into_boxed_path()))
                                            .filter(|v| v.extension().unwrap() == "sd" || v.is_dir())
                                        );
                                        paths.sort();
                                        self.tui_tx.send(tui::Cmd::LoadScene(to_fs_at!(paths, 0)))?;
                                        self.state = GlobalState::LoadScene { paths, file_index: 0 };
                                    } else {
                                        // load scene
                                        let sd_string = std::fs::read_to_string(&paths[*file_index])?;
                                        let scene: audio::pads::Scene<PAD_COUNT> = serde_json::from_str(&sd_string)?;
                                        self.tui_tx.send(tui::Cmd::AssignScene(Box::new(tui::Scene::from_audio(&scene))))?;
                                        self.pads_tx.send(audio::Cmd::LoadScene(Box::new(scene)))?;
                                    }
                                }
                            }
                            GlobalState::LoadWav { paths, file_index } => {
                                if paths.is_empty() {
                                    self.state = GlobalState::Yield;
                                    self.tui_tx.send(tui::Cmd::Yield)?;
                                } else {
                                    // enter subdirectory
                                    let path = &paths[*file_index];
                                    if path.is_dir() {
                                        let mut paths = if path.parent().unwrap() == PathBuf::from("") {
                                            // in ./onsets; don't include ".."
                                            Vec::new()
                                        } else {
                                            // in subdirectory; include ".."
                                            vec![path.parent().unwrap().into()]
                                        };
                                        paths.extend(std::fs::read_dir(path)?
                                            .flat_map(|v| Some(v.ok()?.path().into_boxed_path()))
                                            .filter(|v| v.extension().unwrap() == "wav" || v.is_dir())
                                        );
                                        paths.sort();
                                        self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, 0)))?;
                                        self.state = GlobalState::LoadWav { paths, file_index: 0 };
                                    } else {
                                        // enter onset selection
                                        let rd_string = std::fs::read_to_string(path.with_extension("rd"))?;
                                        let rd: audio::Rd = serde_json::from_str(&rd_string)?;
                                        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
                                        self.tui_tx.send(tui::Cmd::AssignOnset { name, index: 0, count: rd.onsets.len(), alt: false })?;
                                        self.state = GlobalState::AssignOnset {
                                            paths: paths.clone(),
                                            file_index: *file_index,
                                            rd,
                                            onset_index: 0,
                                            alt: false,
                                        };
                                    }
                                }
                            }
                            GlobalState::AssignOnset { paths, file_index, .. } => {
                                // exit onset selection, return to dir
                                self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, *file_index)))?;
                                self.state = GlobalState::LoadWav { paths: paths.clone(), file_index: 0 };
                            }
                        }
                        v if v == KeyCode::PoolA as u8 => match &mut self.state {
                            GlobalState::Yield => {
                                self.bank_a.handle_pool_down(&mut self.pads_tx, &mut self.tui_tx)?;
                            }
                            GlobalState::Prime => {
                                // open scene dir
                                let mut paths = std::fs::read_dir("scenes")?
                                    .flat_map(|v| Some(v.ok()?.path().into_boxed_path()))
                                    .filter(|v| v.extension().unwrap() == "sd" || v.is_dir())
                                    .collect::<Vec<_>>();
                                paths.sort();
                                self.tui_tx.send(tui::Cmd::LoadScene(to_fs_at!(paths, 0)))?;
                                self.state = GlobalState::LoadScene {
                                    paths,
                                    file_index: 0,
                                };
                            }
                            GlobalState::LoadScene { paths, file_index } => {
                                // increment file index
                                *file_index = (*file_index as isize + 1).rem_euclid(paths.len() as isize) as usize;
                                self.tui_tx.send(tui::Cmd::LoadScene(to_fs_at!(paths, *file_index)))?;
                            }
                            GlobalState::LoadWav { paths, file_index } => {
                                // increment file index
                                *file_index = (*file_index as isize + 1).rem_euclid(paths.len() as isize) as usize;
                                self.tui_tx.send(tui::Cmd::LoadWav(to_fs_at!(paths, *file_index)))?;
                            }
                            GlobalState::AssignOnset { paths, file_index, rd, onset_index, alt } => {
                                // increment onset index
                                *onset_index = (*onset_index as isize + 1).rem_euclid(rd.onsets.len() as isize) as usize;
                                let name = paths[*file_index].file_stem().unwrap().to_str().unwrap().to_string();
                                self.tui_tx.send(tui::Cmd::AssignOnset { name, index: *onset_index, count: rd.onsets.len(), alt: *alt })?;
                            }
                        }
                        v if v == KeyCode::ReverseA as u8 => match &mut self.state {
                            GlobalState::Yield => {
                                self.bank_a.handle_reverse_down();
                            }
                            GlobalState::AssignOnset { paths, file_index, rd, onset_index, alt } => {
                                *alt = true;
                                let name = paths[*file_index].file_stem().unwrap().to_str().unwrap().to_string();
                                self.tui_tx.send(tui::Cmd::AssignOnset { name, index: *onset_index, count: rd.onsets.len(), alt: *alt })?;
                            }
                            _ => (),
                        }
                        v if (KeyCode::BankAOffset as u8..KeyCode::BankAOffset as u8 + PAD_COUNT as u8).contains(&v) => {
                            let index = v - KeyCode::BankAOffset as u8;
                            self.bank_a.downs.push(index);
                            self.tui_tx.send(tui_bank_cmd!(Bank::A, Pad, index, true))?;
                            match &self.state {
                                GlobalState::Yield => {
                                    self.bank_a.handle_pad_down(&mut self.pads_tx, &mut self.tui_tx)?;
                                }
                                GlobalState::AssignOnset { paths, file_index, rd, onset_index, alt } => {
                                    // assign onset to pad
                                    let len = std::fs::metadata(&paths[*file_index])?.len() - 44;
                                    let start = rd.onsets[*onset_index];
                                    let wav = audio::Wav {
                                        tempo: rd.tempo,
                                        steps: rd.steps,
                                        path: paths[*file_index].clone(),
                                        len,
                                    };
                                    let onset = audio::Onset { wav, start };
                                    self.pads_tx.send(audio_bank_cmd!(Bank::A, AssignOnset, index, *alt, Box::new(onset)))?;
                                }
                                _ => (),
                            }
                        }
                        v if v == KeyCode::RecordB as u8 => if let GlobalState::Yield = self.state {
                            self.bank_b.handle_record_down(&mut self.pads_tx, &mut self.tui_tx)?;
                        }
                        v if v == KeyCode::KitB as u8 => if let GlobalState::Yield = self.state {
                            self.bank_b.handle_kit_down(&mut self.tui_tx)?;
                        }
                        v if v == KeyCode::PoolB as u8 => if let GlobalState::Yield = self.state {
                            self.bank_b.handle_pool_down(&mut self.pads_tx, &mut self.tui_tx)?;
                        }
                        v if v == KeyCode::ReverseB as u8 => if let GlobalState::Yield = self.state {
                            self.bank_b.handle_reverse_down();
                        }
                        v if (KeyCode::BankBOffset as u8..KeyCode::BankBOffset as u8 + PAD_COUNT as u8).contains(&v) => {
                            let index = v - KeyCode::BankBOffset as u8;
                            self.bank_b.downs.push(index);
                            self.tui_tx.send(tui_bank_cmd!(Bank::B, Pad, index, true))?;
                            match &self.state {
                                GlobalState::Yield => {
                                    self.bank_b.handle_pad_down(&mut self.pads_tx, &mut self.tui_tx)?;
                                }
                                GlobalState::AssignOnset { paths, file_index, rd, onset_index, alt } => {
                                    // assign onset to pad
                                    let len = std::fs::metadata(&paths[*file_index])?.len() - 44;
                                    let start = rd.onsets[*onset_index];
                                    let wav = audio::Wav {
                                        tempo: rd.tempo,
                                        steps: rd.steps,
                                        path: paths[*file_index].clone(),
                                        len,
                                    };
                                    let onset = audio::Onset { wav, start };
                                    self.pads_tx.send(audio_bank_cmd!(Bank::B, AssignOnset, index, *alt, Box::new(onset)))?;
                                }
                                _ => (),
                            }
                        }
                        _ => (),
                    }
                    MidiMessage::Controller { controller, value } => match controller.as_int() {
                        v if v == CtrlCode::Blend as u8 => {
                            self.pads_tx.send(audio::Cmd::AssignBlend(value.as_int() as f32 / 127.))?;
                        }
                        v if v == CtrlCode::SpeedA as u8 => {
                            self.pads_tx.send(audio_bank_cmd!(Bank::A, AssignSpeed, value.as_int() as f32 / 127. * 2.))?;
                        }
                        v if v == CtrlCode::DriftA as u8 => {
                            self.pads_tx.send(audio_bank_cmd!(Bank::A, AssignDrift, value.as_int() as f32 / 127.))?;
                            self.tui_tx.send(tui_bank_cmd!(Bank::A, AssignDrift, value.as_int()))?;
                        }
                        v if v == CtrlCode::BiasA as u8 => {
                            self.pads_tx.send(audio_bank_cmd!(Bank::A, AssignBias, value.as_int() as f32 / 127.))?;
                            self.tui_tx.send(tui_bank_cmd!(Bank::A, AssignBias, value.as_int()))?;
                        }
                        v if v == CtrlCode::WidthA as u8 => {
                            self.pads_tx.send(audio_bank_cmd!(Bank::A, AssignWidth, value.as_int() as f32 / 127.))?;
                        }
                        v if v == CtrlCode::SpeedB as u8 => {
                            self.pads_tx.send(audio_bank_cmd!(Bank::B, AssignSpeed, value.as_int() as f32 / 127. * 2.))?;
                        }
                        v if v == CtrlCode::DriftB as u8 => {
                            self.pads_tx.send(audio_bank_cmd!(Bank::B, AssignDrift, value.as_int() as f32 / 127.))?;
                            self.tui_tx.send(tui_bank_cmd!(Bank::B, AssignDrift, value.as_int()))?;
                        }
                        v if v == CtrlCode::BiasB as u8 => {
                            self.pads_tx.send(audio_bank_cmd!(Bank::B, AssignBias, value.as_int() as f32 / 127.))?;
                            self.tui_tx.send(tui_bank_cmd!(Bank::B, AssignBias, value.as_int()))?;
                        }
                        v if v == CtrlCode::WidthB as u8 => {
                            self.pads_tx.send(audio_bank_cmd!(Bank::B, AssignWidth, value.as_int() as f32 / 127.))?;
                        }
                        _ => (),
                    }
                    MidiMessage::PitchBend { bend } => {
                        // affect both banks
                        self.pads_tx.send(audio::Cmd::OffsetSpeed(bend.as_f32() + 1.))?;
                    }
                    _ => (),
                }
            }
            LiveEvent::Realtime(midly::live::SystemRealtime::TimingClock) => {
                // affect both banks
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
                // affect both banks
                self.last_step = None;
                self.clock = 0;
                self.pads_tx.send(audio::Cmd::Stop)?;
                self.tui_tx.send(tui::Cmd::Stop)?;
            }
            _ => (),
        }
        Ok(())
    }
}
