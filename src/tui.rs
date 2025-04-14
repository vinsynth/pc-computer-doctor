use crate::audio::PAD_COUNT;

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
    Pad(u8, bool),
    Alt(bool),
    Hold(bool),
    Clock,
    Stop,
    AssignDrift(u8),
    AssignBias(u8),

    LoadOnset,
    LoadKit(Option<u8>),
    AssignKit(Option<u8>),
    LoadScene([String; FILE_COUNT]),
    AssignScene(Box<[[Pad; PAD_COUNT]; PAD_COUNT]>),
    LoadWav([String; FILE_COUNT]),
    AssignOnset { name: String, index: usize, count: usize },
    BakeRecord(Option<u8>, u16),
    Pool,
}

#[derive(Default)]
enum State {
    #[default]
    LoadOnset,
    LoadKit(Option<u8>),
    AssignKit(Option<u8>),
    LoadScene([String; FILE_COUNT]),
    LoadWav([String; FILE_COUNT]),
    AssignOnset { name: String, index: usize, count: usize },
    BakeRecord(Option<u8>, u16),
    Pool(bool),
}

#[derive(Copy, Clone, Default)]
pub struct Pad {
    pub onsets: [bool; 2],
    pub phrase: bool,
}

#[derive(Default)]
pub struct Tui {
    exit: bool,
    alt: bool,
    hold: bool,
    clock: bool,
    drift: u8,
    bias: u8,

    scene: [[Pad; PAD_COUNT]; PAD_COUNT],
    pads: [Pad; PAD_COUNT],
    downs: Vec<u8>,
    pool: Vec<u8>,

    state: State,
}

impl Tui {
    pub fn run(
        &mut self,
        terminal: &mut DefaultTerminal,
        input_rx: std::sync::mpsc::Receiver<Cmd>,
    ) -> Result<()> {
        terminal.draw(|frame| self.draw(frame))?;
        while !self.exit {
            let mut flush = false;
            if event::poll(std::time::Duration::ZERO)? {
                self.handle_kbd()?;
                flush = true;
            }
            match input_rx.try_recv() {
                Ok(cmd) => {
                    self.handle_cmd(cmd);
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

    fn handle_kbd(&mut self) -> Result<()> {
        if let down!('q') = event::read()? {
            self.exit = true;
        }
        Ok(())
    }

    fn handle_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Pad(index, down) => {
                if down {
                    self.downs.push(index);
                    match self.state {
                        State::AssignOnset { .. } => {
                            if self.alt {
                                self.pads[index as usize].onsets[1] = true;
                            } else {
                                self.pads[index as usize].onsets[0] = true;
                            }
                        }
                        State::Pool(cleared) => {
                            if !cleared {
                                self.state = State::Pool(true);
                                self.pool.clear();
                            }
                            self.pool.push(index);
                        }
                        _ => (),
                    }
                } else {
                    self.downs.retain(|v| *v != index);
                }
            }
            Cmd::Alt(v) => self.alt = v,
            Cmd::Hold(v) => self.hold = v,
            Cmd::Clock => self.clock = !self.clock,
            Cmd::Stop => self.clock = false,
            Cmd::AssignDrift(v) => self.drift = v,
            Cmd::AssignBias(v) => self.bias = v,
            Cmd::LoadOnset => {
                if let State::BakeRecord(Some(index), ..) = self.state {
                    // assign phrase
                    self.pads[index as usize].phrase = true;
                }
                self.state = State::LoadOnset;
            }
            Cmd::LoadKit(index) => {
                if let Some(index) = index {
                    self.pads = self.scene[index as usize];
                }
                self.state = State::LoadKit(index);
            },
            Cmd::AssignKit(index) => {
                if let Some(index) = index {
                    self.scene[index as usize] = self.pads;
                }
                self.state = State::AssignKit(index)
            },
            Cmd::LoadScene(paths) => self.state = State::LoadScene(paths),
            Cmd::AssignScene(scene) => self.scene = *scene,
            Cmd::LoadWav(paths) => self.state = State::LoadWav(paths),
            Cmd::AssignOnset { name, index, count } => self.state = State::AssignOnset { name, index, count },
            Cmd::BakeRecord(index, len) => self.state = State::BakeRecord(index, len),
            Cmd::Pool => self.state = State::Pool(false),
        }
    }

    fn draw(&self, frame: &mut Frame) {
        frame.render_widget(self, frame.area());
    }

    fn render_load_onset(&self, area: Rect, buf: &mut Buffer) {
        let [pad_area, global_area] =
            Layout::horizontal(vec![Constraint::Max(8), Constraint::Max(14)])
                .flex(Flex::Center)
                .areas(area);

        let block = Block::bordered().bold().padding(Padding::horizontal(1));
        Paragraph::new(Text::raw(String::from_iter(core::array::from_fn::<_, PAD_COUNT, _>(|i| {
            if self.downs.contains(&(i as u8)) {
                'o'
            } else {
                '.'
            }
        }))))
        .block(block)
        .wrap(Wrap { trim: false })
        .render(pad_area, buf);

        let global_text = Text::from(vec![
            Line::raw(format!("drift: {:>3}", self.drift)).italic(),
            Line::raw(format!(" bias: {:>3}", self.bias)).italic(),
        ]);
        Paragraph::new(global_text)
            .left_aligned()
            .block(Block::bordered().bold().padding(Padding::horizontal(1)))
            .render(global_area, buf);
    }

    fn render_load_kit(&self, index: &Option<u8>, area: Rect, buf: &mut Buffer) {
        let [area] = Layout::horizontal(vec![Constraint::Max(14)])
            .flex(Flex::Center)
            .areas(area);
        let mut text: [_; PAD_COUNT] = core::array::from_fn(|i| {
            if self.scene[i].iter().any(|v| v.onsets[0] || v.onsets[1] || v.phrase) {
                'k'
            } else {
                '.'
            }
        });
        if let Some(index) = index {
            text[*index as usize] = 'o';
        }
        Paragraph::new(Text::raw(String::from_iter(text)).centered())
            .block(Block::bordered().bold().title(" load kit: ").padding(Padding::horizontal(4)))
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_assign_kit(&self, index: &Option<u8>, area: Rect, buf: &mut Buffer) {
        let [area] = Layout::horizontal(vec![Constraint::Max(16)])
            .flex(Flex::Center)
            .areas(area);
        let mut text: [_; PAD_COUNT] = core::array::from_fn(|i| {
            if self.scene[i].iter().any(|v| v.onsets[0] || v.onsets[1] || v.phrase) {
                'k'
            } else {
                '.'
            }
        });
        if let Some(index) = index {
            text[*index as usize] = 'o';
        }
        Paragraph::new(Text::raw(String::from_iter(text)).centered())
            .block(Block::bordered().bold().title(" assign kit: ").padding(Padding::horizontal(5)))
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_load_scene(&self, paths: &[String; FILE_COUNT], area: Rect, buf: &mut Buffer) {
        let [pad_area, fs_area] =
            Layout::horizontal(vec![Constraint::Min(8), Constraint::Percentage(100)]).areas(area);
        let [_, arrow_area] = Layout::horizontal(vec![Constraint::Max(7), Constraint::Max(2)])
            .flex(Flex::Start)
            .areas(area);

        Paragraph::new(Text::raw(String::from_iter(core::array::from_fn::<_, PAD_COUNT, _>(|i| {
            if self.downs.contains(&(i as u8)) {
                'o'
            } else if self.scene[i].iter().any(|v| v.onsets[0] || v.onsets[1] || v.phrase) {
                'k'
            } else {
                '.'
            }
        }))))
        .block(
            Block::bordered()
                .bold()
                .padding(Padding::new(1, 1, FILE_COUNT as u16 - 2, 0)),
        )
        .wrap(Wrap { trim: false })
        .render(pad_area, buf);

        let text = if paths.iter().any(|v| !v.is_empty()) {
            let mut lines = paths.clone().map(Line::raw).to_vec();
            let mid = lines.len() / 2;
            lines[mid] = lines[mid].clone().reversed();
            Text::from(lines.to_vec())
        } else {
            Text::raw("no files found")
        };
        Paragraph::new(text)
            .left_aligned()
            .block(
                Block::bordered()
                    .title(" load scene: ")
                    .padding(Padding::horizontal(1)),
            )
            .render(fs_area, buf);

        let text = Text::raw("<<");
        Paragraph::new(text)
            .block(Block::new().padding(Padding::new(0, 0, FILE_COUNT as u16, 1)))
            .render(arrow_area, buf);
    }

    fn render_load_wav(&self, paths: &[String; FILE_COUNT], area: Rect, buf: &mut Buffer) {
        let text = if paths.iter().any(|v| !v.is_empty()) {
            let mut lines = paths.clone().map(Line::raw).to_vec();
            let mid = lines.len() / 2;
            lines[mid] = lines[mid].clone().reversed();
            Text::from(lines.to_vec())
        } else {
            Text::raw("no files found")
        };
        Paragraph::new(text)
            .left_aligned()
            .block(
                Block::bordered()
                    .title(" load wav: ")
                    .padding(Padding::horizontal(1)),
            )
            .render(area, buf);
    }

    fn render_assign_onset(&self, name: &String, index: usize, count: usize, area: Rect, buf: &mut Buffer) {
        let [pad_area, fs_area] =
            Layout::horizontal(vec![Constraint::Min(8), Constraint::Percentage(100)]).areas(area);
        let [_, arrow_area] = Layout::horizontal(vec![Constraint::Max(7), Constraint::Max(2)])
            .flex(Flex::Start)
            .areas(area);

        Paragraph::new(Text::raw(String::from_iter(core::array::from_fn::<_, PAD_COUNT, _>(|i| {
            if self.downs.contains(&(i as u8)) {
                'o'
            } else {
                let pad = self.pads[i];
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
        .block(
            Block::bordered()
                .bold()
                .padding(Padding::new(1, 1, FILE_COUNT as u16 - 2, 0)),
        )
        .wrap(Wrap { trim: false })
        .render(pad_area, buf);

        let mut lines: [Line; FILE_COUNT] = core::array::from_fn(|_| Line::raw(""));
        lines[FILE_COUNT / 2] = Line::raw(name).reversed();
        lines[FILE_COUNT - 1] = Line::raw(format!("{:>3}/{:>3}", index, count));

        let text = Text::from(lines.to_vec());
        Paragraph::new(text)
            .left_aligned()
            .block(
                Block::bordered()
                    .title("  load onset: ")
                    .padding(Padding::horizontal(1)),
            )
            .render(fs_area, buf);

        let text = Text::raw("<<");
        Paragraph::new(text)
            .block(Block::new().padding(Padding::new(0, 0, FILE_COUNT as u16, 1)))
            .render(arrow_area, buf);
    }

    fn render_bake_record(&self, index: &Option<u8>, len: u16, area: Rect, buf: &mut Buffer) {
        let [area] = Layout::horizontal(vec![Constraint::Max(18)])
            .flex(Flex::Center)
            .areas(area);
        let [pad_area, len_area] = Layout::vertical(vec![Constraint::Max(3), Constraint::Max(2)])
            .flex(Flex::SpaceBetween)
            .areas(area);
        let mut text: [_; PAD_COUNT] = core::array::from_fn(|i| {
            if self.downs.contains(&(i as u8)) {
                '-'
            } else {
                '.'
            }
        });
        if let Some(index) = index {
            text[*index as usize] = 'o';
        }
        Block::bordered().bold().title(" assign to pad: ").render(area, buf);
        Paragraph::new(Text::raw(String::from_iter(text)).centered())
            .block(Block::new().padding(Padding::new(7, 7, 1, 0)))
            .wrap(Wrap { trim: false })
            .render(pad_area, buf);
        Paragraph::new(Text::raw(format!("length: {:>3}", len)).right_aligned())
            .block(Block::new().padding(Padding::new(2, 2, 0, 1)))
            .wrap(Wrap { trim: false })
            .render(len_area, buf);
    }

    fn render_pool(&self, area: Rect, buf: &mut Buffer) {
        let [pad_area, pool_area] =
            Layout::horizontal(vec![Constraint::Min(8), Constraint::Percentage(100)]).areas(area);
        let [_, arrow_area] = Layout::horizontal(vec![Constraint::Max(7), Constraint::Max(2)])
            .flex(Flex::Start)
            .areas(area);
        Paragraph::new(Text::raw(String::from_iter(core::array::from_fn::<_, PAD_COUNT, _>(|i| {
            if self.downs.contains(&(i as u8)) {
                'o'
            } else if self.pads[i].phrase {
                'p'
            } else {
                '.'
            }
        }))))
        .block(Block::bordered().bold().padding(Padding::new(1, 1, FILE_COUNT as u16 - 2, 0)))
        .wrap(Wrap { trim: false })
        .render(pad_area, buf);

        Paragraph::new(Text::raw(format!("{:?}", self.pool)).left_aligned())
            .block(Block::bordered().title(" build a sequence: ").padding(Padding::horizontal(1)))
            .wrap(Wrap { trim: false })
            .render(pool_area, buf);
       
        Paragraph::new(Text::raw("<<"))
            .block(Block::new().padding(Padding::new(0, 0, FILE_COUNT as u16, 1)))
            .render(arrow_area, buf);
    }
}

impl Widget for &Tui {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let [area] = Layout::vertical(vec![Constraint::Max(FILE_COUNT as u16 + 4)])
            .flex(Flex::Center)
            .areas(area);
        let [clock_area, area] = Layout::vertical(vec![Constraint::Max(2), Constraint::Max(FILE_COUNT as u16 + 2)])
            .flex(Flex::Center)
            .areas(area);
        let [left, right] = Layout::horizontal(vec![Constraint::Max(11), Constraint::Max(11)]).flex(Flex::Center).areas(clock_area);
        if self.clock {
            Block::new().render(left, buf);
            Block::new().reversed().render(right, buf);
        } else {
            Block::new().reversed().render(left, buf);
            Block::new().render(right, buf);
        }
        match &self.state {
            State::LoadOnset => self.render_load_onset(area, buf),
            State::LoadKit(index) => self.render_load_kit(index, area, buf),
            State::AssignKit(index) => self.render_assign_kit(index, area, buf),
            State::LoadScene(paths) => self.render_load_scene(paths, area, buf),
            State::LoadWav(paths) => self.render_load_wav(paths, area, buf),
            State::AssignOnset { name, index, count } => self.render_assign_onset(name, *index, *count, area, buf),
            State::BakeRecord(index, len) => self.render_bake_record(index, *len, area, buf),
            State::Pool(_) => self.render_pool(area, buf),
        }
    }
}
