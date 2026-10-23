//! sanctum-tui — the same chat, in a terminal. Runs anywhere Rust does,
//! no GUI stack needed. Same env config as the desktop app (SANCTUM_ROLE,
//! creds — see experiment/hippius.env.example). Quit: Esc or Ctrl-C.

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event as TermEvent, KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    DefaultTerminal, Frame,
};
use sanctum_core::demo::{spawn_worker, Event};

struct Msg {
    mine: bool,
    text: String,
}

struct App {
    peer: String,
    status: String,
    paired: bool,
    msgs: Vec<Msg>,
    input: String,
    outbound: std::sync::mpsc::Sender<String>,
    events: std::sync::mpsc::Receiver<Event>,
}

impl App {
    fn new() -> Self {
        let role = std::env::var("SANCTUM_ROLE").unwrap_or_else(|_| "alice".into());
        let peer = if role == "alice" { "Bob" } else { "Alice" }.to_string();
        let (out_tx, out_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        spawn_worker(out_rx, ev_tx);
        Self {
            peer,
            status: "connecting to Hippius…".into(),
            paired: false,
            msgs: Vec::new(),
            input: String::new(),
            outbound: out_tx,
            events: ev_rx,
        }
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                Event::Paired => {
                    self.paired = true;
                    self.status = "end-to-end encrypted · hippius".into();
                }
                Event::Received(text) => self.msgs.push(Msg { mine: false, text }),
                Event::Error(e) => self.status = format!("error: {e}"),
            }
        }
    }

    fn send_current(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() || !self.paired {
            return;
        }
        self.msgs.push(Msg { mine: true, text: text.clone() });
        let _ = self.outbound.send(text);
        self.input.clear();
    }

    fn draw(&self, f: &mut Frame) {
        let [header, body, composer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .areas(f.area());

        let title = Line::from(vec![Span::raw(" "), Span::raw(&self.peer).bold()]);
        let sub = Line::from(Span::styled(
            format!(" {}", self.status),
            Style::default().fg(if self.paired { Color::Green } else { Color::DarkGray }),
        ));
        f.render_widget(Paragraph::new(vec![title, sub]), header);

        // Last messages that fit; right-align own messages iMessage-style.
        let width = body.width.saturating_sub(2) as usize;
        let mut lines: Vec<Line> = Vec::new();
        for m in &self.msgs {
            let style = if m.mine {
                Style::default().fg(Color::White).bg(Color::Blue)
            } else {
                Style::default().fg(Color::Black).bg(Color::Gray)
            };
            let text = format!(" {} ", m.text);
            let line = if m.mine {
                let pad = width.saturating_sub(text.chars().count());
                Line::from(vec![Span::raw(" ".repeat(pad)), Span::styled(text, style)])
            } else {
                Line::from(Span::styled(text, style))
            };
            lines.push(line);
            lines.push(Line::raw(""));
        }
        let shown = lines.len().saturating_sub(body.height as usize);
        f.render_widget(
            Paragraph::new(lines[shown..].to_vec()).wrap(Wrap { trim: false }),
            body,
        );

        f.render_widget(
            Paragraph::new(self.input.as_str())
                .block(Block::default().borders(Borders::ALL).title(" iMessage (Enter to send, Esc to quit) ")),
            composer,
        );
    }

    fn run(mut self, mut terminal: DefaultTerminal) -> io::Result<()> {
        loop {
            self.drain_events();
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(250))? {
                if let TermEvent::Key(key) = event::read()? {
                    match (key.code, key.modifiers) {
                        (KeyCode::Esc, _) => return Ok(()),
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(()),
                        (KeyCode::Enter, _) => self.send_current(),
                        (KeyCode::Backspace, _) => {
                            self.input.pop();
                        }
                        (KeyCode::Char(c), _) => self.input.push(c),
                        _ => {}
                    }
                }
            }
        }
    }
}

fn main() -> io::Result<()> {
    let terminal = ratatui::init();
    let result = App::new().run(terminal);
    ratatui::restore();
    result
}
