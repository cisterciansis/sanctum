//! sanctum-tui — multi-chat terminal client over the encrypted store.
//! Keys: Tab switch chat · Ctrl-N new invite · paste sanctum://v2/… to join
//! · /file <path> attach · Ctrl-R react · Ctrl-U unsend · Ctrl-S settings
//! (also lists every command) · /delete removes a chat · Ctrl-Y copies the
//! invite link · /timer sets a custom disappearing timer · Esc quit.

use std::io;
use std::time::Duration;

use chrono::{Local, TimeZone};
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    DefaultTerminal, Frame,
};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol, Resize, StatefulImage};
use sanctum_core::contact::{spawn_worker, Cmd, Event};
use sanctum_core::demo::{self, Settings};
use sanctum_core::store::StoredMsg;

const REACTIONS: [&str; 6] = ["👍", "👎", "❤️", "‼️", "❓", "😂"];

struct Msg {
    wire_id: String,
    mine: bool,
    delivered: bool,
    /// Never left this device (only known for this session's sends).
    failed: bool,
    /// Own message the peer has read (read receipts on).
    read: bool,
    reaction: Option<String>,
    /// Local path of an attachment (received copy, or the file we sent).
    file: Option<String>,
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
            failed: false,
            read: m.mine && m.read_ms.is_some(),
            reaction: m.reaction,
            file: m.file_path.clone(),
            text: {
                let base = if let Some(p) = &m.file_path {
                    // Local save path, shortened: ~ instead of /Users/<you>.
                    let home = std::env::var("HOME").unwrap_or_default();
                    let p = match p.strip_prefix(&home) {
                        Some(rest) if !home.is_empty() => format!("~{rest}"),
                        _ => p.clone(),
                    };
                    format!("[file] {} → {p}", m.text)
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

/// The resting status line.
const IDLE: &str = "end-to-end encrypted · hippius";

/// Shown under Settings (Ctrl-S); keep in step with `submit` and `run`.
const COMMANDS: &[(&str, &str)] = &[
    ("Ctrl-N", "new invite link (copied for you)"),
    ("Ctrl-Y", "copy this chat's invite link"),
    ("sanctum://…", "paste an invite to join"),
    ("Tab", "next chat"),
    ("Ctrl-R 1-6", "react to the last message"),
    ("Ctrl-U", "unsend your last undelivered message"),
    ("Ctrl-S", "settings and this list"),
    ("Esc", "quit"),
    ("/file <path>", "send a file (up to 25 MB)"),
    ("Ctrl-O or /open", "open the newest attachment (photos, PDFs, media)"),
    ("/reveal", "show the newest attachment in Finder"),
    ("/name <name>", "rename this chat"),
    ("/group <size> <name>", "new group (2-6 seats)"),
    ("/clear", "clear this chat's history"),
    ("/clearweek", "clear messages older than a week"),
    ("/del", "delete your last message for everyone"),
    ("/delme", "delete the last message from this device"),
    ("/delete", "delete this chat (asks to confirm)"),
    ("/timer <time>", "disappearing: off, 90m, 2w, 6mo... (1 min to 1 year)"),
];

/// One transcript row: a line of text, or an inline image preview.
enum Row<'a> {
    Text(Line<'a>),
    Image { path: String, mine: bool },
}

const PREVIEW_ROWS: u16 = 12;
const PREVIEW_COLS: u16 = 48;

fn is_image(path: &str) -> bool {
    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("");
    ["jpg", "jpeg", "png", "gif", "webp"].contains(&ext.to_ascii_lowercase().as_str())
}

/// Decode a received image for preview. It's untrusted input: the decoders
/// are memory-safe Rust, and limits stop a decompression bomb. EXIF rotation
/// is applied (iPhone photos are often stored sideways).
fn load_preview(path: &str) -> Option<image::DynamicImage> {
    use image::ImageDecoder;
    let mut reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(12_000);
    limits.max_image_height = Some(12_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder.orientation().ok()?;
    let mut img = image::DynamicImage::from_decoder(decoder).ok()?;
    img.apply_orientation(orientation);
    Some(img.thumbnail(800, 800)) // plenty for a terminal; keeps the cache small
}

/// File types opened straight in their default app. Peers pick attachment
/// names, so anything else (a `.command` script, an `.app`…) is only shown
/// in Finder, never launched.
const OPENABLE: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "heic", "heif", "webp", "bmp", "tiff", "pdf", "txt", "md",
    "mp4", "mov", "m4v", "mp3", "m4a", "wav", "aac",
];

fn open_file(path: &str, reveal: bool) -> Result<&'static str, String> {
    use std::process::{Command, Stdio};
    let p = std::path::Path::new(path);
    if !p.is_file() {
        return Err("attachment file is missing".into());
    }
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let reveal = reveal || !OPENABLE.contains(&ext.as_str());
    let mac = cfg!(target_os = "macos");
    let mut cmd = Command::new(if mac { "open" } else { "xdg-open" });
    match (reveal, mac) {
        (true, true) => cmd.arg("-R").arg(p),
        (true, false) => cmd.arg(p.parent().unwrap_or(p)),
        (false, _) => cmd.arg(p),
    };
    cmd.stdout(Stdio::null()).stderr(Stdio::null()).status().map_err(|e| e.to_string())?;
    Ok(if reveal { "shown in Finder" } else { "opened" })
}

/// System clipboard via the platform tool; else OSC 52, which most
/// terminals (iTerm2, kitty, WezTerm, Ghostty, tmux) honour, even over SSH.
fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let tools: &[(&str, &[&str])] =
        &[("pbcopy", &[]), ("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])];
    for (tool, args) in tools {
        let Ok(mut child) = Command::new(tool)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let wrote = child.stdin.take().is_some_and(|mut i| i.write_all(text.as_bytes()).is_ok());
        if wrote && child.wait().is_ok_and(|s| s.success()) {
            return true;
        }
    }
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(text);
    let mut out = io::stdout();
    write!(out, "\x1b]52;c;{b64}\x07").and_then(|_| out.flush()).is_ok()
}

struct App {
    chats: Vec<Chat>,
    selected: usize,
    status: String,
    /// When `status` last changed; notes fade back to IDLE after a few seconds.
    status_since: (String, std::time::Instant),
    settings: Settings,
    show_settings: bool,
    react_mode: bool,
    inviting: bool,
    /// Terminal has focus, so the selected chat is actually being looked at
    /// (for read receipts). Terminals without focus reporting stay true.
    focused: bool,
    /// Chat armed by a first `/delete`; a second one confirms.
    pending_delete: Option<i64>,
    input: String,
    outbound: std::sync::mpsc::Sender<Cmd>,
    events: std::sync::mpsc::Receiver<Event>,
    /// Terminal image protocol (kitty / iTerm2 / sixel, else half-blocks).
    picker: Picker,
    /// Decoded previews by path; None = not a decodable image.
    previews: std::cell::RefCell<std::collections::HashMap<String, Option<StatefulProtocol>>>,
}

impl App {
    fn new(picker: Picker) -> Self {
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
            status_since: (String::new(), std::time::Instant::now()),
            settings: Settings::load(),
            show_settings: false,
            react_mode: false,
            inviting: false,
            pending_delete: None,
            focused: true,
            input: String::new(),
            outbound: out_tx,
            events: ev_rx,
            picker,
            previews: Default::default(),
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
                    self.status = IDLE.into();
                    self.mark_read();
                }
                Event::ContactAdded(c) => {
                    let ours = std::mem::take(&mut self.inviting) && !c.invite_link.is_empty();
                    self.chats.push(Chat {
                        id: c.id,
                        name: c.name,
                        waiting: c.peer_pk.is_empty(),
                        invite_link: c.invite_link,
                        msgs: Vec::new(),
                        unread: 0,
                    });
                    self.selected = self.chats.len() - 1;
                    if ours {
                        self.copy_invite();
                    }
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
                    if focused {
                        self.mark_read();
                    }
                }
                Event::Read { contact_id, wire_ids, .. } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        for m in c.msgs.iter_mut().filter(|m| wire_ids.contains(&m.wire_id)) {
                            m.read = true;
                            m.delivered = true;
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
                // The TUI spins until delivered, so upload completion adds nothing.
                Event::Sent { .. } => {}
                Event::SendFailed { contact_id, wire_id, error } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        if let Some(m) = c.msgs.iter_mut().find(|m| m.wire_id == wire_id) {
                            m.failed = true;
                        }
                    }
                    self.status = format!("error: not sent: {error}");
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
                Event::ContactUpdated(c) => {
                    if let Some(chat) = self.chat_by_id(c.id) {
                        chat.waiting = c.peer_pk.is_empty();
                        chat.invite_link = c.invite_link;
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
            self.inviting = true;
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
        // Deleting retires the chat's buckets for good, so it takes two.
        let armed = self.pending_delete.take();
        if text == "/delete" {
            if armed == Some(sel) {
                let _ = self.outbound.send(Cmd::DeleteContact { contact_id: sel });
                self.status = format!("deleted '{}'", self.chats[self.selected].name);
            } else {
                self.pending_delete = Some(sel);
                let name = self.chats[self.selected].name.clone();
                self.status = format!("delete '{name}' and its history? /delete again to confirm");
            }
            self.input.clear();
            return;
        }
        if let Some(t) = text.strip_prefix("/timer ") {
            match demo::parse_disappear(t) {
                Some(secs) => self.set_timer(secs),
                None => self.status = "timer: off, or 1 minute to 1 year, e.g. 90m, 2w, 6mo".into(),
            }
            self.input.clear();
            return;
        }
        if text == "/open" || text == "/reveal" {
            self.open_attachment(text == "/reveal");
            self.input.clear();
            return;
        }
        if text == "/del" || text == "/delme" {
            let everyone = text == "/del";
            // No message cursor in the TUI: act on the newest eligible one.
            let target = self.current().and_then(|c| {
                c.msgs
                    .iter()
                    .rev()
                    .find(|m| !m.wire_id.is_empty() && (!everyone || (m.mine && !m.failed)))
                    .map(|m| m.wire_id.clone())
            });
            match target {
                Some(wire_id) => {
                    let _ = self.outbound.send(Cmd::DeleteMsg { contact_id: sel, wire_id, everyone });
                }
                None => self.status = "no message to delete".into(),
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
            if let Some(m) = self.current().and_then(|c| c.msgs.last_mut()) {
                m.file = Some(path.clone());
            }
            let _ = self.outbound.send(Cmd::SendFile { contact_id: sel, wire_id, path });
        } else {
            self.push_own(&wire_id, text.clone());
            let _ = self.outbound.send(Cmd::Send { contact_id: sel, wire_id, text });
        }
        self.input.clear();
    }

    /// Open the chat's newest attachment, or show it in Finder.
    fn open_attachment(&mut self, reveal: bool) {
        let path = self
            .chats
            .get(self.selected)
            .and_then(|c| c.msgs.iter().rev().find_map(|m| m.file.clone()));
        self.status = match path {
            None => "no attachment in this chat".into(),
            Some(p) => match open_file(&p, reveal) {
                Ok(what) => what.into(),
                Err(e) => format!("error: {e}"),
            },
        };
    }

    /// Tell the worker the selected chat is on screen (no-op when unfocused).
    fn mark_read(&self) {
        if let (true, Some(c)) = (self.focused, self.chats.get(self.selected)) {
            let _ = self.outbound.send(Cmd::MarkRead { contact_id: c.id });
        }
    }

    /// Copy the selected chat's (first open) invite link to the clipboard.
    fn copy_invite(&mut self) {
        let link = self.chats.get(self.selected).and_then(|c| {
            c.invite_link.split('\n').find(|l| !l.is_empty()).map(str::to_string)
        });
        self.status = match link {
            Some(link) if copy_to_clipboard(&link) => "invite link copied — paste it to them".into(),
            Some(_) => "couldn't reach the clipboard".into(),
            None => "this chat has no open invite".into(),
        };
    }

    /// Apply a validated timer locally and sync it to the selected chat.
    fn set_timer(&mut self, secs: u64) {
        self.settings.disappear_secs = secs;
        self.settings.save();
        if let Some(c) = self.chats.get(self.selected) {
            let _ = self.outbound.send(Cmd::SetTimer { contact_id: c.id, secs });
        }
        self.status = format!("disappearing messages: {}", demo::disappear_label(secs));
    }

    fn push_own(&mut self, wire_id: &str, text: String) {
        let now = Local::now();
        if let Some(c) = self.current() {
            c.msgs.push(Msg {
                wire_id: wire_id.into(),
                mine: true,
                delivered: false,
                failed: false,
                read: false,
                reaction: None,
                file: None,
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
        let hint = "tab switch · ^N invite · ^R react · ^U unsend · ^S settings & commands · esc quit";
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
        let mut lines: Vec<Row> = Vec::new();
        if let Some(c) = self.chats.get(self.selected) {
            if !c.invite_link.trim().is_empty() {
                lines.push(Row::Text(Line::from(Span::styled(
                    " Waiting for them to join. Send this invite link (Ctrl-Y copies it):",
                    Style::default().fg(Color::Yellow),
                ))));
                for (n, one) in c.invite_link.split('\n').enumerate() {
                    if one.is_empty() {
                        continue; // seat already claimed
                    }
                    if c.invite_link.contains('\n') {
                        lines.push(Row::Text(Line::from(Span::styled(
                            format!(" Member {}:", n + 2),
                            Style::default().fg(Color::Yellow),
                        ))));
                    }
                    for chunk in one.as_bytes().chunks(width.max(20)) {
                        lines.push(Row::Text(Line::raw(String::from_utf8_lossy(chunk).into_owned())));
                    }
                }
                lines.push(Row::Text(Line::raw("")));
            }
            // Advances on each redraw (~4/s): a micro "sending" spinner.
            const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            let spin = SPIN[(ms / 100 % SPIN.len() as u128) as usize];
            let mut prev_day: Option<&str> = None;
            for m in &c.msgs {
                if prev_day != Some(m.day.as_str()) {
                    prev_day = Some(m.day.as_str());
                    let pad = width.saturating_sub(m.day.len()) / 2;
                    lines.push(Row::Text(Line::from(vec![
                        Span::raw(" ".repeat(pad)),
                        Span::styled(m.day.clone(), Style::default().fg(Color::DarkGray)),
                    ])));
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
                    // Groups have no receipts yet, so no spinner there.
                    let (receipt, color) = if m.failed {
                        (" ! not sent".to_string(), Color::Red)
                    } else if m.read && self.settings.read_receipts {
                        (" ✓✓ read".to_string(), Color::Cyan)
                    } else if m.delivered {
                        (" ✓".to_string(), Color::Green)
                    } else if c.id >= 0 {
                        (format!(" {spin}"), Color::DarkGray)
                    } else {
                        (String::new(), Color::DarkGray)
                    };
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
                        Span::styled(receipt, Style::default().fg(color)),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled(text, style),
                        Span::raw(react),
                        Span::styled(format!(" {time}"), Style::default().fg(Color::DarkGray)),
                    ])
                };
                lines.push(Row::Text(line));
                if let Some(path) = m.file.as_ref().filter(|p| is_image(p)) {
                    lines.push(Row::Image { path: path.clone(), mine: m.mine });
                }
                lines.push(Row::Text(Line::raw("")));
            }
        }
        // Lay rows out bottom-up so the newest stay in view; a row that
        // doesn't fit whole is left off the top.
        let mut y = body.bottom();
        let mut placed = Vec::new();
        for row in lines.into_iter().rev() {
            let h = match &row {
                Row::Text(l) => {
                    Paragraph::new(l.clone()).wrap(Wrap { trim: false }).line_count(body.width) as u16
                }
                Row::Image { .. } => PREVIEW_ROWS,
            };
            if h > y - body.y {
                break;
            }
            y -= h;
            placed.push((Rect { y, height: h, ..body }, row));
        }
        for (area, row) in placed {
            match row {
                Row::Text(l) => f.render_widget(Paragraph::new(l).wrap(Wrap { trim: false }), area),
                Row::Image { path, mine } => self.draw_preview(f, area, &path, mine),
            }
        }

        f.render_widget(
            Paragraph::new(self.input.as_str()).block(Block::default().borders(Borders::ALL)),
            composer,
        );
        f.set_cursor_position((
            composer.x + 1 + self.input.chars().count() as u16,
            composer.y + 1,
        ));
    }

    /// An image attachment, fitted into a PREVIEW_COLS × PREVIEW_ROWS box on
    /// its sender's side. Decoded once, then cached.
    fn draw_preview(&self, f: &mut Frame, area: Rect, path: &str, mine: bool) {
        let mut cache = self.previews.borrow_mut();
        let entry = cache
            .entry(path.to_string())
            .or_insert_with(|| load_preview(path).map(|img| self.picker.new_resize_protocol(img)));
        let Some(state) = entry.as_mut() else { return };
        let boxed = Rect { width: area.width.min(PREVIEW_COLS), ..area };
        let fit = state.size_for(Resize::Fit(None), boxed);
        let x = if mine { area.right().saturating_sub(fit.width) } else { area.x };
        f.render_stateful_widget(StatefulImage::default(), Rect { x, ..fit }, state);
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
                "  3 · Disappearing msgs     [{}]  · /timer <time> for custom",
                demo::disappear_label(self.settings.disappear_secs)
            )),
            Line::raw(format!(
                "  4 · Read receipts         {}  send yours and see theirs",
                on(self.settings.read_receipts)
            )),
            Line::from(Span::styled(
                "  5 · Typing indicators     coming soon",
                Style::default().fg(Color::DarkGray),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                "  press 1-4 to toggle · esc to close",
                Style::default().fg(Color::DarkGray),
            )),
            Line::raw(""),
            Line::from(Span::raw("  Commands").bold()),
            Line::raw(""),
        ];
        let rows = rows.into_iter().chain(COMMANDS.iter().map(|(cmd, what)| {
            Line::from(vec![
                Span::raw(format!("  {cmd:<22}")),
                Span::styled(*what, Style::default().fg(Color::DarkGray)),
            ])
        }));
        f.render_widget(Paragraph::new(rows.collect::<Vec<_>>()), body);
        f.render_widget(Paragraph::new("").block(Block::default().borders(Borders::ALL)), composer);
    }

    /// The status line is shared by every chat, so notes are transient: info
    /// fades after 4 s, errors after 10 s. An expired delete prompt disarms.
    fn expire_status(&mut self) {
        let now = std::time::Instant::now();
        if self.status != self.status_since.0 {
            self.status_since = (self.status.clone(), now);
            return;
        }
        let ttl = if self.status.starts_with("error") { 10 } else { 4 };
        let sticky = self.status == IDLE || self.status == "loading…";
        if !sticky && now.duration_since(self.status_since.1).as_secs() >= ttl {
            self.status = IDLE.into();
            self.pending_delete = None;
        }
    }

    fn run(mut self, mut terminal: DefaultTerminal) -> io::Result<()> {
        loop {
            self.drain_events();
            self.expire_status();
            terminal.draw(|f| self.draw(f))?;
            if event::poll(Duration::from_millis(250))? {
                let ev = event::read()?;
                match ev {
                    TermEvent::FocusGained => {
                        self.focused = true;
                        self.mark_read();
                    }
                    TermEvent::FocusLost => self.focused = false,
                    _ => {}
                }
                if let TermEvent::Key(key) = ev {
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
                                self.set_timer(demo::next_disappear_preset(self.settings.disappear_secs))
                            }
                            KeyCode::Char('4') => {
                                self.settings.read_receipts = !self.settings.read_receipts;
                                self.settings.save();
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
                                self.status = IDLE.into(); // notes belong to the chat you left
                                self.pending_delete = None;
                                self.mark_read();
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
                        (KeyCode::Char('y'), KeyModifiers::CONTROL) => self.copy_invite(),
                        (KeyCode::Char('o'), KeyModifiers::CONTROL) => self.open_attachment(false),
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
    // Focus reports tell us when the chat is really on screen (read receipts).
    let _ = crossterm::execute!(io::stdout(), crossterm::event::EnableFocusChange);
    // Ask the terminal which image protocol it speaks; half-blocks otherwise.
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((8, 16)));
    let result = App::new(picker).run(terminal);
    let _ = crossterm::execute!(io::stdout(), crossterm::event::DisableFocusChange);
    ratatui::restore();
    result
}
