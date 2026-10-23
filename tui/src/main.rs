//! sanctum-tui — multi-chat terminal client over the encrypted store.
//! Keys: Tab switch chat · Ctrl-N new invite · paste sanctum://v2/… to join
//! · /file <path> attach · Ctrl-R react · Ctrl-U unsend · Ctrl-S settings
//! · Esc quit.

use std::io;
use std::time::Duration;

use chrono::{Local, TimeZone};
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    DefaultTerminal, Frame,
};
use sanctum_core::contact::{spawn_worker, Cmd, Event};
use sanctum_core::demo::Settings;
use sanctum_core::store::StoredMsg;

const REACTIONS: [&str; 6] = ["👍", "👎", "❤️", "‼️", "❓", "😂"];

struct Msg {
    wire_id: String,
    mine: bool,
    delivered: bool,
    reaction: Option<String>,
    text: String,
    time: String,
    day: String,
}

impl Msg {
    fn from_stored(m: StoredMsg) -> Self {
        let ts = Local.timestamp_millis_opt(m.ts_ms).single().unwrap_or_else(Local::now);
        Self {
            wire_id: m.wire_id,
            mine: m.mine,
            delivered: m.delivered,
            reaction: m.reaction,
            text: {
                let base = if let Some(p) = &m.file_path {
                    format!("[file] {} -> {p}", m.text)
                } else {
                    m.text
                };
                match m.sender_seat {
                    Some(seat) if !m.mine => format!("[m{}] {base}", seat + 1),
                    _ => base,
                }
            },
            time: ts.format("%-I:%M %p").to_string(),
            day: ts.format("%A, %b %-d").to_string(),
        }
    }
}

struct Chat {
    id: i64,
    name: String,
    waiting: bool,
    invite_link: String,
    msgs: Vec<Msg>,
    unread: u32,
}

struct App {
    chats: Vec<Chat>,
    selected: usize,
    status: String,
    settings: Settings,
    show_settings: bool,
    react_mode: bool,
    inviting: bool,
    input: String,
    outbound: std::sync::mpsc::Sender<Cmd>,
    events: std::sync::mpsc::Receiver<Event>,
}

impl App {
    fn new() -> Self {
        let (out_tx, out_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        spawn_worker(out_rx, ev_tx);
        // `sanctum-tui <invite-link>`: join straight from the command line.
        if let Some(link) = std::env::args().nth(1) {
            if link.starts_with("sanctum://v2/") {
                let _ = out_tx.send(Cmd::AcceptInvite { link, name: "New contact".into() });
            }
        }
        Self {
            chats: Vec::new(),
            selected: 0,
            status: "loading…".into(),
            settings: Settings::load(),
            show_settings: false,
            react_mode: false,
            inviting: false,
            input: String::new(),
            outbound: out_tx,
            events: ev_rx,
        }
    }

    fn current(&mut self) -> Option<&mut Chat> {
        self.chats.get_mut(self.selected)
    }

    fn chat_by_id(&mut self, id: i64) -> Option<&mut Chat> {
        self.chats.iter_mut().find(|c| c.id == id)
    }

    fn drain_events(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                Event::Snapshot(all) => {
                    self.chats = all
                        .into_iter()
                        .map(|(c, msgs)| Chat {
                            id: c.id,
                            name: c.name,
                            waiting: c.peer_pk.is_empty(),
                            invite_link: c.invite_link,
                            msgs: msgs.into_iter().map(Msg::from_stored).collect(),
                            unread: 0,
                        })
                        .collect();
                    self.status = "end-to-end encrypted · hippius".into();
                }
                Event::ContactAdded(c) => {
                    self.inviting = false;
                    self.chats.push(Chat {
                        id: c.id,
                        name: c.name,
                        waiting: c.peer_pk.is_empty(),
                        invite_link: c.invite_link,
                        msgs: Vec::new(),
                        unread: 0,
                    });
                    self.selected = self.chats.len() - 1;
                }
                Event::PeerJoined { contact_id } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        c.waiting = false;
                        c.invite_link.clear();
                    }
                }
                Event::Received { contact_id, msg } => {
                    let focused = self.chats.get(self.selected).map(|c| c.id) == Some(contact_id);
                    if let Some(c) = self.chat_by_id(contact_id) {
                        c.msgs.push(Msg::from_stored(msg));
                        if !focused {
                            c.unread += 1;
                        }
                    }
                }
                Event::Delivered { contact_id, wire_id } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        if let Some(m) = c.msgs.iter_mut().find(|m| m.wire_id == wire_id) {
                            m.delivered = true;
                        }
                    }
                }
                Event::Reaction { contact_id, ref_id, emoji } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        if let Some(m) = c.msgs.iter_mut().find(|m| m.wire_id == ref_id) {
                            m.reaction = Some(emoji);
                        }
                    }
                }
                Event::TimerChanged { secs, .. } => {
                    self.settings.disappear_secs = secs;
                }
                Event::Unsent { contact_id, wire_id } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        c.msgs.retain(|m| m.wire_id != wire_id);
                    }
                }
                Event::History { contact_id, msgs } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        c.msgs = msgs.into_iter().map(Msg::from_stored).collect();
                    }
                }
                Event::ContactRemoved { contact_id } => {
                    self.chats.retain(|c| c.id != contact_id);
                    if self.selected >= self.chats.len() {
                        self.selected = self.chats.len().saturating_sub(1);
                    }
                }
                Event::Error(e) => {
                    self.inviting = false;
                    self.status = format!("error: {e}");
                }
            }
        }
    }

    fn submit(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        // Pasting an invite joins it.
        if text.starts_with("sanctum://v2/") || text.starts_with("sanctum://g1/") {
            let n = self.chats.len();
            let _ = self
                .outbound
                .send(Cmd::AcceptInvite { link: text, name: format!("Contact {}", n + 1) });
            self.input.clear();
            return;
        }
        let Some(sel) = self.chats.get(self.selected).map(|c| c.id) else {
            self.status = "no chat selected — Ctrl-N to invite someone".into();
            return;
        };
        if let Some(rest) = text.strip_prefix("/group ") {
            let mut it = rest.splitn(2, ' ');
            let size: i64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(3);
            let name = it.next().unwrap_or("Group").trim().to_string();
            if !self.inviting {
                self.inviting = true;
                let _ = self.outbound.send(Cmd::NewGroup { name, size });
            }
            self.input.clear();
            return;
        }
        if text == "/clear" || text == "/clearweek" {
            let older = (text == "/clearweek").then_some(7 * 86400);
            let _ = self.outbound.send(Cmd::ClearHistory { contact_id: sel, older_than_secs: older });
            self.input.clear();
            return;
        }
        if let Some(name) = text.strip_prefix("/name ") {
            let name = name.trim().to_string();
            if !name.is_empty() {
                let _ = self.outbound.send(Cmd::Rename { contact_id: sel, name: name.clone() });
                if let Some(c) = self.current() {
                    c.name = name;
                }
            }
            self.input.clear();
            return;
        }
        let wire_id = ulid::Ulid::new().to_string();
        if let Some(path) = text.strip_prefix("/file ") {
            let path = path.trim().to_string();
            self.push_own(&wire_id, format!("[file] {path}"));
            let _ = self.outbound.send(Cmd::SendFile { contact_id: sel, wire_id, path });
        } else {
            self.push_own(&wire_id, text.clone());
            let _ = self.outbound.send(Cmd::Send { contact_id: sel, wire_id, text });
        }
        self.input.clear();
    }

    fn push_own(&mut self, wire_id: &str, text: String) {
        let now = Local::now();
        if let Some(c) = self.current() {
            c.msgs.push(Msg {
                wire_id: wire_id.into(),
                mine: true,
                delivered: false,
                reaction: None,
                text,
                time: now.format("%-I:%M %p").to_string(),
                day: now.format("%A, %b %-d").to_string(),
            });
        }
    }

    fn unsend_last(&mut self) {
        let Some(sel) = self.chats.get(self.selected).map(|c| c.id) else { return };
        let wire_id = self.current().and_then(|c| {
            c.msgs.iter().rev().find(|m| m.mine && !m.delivered).map(|m| m.wire_id.clone())
        });
        if let Some(wire_id) = wire_id {
            let _ = self.outbound.send(Cmd::Unsend { contact_id: sel, wire_id });
        }
    }

    fn react_last(&mut self, n: usize) {
        let Some(emoji) = REACTIONS.get(n) else { return };
        let Some(sel) = self.chats.get(self.selected).map(|c| c.id) else { return };
        let ref_id = self.current().and_then(|c| {
            c.msgs.iter_mut().rev().find(|m| !m.mine && !m.wire_id.is_empty()).map(|m| {
                m.reaction = Some((*emoji).to_string());
                m.wire_id.clone()
            })
        });
        if let Some(ref_id) = ref_id {
            let _ = self.outbound.send(Cmd::React {
                contact_id: sel,
                ref_id,
                emoji: (*emoji).to_string(),
            });
        }
    }

    fn draw(&self, f: &mut Frame) {
        let [sidebar, main] =
            Layout::horizontal([Constraint::Length(24), Constraint::Min(30)]).areas(f.area());

        // Sidebar: contact list.
        let mut side: Vec<Line> = vec![Line::from(Span::raw(" Sanctum").bold()), Line::raw("")];
        if self.chats.is_empty() {
            side.push(Line::from(Span::styled(
                " ^N to invite",
                Style::default().fg(Color::DarkGray),
            )));
        }
        for (i, c) in self.chats.iter().enumerate() {
            let marker = if i == self.selected { "▸ " } else { "  " };
            let suffix = if c.waiting {
                " (pending)".to_string()
            } else if c.unread > 0 {
                format!(" ({})", c.unread)
            } else {
                String::new()
            };
            let style = if i == self.selected {
                Style::default().fg(Color::White).bold()
            } else {
                Style::default().fg(Color::Gray)
            };
            side.push(Line::from(Span::styled(format!("{marker}{}{suffix}", c.name), style)));
        }
        if self.inviting {
            const FRAMES: [char; 10] =
                ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let f_idx = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
                / 100) as usize
                % FRAMES.len();
            side.push(Line::from(Span::styled(
                format!("  {} inviting…", FRAMES[f_idx]),
                Style::default().fg(Color::DarkGray),
            )));
        }
        f.render_widget(
            Paragraph::new(side).block(Block::default().borders(Borders::RIGHT)),
            sidebar,
        );

        let [header, body, composer] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .areas(main);

        let title = self
            .chats
            .get(self.selected)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "Sanctum".into());
        let hint = "tab switch · ^N invite · ^R react · ^U unsend · ^S settings · esc quit";
        let top = Line::from(vec![
            Span::raw(" "),
            Span::raw(title).bold(),
            Span::styled(
                format!("{:>w$}", hint, w = (header.width as usize).saturating_sub(12)),
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        let status_line = if self.react_mode {
            format!(
                " react: {}",
                REACTIONS
                    .iter()
                    .enumerate()
                    .map(|(i, e)| format!("{} {e}", i + 1))
                    .collect::<Vec<_>>()
                    .join("  ")
            )
        } else {
            format!(" {}", self.status)
        };
        let sub = Line::from(Span::styled(status_line, Style::default().fg(Color::Green)));
        f.render_widget(Paragraph::new(vec![top, sub]), header);

        if self.show_settings {
            self.draw_settings(f, body, composer);
            return;
        }

        let width = body.width.saturating_sub(2) as usize;
        let mut lines: Vec<Line> = Vec::new();
        if let Some(c) = self.chats.get(self.selected) {
            if c.waiting && !c.invite_link.is_empty() {
                lines.push(Line::from(Span::styled(
                    " Waiting for them to join. Send this invite link:",
                    Style::default().fg(Color::Yellow),
                )));
                for (n, one) in c.invite_link.split('\n').enumerate() {
                    if c.invite_link.contains('\n') {
                        lines.push(Line::from(Span::styled(
                            format!(" Member {}:", n + 2),
                            Style::default().fg(Color::Yellow),
                        )));
                    }
                    for chunk in one.as_bytes().chunks(width.max(20)) {
                        lines.push(Line::raw(String::from_utf8_lossy(chunk).into_owned()));
                    }
                }
                lines.push(Line::raw(""));
            }
            let mut prev_day: Option<&str> = None;
            for m in &c.msgs {
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
                let react = m.reaction.clone().map(|r| format!(" {r}")).unwrap_or_default();
                let line = if m.mine {
                    let receipt = if m.delivered { " ✓" } else { "" };
                    let pad = width.saturating_sub(
                        text.chars().count()
                            + receipt.chars().count()
                            + time.chars().count()
                            + react.chars().count(),
                    );
                    Line::from(vec![
                        Span::raw(" ".repeat(pad)),
                        Span::raw(react.clone()),
                        Span::styled(time, Style::default().fg(Color::DarkGray)),
                        Span::styled(text, style),
                        Span::styled(receipt.to_string(), Style::default().fg(Color::Green)),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled(text, style),
                        Span::raw(react),
                        Span::styled(format!(" {time}"), Style::default().fg(Color::DarkGray)),
                    ])
                };
                lines.push(line);
                lines.push(Line::raw(""));
            }
        }
        let shown = lines.len().saturating_sub(body.height as usize);
        f.render_widget(Paragraph::new(lines[shown..].to_vec()).wrap(Wrap { trim: false }), body);

        f.render_widget(
            Paragraph::new(self.input.as_str()).block(Block::default().borders(Borders::ALL)),
            composer,
        );
        f.set_cursor_position((
            composer.x + 1 + self.input.chars().count() as u16,
            composer.y + 1,
        ));
    }

    fn draw_settings(
        &self,
        f: &mut Frame,
        body: ratatui::layout::Rect,
        composer: ratatui::layout::Rect,
    ) {
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
            Line::raw(format!(
                "  3 · Disappearing msgs     [{}]",
                match self.settings.disappear_secs {
                    0 => "off",
                    60 => "1m",
                    3600 => "1h",
                    _ => "1d",
                }
            )),
            Line::from(Span::styled(
                "  4 · Typing indicators     coming soon",
                Style::default().fg(Color::DarkGray),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                "  press 1/2/3 to toggle · esc to close",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        f.render_widget(Paragraph::new(rows), body);
        f.render_widget(Paragraph::new("").block(Block::default().borders(Borders::ALL)), composer);
    }

    fn run(mut self, mut terminal: DefaultTerminal) -> io::Result<()> {
        loop {
            self.drain_events();
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(250))? {
                if let TermEvent::Key(key) = event::read()? {
                    if self.react_mode {
                        match key.code {
                            KeyCode::Char(c @ '1'..='6') => {
                                self.react_last(c as usize - '1' as usize);
                                self.react_mode = false;
                            }
                            _ => self.react_mode = false,
                        }
                        continue;
                    }
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
                            KeyCode::Char('3') => {
                                self.settings.disappear_secs = match self.settings.disappear_secs {
                                    0 => 60,
                                    60 => 3600,
                                    3600 => 86400,
                                    _ => 0,
                                };
                                self.settings.save();
                                if let Some(c) = self.chats.get(self.selected) {
                                    let _ = self.outbound.send(Cmd::SetTimer {
                                        contact_id: c.id,
                                        secs: self.settings.disappear_secs,
                                    });
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    match (key.code, key.modifiers) {
                        (KeyCode::Esc, _) => return Ok(()),
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(()),
                        (KeyCode::Tab, _) => {
                            if !self.chats.is_empty() {
                                self.selected = (self.selected + 1) % self.chats.len();
                                if let Some(c) = self.current() {
                                    c.unread = 0;
                                }
                            }
                        }
                        (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                            if !self.inviting {
                                self.inviting = true;
                                let n = self.chats.len();
                                let _ = self
                                    .outbound
                                    .send(Cmd::NewInvite { name: format!("Contact {}", n + 1) });
                            }
                        }
                        (KeyCode::Char('u'), KeyModifiers::CONTROL) => self.unsend_last(),
                        (KeyCode::Char('r'), KeyModifiers::CONTROL) => self.react_mode = true,
                        (KeyCode::Char('s'), KeyModifiers::CONTROL) => self.show_settings = true,
                        (KeyCode::Enter, _) => self.submit(),
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
