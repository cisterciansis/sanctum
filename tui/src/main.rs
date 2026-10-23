//! sanctum-tui — the same chat, in a terminal. Runs anywhere Rust does,
//! no GUI stack needed. Same env config as the desktop app (SANCTUM_ROLE,
//! creds — see experiment/hippius.env.example).
//! Keys: Enter send · Ctrl-U unsend last · Ctrl-T toggle timestamps · Esc quit.

use std::io;
use std::time::Duration;

use chrono::Local;
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    DefaultTerminal, Frame,
};
use sanctum_core::demo::{spawn_worker, Cmd, Event, Settings};

struct Msg {
    id: u64,
    mine: bool,
    delivered: bool,
    text: String,
    time: String,
    day: String,
}

impl Msg {
    fn now(id: u64, mine: bool, text: String) -> Self {
        let now = Local::now();
        Self {
            id,
            mine,
            delivered: false,
            text,
            time: now.format("%-I:%M %p").to_string(),
            day: now.format("%A, %b %-d").to_string(),
        }
    }
}

struct App {
    peer: String,
    status: String,
    paired: bool,
    msgs: Vec<Msg>,
    next_id: u64,
    settings: Settings,
    show_settings: bool,
    input: String,
    outbound: std::sync::mpsc::Sender<Cmd>,
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
            next_id: 0,
            settings: Settings::load(),
            show_settings: false,
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
                Event::Received(text) => self.msgs.push(Msg::now(u64::MAX, false, text)),
                Event::Delivered(id) => {
                    if let Some(m) = self.msgs.iter_mut().find(|m| m.mine && m.id == id) {
                        m.delivered = true;
                    }
                }
                Event::Unsent(id) => self.msgs.retain(|m| !(m.mine && m.id == id)),
                Event::Error(e) => self.status = format!("error: {e}"),
            }
        }
    }

    fn send_current(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() || !self.paired {
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.msgs.push(Msg::now(id, true, text.clone()));
        let _ = self.outbound.send(Cmd::Send(id, text));
        self.input.clear();
    }

    fn unsend_last(&mut self) {
        if let Some(m) = self.msgs.iter().rev().find(|m| m.mine && !m.delivered) {
            let _ = self.outbound.send(Cmd::Unsend(m.id));
        }
    }

    fn draw(&self, f: &mut Frame) {
        let [header, body, composer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .areas(f.area());

        let hint = "enter send · ^U unsend · ^S settings · esc quit";
        let title = Line::from(vec![
            Span::raw(" "),
            Span::raw(&self.peer).bold(),
            Span::styled(
                format!(
                    "{:>width$}",
                    hint,
                    width = (header.width as usize).saturating_sub(self.peer.len() + 1)
                ),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        let sub = Line::from(Span::styled(
            format!(" {}", self.status),
            Style::default().fg(if self.paired { Color::Green } else { Color::DarkGray }),
        ));
        f.render_widget(Paragraph::new(vec![title, sub]), header);

        if self.show_settings {
            let on = |b: bool| if b { "[on] " } else { "[off]" };
            let rows = vec![
                Line::raw(""),
                Line::from(Span::raw("  Settings").bold()),
                Line::raw(""),
                Line::raw(format!("  1 · Timestamps            {}", on(self.settings.timestamps))),
                Line::raw(format!(
                    "  2 · Proxy (Tor / UR)      {}  {} · applies on restart",
                    on(self.settings.proxy_enabled),
                    self.settings.proxy_url
                )),
                Line::from(Span::styled(
                    "  3 · Typing indicators     coming soon",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "  press 1/2 to toggle · esc to close",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            f.render_widget(Paragraph::new(rows), body);
            f.render_widget(
                Paragraph::new("").block(Block::default().borders(Borders::ALL)),
                composer,
            );
            return;
        }

        let width = body.width.saturating_sub(2) as usize;
        let mut lines: Vec<Line> = Vec::new();
        let mut prev_day: Option<&str> = None;
        for m in &self.msgs {
            if prev_day != Some(m.day.as_str()) {
                prev_day = Some(m.day.as_str());
                let pad = width.saturating_sub(m.day.len()) / 2;
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(pad)),
                    Span::styled(m.day.clone(), Style::default().fg(Color::DarkGray)),
                ]));
            }
            let style = if m.mine {
                Style::default().fg(Color::White).bg(Color::Blue)
            } else {
                Style::default().fg(Color::Black).bg(Color::Gray)
            };
            let text = format!(" {} ", m.text);
            let time = if self.settings.timestamps {
                format!("{} ", m.time)
            } else {
                String::new()
            };
            let line = if m.mine {
                let receipt = if m.delivered { " ✓" } else { "" };
                let pad = width
                    .saturating_sub(text.chars().count() + receipt.chars().count() + time.chars().count());
                Line::from(vec![
                    Span::raw(" ".repeat(pad)),
                    Span::styled(time, Style::default().fg(Color::DarkGray)),
                    Span::styled(text, style),
                    Span::styled(receipt.to_string(), Style::default().fg(Color::Green)),
                ])
            } else {
                Line::from(vec![
                    Span::styled(text, style),
                    Span::styled(format!(" {time}"), Style::default().fg(Color::DarkGray)),
                ])
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
            Paragraph::new(self.input.as_str()).block(Block::default().borders(Borders::ALL)),
            composer,
        );
        f.set_cursor_position((
            composer.x + 1 + self.input.chars().count() as u16,
            composer.y + 1,
        ));
    }

    fn run(mut self, mut terminal: DefaultTerminal) -> io::Result<()> {
        loop {
            self.drain_events();
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(250))? {
                if let TermEvent::Key(key) = event::read()? {
                    if self.show_settings {
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('q') => self.show_settings = false,
                            KeyCode::Char('1') => {
                                self.settings.timestamps = !self.settings.timestamps;
                                self.settings.save();
                            }
                            KeyCode::Char('2') => {
                                self.settings.proxy_enabled = !self.settings.proxy_enabled;
                                self.settings.save();
                            }
                            _ => {}
                        }
                        continue;
                    }
                    match (key.code, key.modifiers) {
                        (KeyCode::Esc, _) => return Ok(()),
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(()),
                        (KeyCode::Char('u'), KeyModifiers::CONTROL) => self.unsend_last(),
                        (KeyCode::Char('s'), KeyModifiers::CONTROL) => self.show_settings = true,
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
