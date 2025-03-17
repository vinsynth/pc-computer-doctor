use crate::audio::PAD_COUNT;
use crate::input::Global;

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
    Clear,
    Start,
    Clock,
    Global(Global),
    Alt(bool),
    Pad(u8, bool),
    Ghost(bool),
    Sequence(bool),
    Record,
    Phrase,
    Dir(Option<[String; FILE_COUNT]>),
    File {
        name: String,
        index: usize,
        count: usize,
    },
}

#[derive(Default)]
enum State {
    #[default]
    None,
    Ghost([u8; PAD_COUNT]),
    Sequence([u8; PAD_COUNT]),
    Dir(Option<[String; FILE_COUNT]>),
    File {
        path: String,
        index: usize,
        count: usize,
    },
    Phrase,
}

#[derive(Default)]
struct Pad {
    down: bool,
    onsets: [bool; 2],
    phrase: bool,
}

#[derive(Default)]
pub struct Tui {
    exit: bool,
    pads: [Pad; PAD_COUNT],
    alt: bool,
    clock: bool,
    recording: bool,
    bias: u8,
    roll: u8,
    drift: u8,
    width: u8,
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
            Cmd::Clear => self.state = State::None,
            Cmd::Start => self.clock = false,
            Cmd::Clock => self.clock = !self.clock,
            Cmd::Global(global) => match global {
                Global::Bias(value) => self.bias = value,
                Global::Roll(value) => self.roll = value,
                Global::Drift(value) => self.drift = value,
                Global::Width(value) => self.width = value,
            },
            Cmd::Alt(alt) => self.alt = alt,
            Cmd::Pad(index, down) => {
                let pad = &mut self.pads[index as usize];
                pad.down = down;
                match self.state {
                    State::Dir(_) => self.state = State::None,
                    State::File { .. } => {
                        if self.alt {
                            pad.onsets[1] = true;
                        } else {
                            pad.onsets[0] = true;
                        }
                    }
                    State::Phrase => {
                        if down {
                            pad.phrase = true;
                        } else {
                            self.state = State::None;
                        }
                    }
                    State::Ghost(ref mut weights) => {
                        if down {
                            weights[index as usize] = (weights[index as usize] + 1).min(15);
                        }
                    }
                    State::Sequence(ref mut weights) => {
                        if down {
                            weights[index as usize] = (weights[index as usize] + 1).min(15);
                        }
                    }
                    _ => (),
                }
            }
            Cmd::Ghost(down) => {
                if down {
                    self.state = State::Ghost([0; PAD_COUNT]);
                } else {
                    self.state = State::None;
                }
            }
            Cmd::Sequence(down) => {
                if down {
                    self.state = State::Sequence([0; PAD_COUNT]);
                } else {
                    self.state = State::None;
                }
            }
            Cmd::Record => self.recording = true,
            Cmd::Phrase => {
                self.state = State::Phrase;
                self.recording = false;
            }
            Cmd::Dir(paths) => self.state = State::Dir(paths),
            Cmd::File {
                name: path,
                index,
                count,
            } => {
                self.state = State::File { path, index, count };
            }
        }
    }

    fn draw(&self, frame: &mut Frame) {
        frame.render_widget(self, frame.area());
    }

    fn render_state_none(&self, area: Rect, buf: &mut Buffer) {
        let [pad_area, global_area] =
            Layout::horizontal(vec![Constraint::Max(8), Constraint::Max(14)])
                .flex(Flex::Center)
                .areas(area);

        let mut block = Block::bordered().bold().padding(Padding::horizontal(1));
        if self.recording {
            block = block.reversed();
        }
        Paragraph::new(Text::raw(String::from_iter(self.pads.iter().map(|p| {
            if p.down {
                'o'
            } else {
                '.'
            }
        }))))
        .block(block)
        .wrap(Wrap { trim: true })
        .render(pad_area, buf);

        let global_text = Text::from(vec![
            Line::raw(format!(" bias: {:>3}", self.bias)).italic(),
            Line::raw(format!(" roll: {:>3}", self.roll)).italic(),
            Line::raw(format!("drift: {:>3}", self.drift)).italic(),
            Line::raw(format!("width: {:>3}", self.width)).italic(),
        ]);
        Paragraph::new(global_text)
            .left_aligned()
            .block(Block::bordered().bold().padding(Padding::horizontal(1)))
            .render(global_area, buf);
    }

    fn render_state_ghost(&self, weights: &[u8; 8], area: Rect, buf: &mut Buffer) {
        let [pad_area] = Layout::horizontal(vec![Constraint::Max(14)])
            .flex(Flex::Center)
            .areas(area);
        Paragraph::new(Text::raw(String::from_iter(
            weights.iter().map(|v| format!("{:x}", v)),
        )))
        .block(
            Block::bordered()
                .bold()
                .title(" ghost pool ")
                .padding(Padding::horizontal(4)),
        )
        .wrap(Wrap { trim: true })
        .render(pad_area, buf);
    }

    fn render_state_sequence(&self, weights: &[u8; 8], area: Rect, buf: &mut Buffer) {
        let [pad_area] = Layout::horizontal(vec![Constraint::Max(16)])
            .flex(Flex::Center)
            .areas(area);
        Paragraph::new(Text::raw(String::from_iter(
            weights.iter().map(|v| format!("{:x}", v)),
        )))
        .block(
            Block::bordered()
                .bold()
                .title(" pattern pool ")
                .padding(Padding::horizontal(5)),
        )
        .wrap(Wrap { trim: true })
        .render(pad_area, buf);
    }

    fn render_state_phrase(&self, area: Rect, buf: &mut Buffer) {
        let [pad_area] = Layout::horizontal(vec![Constraint::Max(18)])
            .flex(Flex::Center)
            .areas(area);
        Paragraph::new(Text::raw(String::from_iter(self.pads.iter().map(|p| {
            if p.down {
                'o'
            } else if p.phrase {
                'p'
            } else {
                '.'
            }
        }))))
        .block(
            Block::bordered()
                .bold()
                .title(" assign to pad: ")
                .padding(Padding::horizontal(6)),
        )
        .wrap(Wrap { trim: true })
        .render(pad_area, buf);
    }

    fn render_state_dir(&self, paths: &Option<[String; 5]>, area: Rect, buf: &mut Buffer) {
        let text = if let Some(paths) = paths {
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
                    .title(" select a file: ")
                    .padding(Padding::horizontal(1)),
            )
            .render(area, buf);
    }

    fn render_state_file(
        &self,
        path: &str,
        index: &usize,
        count: &usize,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let [pad_area, fs_area] =
            Layout::horizontal(vec![Constraint::Min(8), Constraint::Percentage(100)]).areas(area);
        let [_, arrow_area] = Layout::horizontal(vec![Constraint::Max(7), Constraint::Max(2)])
            .flex(Flex::Start)
            .areas(area);

        Paragraph::new(Text::raw(String::from_iter(self.pads.iter().map(|p| {
            if p.down {
                'o'
            } else if p.onsets[0] && p.onsets[1] {
                '@'
            } else if p.onsets[0] {
                'a'
            } else if p.onsets[1] {
                'b'
            } else {
                '.'
            }
        }))))
        .block(
            Block::bordered()
                .bold()
                .padding(Padding::new(1, 1, FILE_COUNT as u16 - 2, 0)),
        )
        .wrap(Wrap { trim: true })
        .render(pad_area, buf);

        let mut lines: [Line; FILE_COUNT] = core::array::from_fn(|_| Line::raw(""));
        lines[FILE_COUNT / 2] = Line::raw(path).reversed();
        lines[FILE_COUNT - 1] = Line::raw(format!("{:>3}/{:>3}", index, count));

        let text = Text::from(lines.to_vec());
        Paragraph::new(text)
            .left_aligned()
            .block(
                Block::bordered()
                    .title(" select an onset: ")
                    .padding(Padding::horizontal(1)),
            )
            .render(fs_area, buf);

        let text = Text::raw("<<");
        Paragraph::new(text)
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
            State::None => self.render_state_none(area, buf),
            State::Dir(paths) => self.render_state_dir(paths, area, buf),
            State::File { path, index, count } => {
                self.render_state_file(path, index, count, area, buf)
            }
            State::Phrase => self.render_state_phrase(area, buf),
            State::Ghost(weights) => self.render_state_ghost(weights, area, buf),
            State::Sequence(weights) => self.render_state_sequence(weights, area, buf),
        }
    }
}
