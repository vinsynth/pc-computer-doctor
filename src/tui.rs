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
    Clear,
    Clock,
    Stop,
    AssignBias(u8),
    AssignDrift(u8),
    Alt(bool),
    Hold(bool),
    Fs(Option<[String; FILE_COUNT]>),
    AssignOnset { name: String, index: usize, count: usize },
    Pad(u8, bool),
    BakeRecord(Option<u8>, u16),
    Pool,
}

#[derive(Default)]
enum State {
    #[default]
    Clear,
    Fs(Option<[String; FILE_COUNT]>),
    AssignOnset {
        name: String,
        index: usize,
        count: usize,
    },
    BakeRecord(Option<u8>, u16),
    Pool(bool),
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
    clock: bool,

    bias: u8,
    drift: u8,

    alt: bool,
    hold: bool,
    pads: [Pad; PAD_COUNT],
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
            Cmd::Clear => {
                if let State::BakeRecord(Some(index), ..) = self.state {
                    // assign phrase
                    self.pads[index as usize].phrase = true;
                }
                self.state = State::Clear;
            },
            Cmd::Clock => self.clock = !self.clock,
            Cmd::Stop => self.clock = false,
            Cmd::AssignBias(v) => self.bias = v,
            Cmd::AssignDrift(v) => self.drift = v,
            Cmd::Alt(v) => self.alt = v,
            Cmd::Hold(v) => self.hold = v,
            Cmd::Fs(paths) => self.state = State::Fs(paths),
            Cmd::AssignOnset { name, index, count } => self.state = State::AssignOnset { name, index, count },
            Cmd::Pad(index, down) => {
                let pad = &mut self.pads[index as usize];
                pad.down = down;
                if down {
                    match &mut self.state {
                        State::AssignOnset { .. } => {
                            if self.alt {
                                pad.onsets[1] = true;
                            } else {
                                pad.onsets[0] = true;
                            }
                        }
                        State::Pool(cleared) => {
                            if !*cleared {
                                *cleared = true;
                                self.pool.clear();
                            }
                            self.pool.push(index)
                        }
                        _ => (),
                    }
                }
            }
            Cmd::BakeRecord(index, len) => self.state = State::BakeRecord(index, len),
            Cmd::Pool => self.state = State::Pool(false),
        }
    }

    fn draw(&self, frame: &mut Frame) {
        frame.render_widget(self, frame.area());
    }

    fn render_state_clear(&self, area: Rect, buf: &mut Buffer) {
        let [pad_area, global_area] =
            Layout::horizontal(vec![Constraint::Max(8), Constraint::Max(14)])
                .flex(Flex::Center)
                .areas(area);

        let block = Block::bordered().bold().padding(Padding::horizontal(1));
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
            Line::raw(format!("drift: {:>3}", self.drift)).italic(),
            Line::raw(format!(" bias: {:>3}", self.bias)).italic(),
        ]);
        Paragraph::new(global_text)
            .left_aligned()
            .block(Block::bordered().bold().padding(Padding::horizontal(1)))
            .render(global_area, buf);
    }

    fn render_state_fs(&self, paths: &Option<[String; 5]>, area: Rect, buf: &mut Buffer) {
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

    fn render_state_assign_onset(
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

    fn render_state_bake_record(&self, index: &Option<u8>, len: &u16, area: Rect, buf: &mut Buffer) {
        let [area] = Layout::horizontal(vec![Constraint::Max(18)])
            .flex(Flex::Center)
            .areas(area);
        let [pad_area, len_area] = Layout::vertical(vec![Constraint::Max(3), Constraint::Max(2)])
            .flex(Flex::SpaceBetween)
            .areas(area);
        let mut text = String::from_iter(self.pads.iter().map(|p| {
            if p.down {
                '-'
            } else {
                '.'
            }
        }));
        if let Some(index) = index {
            text.replace_range(*index as usize..=*index as usize, "o");
        }
        Block::bordered().bold().title(" assign to pad: ").render(pad_area, buf);
        Paragraph::new(Text::raw(text).centered())
            .wrap(Wrap { trim: true })
            .render(pad_area, buf);
        Paragraph::new(Text::raw(format!("length: {:>3}", len)).centered())
            .wrap(Wrap { trim: true })
            .render(len_area, buf);
    }

    fn render_state_pool(&self, area: Rect, buf: &mut Buffer) {
        let [pad_area, pool_area] =
            Layout::horizontal(vec![Constraint::Min(8), Constraint::Percentage(100)]).areas(area);
        let [_, arrow_area] = Layout::horizontal(vec![Constraint::Max(7), Constraint::Max(2)])
            .flex(Flex::Start)
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
        .block(Block::bordered().bold().padding(Padding::new(1, 1, FILE_COUNT as u16 - 2, 0)))
        .wrap(Wrap { trim: true })
        .render(pad_area, buf);

        Paragraph::new(Text::raw(format!("{:?}", self.pool)).left_aligned())
            .block(Block::bordered().title(" build a sequence: ").padding(Padding::horizontal(1)))
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
            State::Clear => self.render_state_clear(area, buf),
            State::Fs(paths) => self.render_state_fs(paths, area, buf),
            State::AssignOnset { name, index, count } => {
                self.render_state_assign_onset(name, index, count, area, buf)
            }
            State::BakeRecord(index, len) => self.render_state_bake_record(index, len, area, buf),
            State::Pool(_) => self.render_state_pool(area, buf),
        }
    }
}
