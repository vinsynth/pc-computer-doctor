use crate::audio::PAD_COUNT;
use crate::input::Bank;

use color_eyre::eyre::Result;
use crossterm::event::{self, KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Flex, Layout, Rect},
    style::Stylize,
    text::{Line, Text},
    widgets::{Block, Padding, Paragraph, Widget, Wrap},
    DefaultTerminal, Frame,
};

pub const FILE_COUNT: usize = 5;
pub const LOG_DURATION: std::time::Duration = std::time::Duration::from_millis(1000);

macro_rules! down {
    ($char:expr) => {
        event::Event::Key(KeyEvent {
            code: KeyCode::Char($char),
            kind: KeyEventKind::Press,
            ..
        })
    };
}

pub enum Cmd {
    Clock,
    Stop,
    Yield,
    AssignScene(Box<Scene>),
    SaveScene(String),
    LoadScene([String; FILE_COUNT]),
    LoadWav([String; FILE_COUNT]),
    AssignOnset { name: String, index: usize, count: usize, alt: bool },
    Bank(Bank, BankCmd),
}

pub enum BankCmd {
    Pad(u8, bool),
    LoadOnset,
    AssignDrift(u8),
    AssignBias(u8),
    AssignKit(Option<u8>),
    LoadKit(Option<u8>),
    BakeRecord(Option<u8>, u16),
    BuildPool,
    ClearPool,
}

#[derive(Default)]
enum GlobalState {
    #[default]
    Yield,
    LoadScene { paths: [String; FILE_COUNT] },
    LoadWav { paths: [String; FILE_COUNT] },
    AssignOnset { name: String, index: usize, count: usize, alt: bool },
}

#[derive(Default)]
enum BankState {
    #[default]
    LoadOnset,
    LoadKit { index: Option<u8> },
    AssignKit { index: Option<u8> },
    BakeRecord { index: Option<u8>, len: u16 },
    BuildPool,
}

#[derive(Copy, Clone, Default)]
pub struct Pad {
    pub onsets: [bool; 2],
    pub phrase: bool,
}

#[derive(Default)]
pub struct Scene {
    pub kit_a: [[Pad; PAD_COUNT]; PAD_COUNT],
    pub kit_b: [[Pad; PAD_COUNT]; PAD_COUNT],
}

impl Scene {
    pub fn from_audio(scene: &crate::audio::pads::Scene<PAD_COUNT>) -> Self {
        let kit_a = core::array::from_fn(|i| {
            core::array::from_fn(|j| {
                let pad = &scene.kit_a[i].inner[j];
                Pad {
                    onsets: [pad.onsets[0].is_some(), pad.onsets[1].is_some()],
                    phrase: pad.phrase.is_some(),
                }
            })
        });
        let kit_b = core::array::from_fn(|i| {
            core::array::from_fn(|j| {
                let pad = &scene.kit_b[i].inner[j];
                Pad {
                    onsets: [pad.onsets[0].is_some(), pad.onsets[1].is_some()],
                    phrase: pad.phrase.is_some(),
                }
            })
        });
        Self { kit_a, kit_b }
    }
}

#[derive(Default)]
struct BankHandler {
    drift: u8,
    bias: u8,
    pads: [Pad; PAD_COUNT],
    downs: Vec<u8>,
    pool: Vec<u8>,
    state: BankState,
}

impl BankHandler {
    fn cmd(&mut self, kits: &mut [[Pad; PAD_COUNT]; PAD_COUNT], cmd: BankCmd) {
        match cmd {
            BankCmd::Pad(index, down) => self.pad(index, down),
            BankCmd::LoadOnset => self.load_onset(),
            BankCmd::AssignDrift(v) => self.drift = v,
            BankCmd::AssignBias(v) => self.bias = v,
            BankCmd::AssignKit(index) => self.assign_kit(kits, index),
            BankCmd::LoadKit(index) => self.load_kit(kits, index),
            BankCmd::BakeRecord(index, len) => self.state = BankState::BakeRecord { index, len },
            BankCmd::BuildPool => self.state = BankState::BuildPool,
            BankCmd::ClearPool => self.pool.clear(),
        }
    }

    fn pad(&mut self, index: u8, down: bool) {
        if down {
            self.downs.push(index);
            if let BankState::BuildPool = &mut self.state {
                self.pool.push(index);
            }
        } else {
            self.downs.retain(|v| *v != index);
        }
    }

    fn load_onset(&mut self) {
        if let BankState::BakeRecord { index: Some(index), .. } = self.state {
            self.pads[index as usize].phrase = true;
        }
        self.state = BankState::LoadOnset;
    }

    fn assign_kit(&mut self, kits: &mut [[Pad; PAD_COUNT]; PAD_COUNT], index: Option<u8>) {
        if let Some(index) = index {
            kits[index as usize] = self.pads;
        }
        self.state = BankState::AssignKit { index };
    }

    fn load_kit(&mut self, kits: &mut [[Pad; PAD_COUNT]; PAD_COUNT], index: Option<u8>) {
        if let Some(index) = index {
            self.pads = kits[index as usize];
        }
        self.state = BankState::LoadKit { index };
    }

    fn render(&self, kits: &[[Pad; PAD_COUNT]; PAD_COUNT], flex: Flex, area: Rect, buf: &mut Buffer) {
        match self.state {
            BankState::LoadOnset => self.render_load_onset(flex, area, buf),
            BankState::LoadKit { index } => self.render_load_kit(index, kits, flex, area, buf),
            BankState::AssignKit { index } => self.render_assign_kit(index, kits, flex, area, buf),
            BankState::BakeRecord { index, len } => self.render_bake_record(index, len, flex, area, buf),
            BankState::BuildPool { .. } => self.render_pool(area, buf),
        }
    }

    fn render_load_onset(&self, flex: Flex, area: Rect, buf: &mut Buffer) {
        let [area] = Layout::horizontal(vec![Constraint::Max(14)])
            .flex(flex)
            .areas(area);
        let [pad_area, param_area] = Layout::vertical(Constraint::from_maxes([3, 3]))
            .flex(Flex::SpaceBetween)
            .areas(area);
        // render border
        Block::bordered().bold().render(area, buf);
        // render pads
        Paragraph::new(Text::raw(String::from_iter(core::array::from_fn::<_, PAD_COUNT, _>(|i| {
            if self.downs.contains(&(i as u8)) {
                'o'
            } else {
                '.'
            }
        }))))
        .block(Block::new().bold().padding(Padding::new(5, 5, 1, 0)))
        .wrap(Wrap { trim: false })
        .render(pad_area, buf);
        // render params
        Paragraph::new(Text::from(vec![
            Line::raw(format!("drift: {:>3}", self.drift)).italic(),
            Line::raw(format!(" bias: {:>3}", self.bias)).italic(),
        ]))
        .block(Block::new().bold().padding(Padding::new(2, 2, 0, 1)))
        .left_aligned()
        .render(param_area, buf);
    }

    fn render_load_kit(&self, index: Option<u8>, kits: &[[Pad; PAD_COUNT]; PAD_COUNT], flex: Flex, area: Rect, buf: &mut Buffer) {
        let [area] = Layout::horizontal(vec![Constraint::Max(14)]).flex(flex).areas(area);
        let mut text: [_; PAD_COUNT] = core::array::from_fn(|i| {
            if kits[i].iter().any(|v| v.onsets[0] || v.onsets[1] || v.phrase) {
                'k'
            } else {
                '.'
            }
        });
        if let Some(index) = index {
            text[index as usize] = 'o';
        }
        Paragraph::new(Text::raw(String::from_iter(text)).centered())
            .block(Block::bordered().bold().title(" load kit: ").padding(Padding::horizontal(4)))
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_assign_kit(&self, index: Option<u8>, kits: &[[Pad; PAD_COUNT]; PAD_COUNT], flex: Flex, area: Rect, buf: &mut Buffer) {
        let [area] = Layout::horizontal(vec![Constraint::Max(16)]).flex(flex).areas(area);
        let mut text: [_; PAD_COUNT] = core::array::from_fn(|i| {
            if kits[i].iter().any(|v| v.onsets[0] || v.onsets[1] || v.phrase) {
                'k'
            } else {
                '.'
            }
        });
        if let Some(index) = index {
            text[index as usize] = 'o';
        }
        Paragraph::new(Text::raw(String::from_iter(text)).centered())
            .block(Block::bordered().bold().title(" assign kit: ").padding(Padding::horizontal(5)))
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_bake_record(&self, index: Option<u8>, len: u16, flex: Flex, area: Rect, buf: &mut Buffer) {
        let [area] = Layout::horizontal(vec![Constraint::Max(16)])
            .flex(flex)
            .areas(area);
        let [pad_area, len_area] = Layout::vertical(Constraint::from_maxes([3, 2]))
            .flex(Flex::SpaceBetween)
            .areas(area);
        // render border
        Block::bordered().bold().title(" bake record: ").render(area, buf);
        {
            // render pads
            let mut text: [_; PAD_COUNT] = core::array::from_fn(|i| {
                if self.downs.contains(&(i as u8)) {
                    '-'
                } else {
                    '.'
                }
            });
            if let Some(index) = index {
                text[index as usize] = 'o';
            }
            Paragraph::new(Text::raw(String::from_iter(text)).centered())
                .block(Block::new().padding(Padding::new(6, 6, 1, 0)))
                .wrap(Wrap { trim: false })
                .render(pad_area, buf);
        }
        // render length
        Paragraph::new(Text::raw(format!("length: {:>3}", len)).left_aligned())
            .block(Block::new().padding(Padding::new(2, 2, 0, 1)))
            .wrap(Wrap { trim: false})
            .render(len_area, buf);
    }

    fn render_pool(&self, area: Rect, buf: &mut Buffer) {
        let [pad_area, pool_area] = Layout::horizontal(vec![Constraint::Min(8), Constraint::Percentage(100)]).areas(area);
        let [_, arrow_area] = Layout::horizontal(Constraint::from_maxes([7, 2])).flex(Flex::Start).areas(area);
        // render pads
        Paragraph::new(Text::raw(String::from_iter(core::array::from_fn::<_, PAD_COUNT, _>(|i| {
            if self.downs.contains(&(i as u8)) {
                'o'
            } else if self.pads[i].phrase {
                'p'
            } else {
                '.'
            }
        }))))
        .block(Block::bordered().bold().padding(Padding::horizontal(1)))
        .wrap(Wrap { trim: false })
        .render(pad_area, buf);
        // render pool
        Paragraph::new(Text::raw(format!("{:?}", self.pool)).left_aligned())
            .block(Block::bordered().title(" build sequence: ").padding(Padding::horizontal(1)))
            .wrap(Wrap { trim: false })
            .render(pool_area, buf);
        // render arrow
        Paragraph::new(Text::raw(">>"))
            .block(Block::new().padding(Padding::new(0, 0, 1, 0)))
            .render(arrow_area, buf);
    }
}

#[derive(Default)]
pub struct TuiHandler {
    exit: bool,
    clock: bool,
    scene: Scene,

    log: Option<(std::time::Instant, String)>,

    state: GlobalState,
    bank_a: BankHandler,
    bank_b: BankHandler,
}

impl TuiHandler {
    pub fn run(&mut self, terminal: &mut DefaultTerminal, input_rx: std::sync::mpsc::Receiver<Cmd>) -> Result<()> {
        terminal.draw(|frame| self.draw(frame))?;
        while !self.exit {
            let mut flush = false;
            if let Some((start, ..)) = &self.log {
                if start.elapsed() >= LOG_DURATION {
                    self.log = None;
                    flush = true;
                }
            }
            if event::poll(std::time::Duration::ZERO)? {
                self.kbd()?;
                flush = true;
            }
            match input_rx.try_recv() {
                Ok(cmd) => {
                    self.cmd(cmd);
                    flush = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => (),
                Err(e) => Err(e)?,
            }
            if flush {
                terminal.draw(|frame| self.draw(frame))?;
            }
        }
        Ok(())
    }

    fn kbd(&mut self) -> Result<()> {
        if let down!('q') = event::read()? {
            self.exit = true;
        }
        Ok(())
    }

    fn cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Clock => self.clock = !self.clock,
            Cmd::Stop => self.clock = false,
            Cmd::Yield => {
                self.state = GlobalState::Yield;
                self.bank_a.state = BankState::LoadOnset;
                self.bank_b.state = BankState::LoadOnset;
            }
            Cmd::AssignScene(scene) => self.scene = *scene,
            Cmd::SaveScene(path) => self.log = Some((std::time::Instant::now(), format!("saved scene to `{}`!", path))),
            Cmd::LoadScene(paths) => self.state = GlobalState::LoadScene { paths },
            Cmd::LoadWav(paths) => self.state = GlobalState::LoadWav { paths },
            Cmd::AssignOnset { name, index, count, alt } => self.state = GlobalState::AssignOnset { name, index, count, alt },
            Cmd::Bank(bank, cmd) => {
                if let BankCmd::Pad(index, true) = cmd {
                    if let GlobalState::AssignOnset { alt, .. } = self.state {
                        let bank = match bank {
                            Bank::A => &mut self.bank_a,
                            Bank::B => &mut self.bank_b,
                        };
                        bank.pads[index as usize].onsets[alt as usize] = true;
                    }
                }
                match bank {
                    Bank::A => self.bank_a.cmd(&mut self.scene.kit_a, cmd),
                    Bank::B => self.bank_b.cmd(&mut self.scene.kit_b, cmd),
                }
            }
        }
    }

    fn draw(&self, frame: &mut Frame) {
        frame.render_widget(self, frame.area());
    }

    fn render_clock(&self, area: Rect, buf: &mut Buffer) {
        let [left, right] = Layout::horizontal(Constraint::from_maxes([11, 11])).flex(Flex::Center).areas(area);
        if self.clock {
            Block::new().reversed().render(right, buf);
        } else {
            Block::new().reversed().render(left, buf);
        }
    }

    fn render_log(&self, area: Rect, buf: &mut Buffer) {
        if let Some((_, msg)) = &self.log {
            Paragraph::new(Text::raw(msg)).centered().render(area, buf);
        }
    }

    fn render_load_scene(&self, paths: &[String; FILE_COUNT], area: Rect, buf: &mut Buffer) {
        let [pad_area, fs_area] = Layout::horizontal(vec![Constraint::Min(8), Constraint::Percentage(100)]).areas(area);
        let [_, arrow_area] = Layout::horizontal(Constraint::from_maxes([7, 2])).flex(Flex::Start).areas(area);
        let [a_area, b_area] = Layout::vertical(Constraint::from_maxes([3, 3])).flex(Flex::SpaceBetween).areas(pad_area);
        // render border
        Block::bordered().bold().render(pad_area, buf);
        // render bank a
        Paragraph::new(Text::raw(String::from_iter(core::array::from_fn::<_, PAD_COUNT, _>(|i| {
            if self.bank_a.downs.contains(&(i as u8)) {
                'o'
            } else if self.scene.kit_a[i].iter().any(|v| v.onsets[0] || v.onsets[1] || v.phrase) {
                'k'
            } else {
                '.'
            }
        }))))
        .block(Block::new().bold().padding(Padding::new(2, 2, 1, 0)))
        .wrap(Wrap { trim: false })
        .render(a_area, buf);
        // render bank b
        Paragraph::new(Text::raw(String::from_iter(core::array::from_fn::<_, PAD_COUNT, _>(|i| {
            if self.bank_b.downs.contains(&(i as u8)) {
                'o'
            } else if self.scene.kit_b[i].iter().any(|v| v.onsets[0] || v.onsets[1] || v.phrase) {
                'k'
            } else {
                '.'
            }
        }))))
        .block(Block::new().bold().padding(Padding::new(2, 2, 0, 1)))
        .wrap(Wrap { trim: false })
        .render(b_area, buf);
        {
            // render fs
            let text = if paths.iter().any(|v| !v.is_empty()) {
                let mut lines = paths.clone().map(Line::raw).to_vec();
                let mid = lines.len() / 2;
                lines[mid] = lines[mid].clone().reversed();
                Text::from(lines.to_vec())
            } else {
                Text::raw("no files found </3")
            };
            Paragraph::new(text)
                .left_aligned()
                .block(Block::bordered().title(" load scene: ").padding(Padding::horizontal(1)))
                .render(fs_area, buf);
        }
        // render arrow
        Paragraph::new(Text::raw("<<"))
            .block(Block::new().padding(Padding::new(0, 0, FILE_COUNT as u16 / 2 + 1, 0)))
            .render(arrow_area, buf);
    }

    fn render_load_wav(&self, paths: &[String; FILE_COUNT], area: Rect, buf: &mut Buffer) {
        let [pad_area, fs_area] = Layout::horizontal(vec![Constraint::Min(8), Constraint::Percentage(100)]).areas(area);
        let [a_area, b_area] = Layout::vertical(Constraint::from_maxes([3, 3])).flex(Flex::SpaceBetween).areas(pad_area);
        // render border
        Block::bordered().bold().render(pad_area, buf);
        // render bank a
        Paragraph::new(Text::raw(String::from_iter(core::array::from_fn::<_, PAD_COUNT, _>(|i| {
            if self.bank_a.downs.contains(&(i as u8)) {
                'o'
            } else {
                let pad = self.bank_a.pads[i];
                if pad.onsets[0] && pad.onsets[1] {
                    '@'
                } else if pad.onsets[0] {
                    'a'
                } else if pad.onsets[1] {
                    'b'
                } else {
                    '.'
                }
            }
        }))))
        .block(Block::new().bold().padding(Padding::new(2, 2, 1, 0)))
        .wrap(Wrap { trim: false })
        .render(a_area, buf);
        // render bank b
        Paragraph::new(Text::raw(String::from_iter(core::array::from_fn::<_, PAD_COUNT, _>(|i| {
            if self.bank_b.downs.contains(&(i as u8)) {
                'o'
            } else {
                let pad = self.bank_b.pads[i];
                if pad.onsets[0] && pad.onsets[1] {
                    '@'
                } else if pad.onsets[0] {
                    'a'
                } else if pad.onsets[1] {
                    'b'
                } else {
                    '.'
                }
            }
        }))))
        .block(Block::new().bold().padding(Padding::new(2, 2, 0, 1)))
        .wrap(Wrap { trim: false })
        .render(b_area, buf);
        {
            // render fs
            let text = if paths.iter().any(|v| !v.is_empty()) {
                let mut lines = paths.clone().map(Line::raw).to_vec();
                let mid = lines.len() / 2;
                lines[mid] = lines[mid].clone().reversed();
                Text::from(lines.to_vec())
            } else {
                Text::raw("no files found </3")
            };
            Paragraph::new(text)
                .left_aligned()
                .block(Block::bordered().title(" load wav: ").padding(Padding::horizontal(1)))
                .render(fs_area, buf);
        }
    }

    fn render_assign_onset(&self, name: &String, index: usize, count: usize, alt: bool, area: Rect, buf: &mut Buffer) {
        let [pad_area, onset_area] = Layout::horizontal(vec![Constraint::Min(8), Constraint::Percentage(100)]).areas(area);
        let [_, arrow_area] = Layout::horizontal(Constraint::from_maxes([7, 2])).flex(Flex::Start).areas(area);
        let [a_area, b_area] = Layout::vertical(Constraint::from_maxes([3, 3])).flex(Flex::SpaceBetween).areas(pad_area);
        // render border
        Block::bordered().bold().render(pad_area, buf);
        // render bank a
        Paragraph::new(Text::raw(String::from_iter(core::array::from_fn::<_, PAD_COUNT, _>(|i| {
            if self.bank_a.downs.contains(&(i as u8)) {
                'o'
            } else {
                let pad = self.bank_a.pads[i];
                if alt && pad.onsets[1] {
                    'b'
                } else if !alt && pad.onsets[0] {
                    'a'
                } else {
                    '.'
                }
            }
        }))))
        .block(Block::new().bold().padding(Padding::new(2, 2, 1, 0)))
        .wrap(Wrap { trim: false })
        .render(a_area, buf);
        // render bank b
        Paragraph::new(Text::raw(String::from_iter(core::array::from_fn::<_, PAD_COUNT, _>(|i| {
            if self.bank_b.downs.contains(&(i as u8)) {
                'o'
            } else {
                let pad = self.bank_b.pads[i];
                if alt && pad.onsets[1] {
                    'b'
                } else if !alt && pad.onsets[0] {
                    'a'
                } else {
                    '.'
                }
            }
        }))))
        .block(Block::new().bold().padding(Padding::new(2, 2, 0, 1)))
        .wrap(Wrap { trim: false })
        .render(b_area, buf);
        {
            // render onset
            let mut lines: [Line; FILE_COUNT] = core::array::from_fn(|_| Line::raw(""));
            lines[FILE_COUNT / 2] = Line::raw(name).reversed();
            let to = if alt { 'b' } else { 'a' };
            lines[FILE_COUNT - 1] = Line::raw(format!("{:>3}/{:>3} to {}", index, count, to));
            Paragraph::new(Text::from(lines.to_vec()))
                .left_aligned()
                .block(Block::bordered().title(" assign onset: ").padding(Padding::horizontal(1)))
                .render(onset_area, buf);
        }
        // render arrow
        Paragraph::new(Text::raw("<<"))
            .block(Block::new().padding(Padding::new(0, 0, FILE_COUNT as u16, 0)))
            .render(arrow_area, buf);
    }
}

impl Widget for &TuiHandler {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let [area] = Layout::vertical(vec![Constraint::Max(FILE_COUNT as u16 + 5)])
            .flex(Flex::Center)
            .areas(area);
        let [clock_area, area, log_area] = Layout::vertical(Constraint::from_maxes([2, FILE_COUNT as u16 + 2, 1]))
            .flex(Flex::Center)
            .areas(area);
        self.render_clock(clock_area, buf);
        self.render_log(log_area, buf);
        match &self.state {
            GlobalState::Yield => {
                let [a_area, b_area] = Layout::horizontal(Constraint::from_percentages([50, 50])).flex(Flex::Center).areas(area);
                self.bank_a.render(&self.scene.kit_a, Flex::End, a_area, buf);
                self.bank_b.render(&self.scene.kit_b, Flex::Start, b_area, buf);
            }
            GlobalState::LoadScene { paths } => self.render_load_scene(paths, area, buf),
            GlobalState::LoadWav { paths } => self.render_load_wav(paths, area, buf),
            GlobalState::AssignOnset { name, index, count, alt } => self.render_assign_onset(name, *index, *count, *alt, area, buf)
        }
    }
}
