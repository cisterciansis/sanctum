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
use sanctum_core::contact::{self, spawn_worker, Cmd, Event};
use sanctum_core::demo::{self, Settings};
use sanctum_core::pay::{self, PayKind, PayLink};
use sanctum_core::store::{Contact, Mark, StoredMsg};
use sanctum_core::tao::{self as taolib, Net};
use sanctum_core::wallet::{PayCard, PayKind as TaoKind, PayStatus, TaoCmd, WalletView};

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
    /// TAO payment card, if this message is one.
    tao: Option<PayCard>,
    text: String,
    /// Payment chips found in the text (which shows them shortened).
    pay: Vec<PayLink>,
    time: String,
    day: String,
}

impl Msg {
    fn from_stored(m: StoredMsg) -> Self {
        let ts = Local.timestamp_millis_opt(m.ts_ms).single().unwrap_or_else(Local::now);
        let scanned = if m.file_path.is_none() { pay::scan(&m.text) } else { Default::default() };
        Self {
            wire_id: m.wire_id,
            mine: m.mine,
            delivered: m.delivered,
            failed: false,
            read: m.mine && m.read_ms.is_some(),
            reaction: m.reaction,
            file: m.file_path.clone(),
            tao: m.pay.as_deref().and_then(PayCard::parse),
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
                    scanned.text
                };
                match m.sender_seat {
                    Some(seat) if !m.mine => format!("[m{}] {base}", seat + 1),
                    _ => base,
                }
            },
            pay: scanned.links,
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
    verified: bool,
    blocked: bool,
    favorite: bool,
    key_changed: bool,
    safety: String,
}

impl Chat {
    fn new(c: Contact, msgs: Vec<StoredMsg>) -> Self {
        let mut chat = Self {
            id: c.id,
            name: c.name.clone(),
            waiting: false,
            invite_link: String::new(),
            msgs: msgs.into_iter().map(Msg::from_stored).collect(),
            unread: 0,
            verified: false,
            blocked: false,
            favorite: false,
            key_changed: false,
            safety: String::new(),
        };
        chat.sync(c);
        chat
    }

    /// Take the worker's view of everything but the name (renames are local).
    fn sync(&mut self, c: Contact) {
        self.waiting = c.peer_pk.is_empty();
        self.invite_link = c.invite_link;
        self.verified = c.verified;
        self.blocked = c.blocked;
        self.favorite = c.favorite;
        self.key_changed = c.key_changed;
        self.safety = c.safety;
    }
}

/// TAO commands, shown in the wallet (Ctrl-W).
const TAO_COMMANDS: &[(&str, &str)] = &[
    ("/tao new [name]", "create an account (12-word phrase)"),
    ("/tao import <words>", "restore from a recovery phrase"),
    ("/tao watch <address>", "add a watch-only address"),
    ("/tao use <n> · forget <n>", "pick the paying account · remove one"),
    ("/tao phrase", "show the active account's recovery phrase"),
    ("/tao net main|test", "switch network"),
    ("in a chat: /tao share", "send them your address"),
    ("/tao send <amt> [address]", "pay them (asks /yes first)"),
    ("/tao request <amt> [memo]", "ask them to pay you"),
    ("/tao pay", "pay their newest open request"),
    ("/tao cheque <amt>", "signed offline; either side cashes it later"),
    ("/tao cash", "cash their newest cheque"),
];

/// Mailbox account commands, shown in the wallet (Ctrl-W).
const MAILBOX_COMMANDS: &[(&str, &str)] = &[
    ("/mailbox new", "set up the account (Ctrl-N does it for you)"),
    ("/mailbox fund", "add $20 of credits from your TAO wallet (asks /yes)"),
    ("/mailbox restore <words>", "use your account on this device"),
    ("/mailbox phrase", "show its recovery phrase"),
];

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
    ("Ctrl-W or /tao", "TAO wallet: accounts, balances"),
    ("/mailbox", "mailbox account: credits for the chats you start"),
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
    ("/pay <cashu token>", "send ecash (bearer: whoever redeems first gets it)"),
    ("/paycopy", "copy the newest token, invoice or address"),
    ("/payopen", "open the newest payment in your wallet app"),
    ("/safety", "show the safety number to compare in person"),
    ("/verify, /unverify", "mark this chat's safety number checked"),
    ("/block, /unblock", "drop their messages unseen (kept on this device)"),
    ("/fav", "pin this chat to the top"),
    ("/wipe keep", "erase everything but your TAO and mailbox accounts (asks to confirm)"),
    ("/wipe", "erase everything, wallets included (asks to confirm)"),
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

/// Hand a payment URI (cashu:, lightning:, bitcoin:, rebuilt by the core
/// from validated parts) to whatever wallet app claims the scheme.
fn open_uri(uri: &str) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let tool = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let ok = Command::new(tool)
        .arg(uri)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?
        .success();
    if ok { Ok(()) } else { Err("no wallet app handles this link; /paycopy instead".into()) }
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
    /// Wallet overlay (Ctrl-W), its last state, and a phrase being shown.
    show_wallet: bool,
    wallet: Option<WalletView>,
    phrase: Option<(String, String)>,
    /// A money action waiting for /yes.
    pending_tao: Option<TaoCmd>,
    react_mode: bool,
    inviting: bool,
    /// Terminal has focus, so the selected chat is actually being looked at
    /// (for read receipts). Terminals without focus reporting stay true.
    focused: bool,
    /// Chat armed by a first `/delete`; a second one confirms.
    pending_delete: Option<i64>,
    /// Armed by a first `/wipe`.
    /// Wipe armed by a first `/wipe`: Some(keep_wallets).
    pending_wipe: Option<bool>,
    /// The selected chat's safety number is on screen (`/safety`).
    show_safety: bool,
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
        // Wallet and mailbox state, so Ctrl-N knows whether we can invite.
        let _ = out_tx.send(Cmd::Tao(TaoCmd::Refresh));
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
            show_wallet: false,
            wallet: None,
            phrase: None,
            pending_tao: None,
            react_mode: false,
            inviting: false,
            pending_delete: None,
            pending_wipe: None,
            show_safety: false,
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
                    self.chats = all.into_iter().map(|(c, msgs)| Chat::new(c, msgs)).collect();
                    self.sort_chats();
                    self.status = IDLE.into();
                    self.mark_read();
                }
                Event::ContactAdded(c) => {
                    let ours = std::mem::take(&mut self.inviting) && !c.invite_link.is_empty();
                    self.chats.push(Chat::new(c, Vec::new()));
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
                        chat.sync(c);
                    }
                    self.sort_chats();
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
                Event::TaoWallet(v) => self.wallet = Some(v),
                Event::TaoPhrase { address, phrase } => {
                    self.phrase = Some((address, phrase));
                    self.show_wallet = true;
                }
                Event::MsgUpdated { contact_id, wire_id, text, pay } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        if let Some(m) = c.msgs.iter_mut().find(|m| m.wire_id == wire_id) {
                            m.text = text;
                            m.tao = pay.as_deref().and_then(PayCard::parse);
                        }
                    }
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
        // Erasing takes two of the same, like /delete.
        let keep = match text.as_str() {
            "/wipe" => Some(false),
            "/wipe keep" => Some(true),
            _ => None,
        };
        if let Some(keep) = keep {
            if self.pending_wipe.take() == Some(keep) {
                self.wipe(keep);
            } else {
                self.pending_wipe = Some(keep);
                self.status = if keep {
                    "ERASE chats, contacts, keys, files and settings, keeping your TAO and mailbox accounts? /wipe keep again to confirm"
                } else {
                    "ERASE EVERYTHING, wallets included (funds are gone without their phrases)? /wipe again to confirm"
                }
                .into();
            }
            self.input.clear();
            return;
        }
        if text == "/yes" {
            match self.pending_tao.take() {
                Some(cmd) => {
                    self.status = match cmd {
                        TaoCmd::MailboxFund => "sending; Ctrl-W shows when the credits arrive".into(),
                        TaoCmd::MailboxOpen { .. } => "signing in…".into(),
                        _ => "on its way; the card in the chat tracks it".into(),
                    };
                    let _ = self.outbound.send(Cmd::Tao(cmd));
                }
                None => self.status = "nothing to confirm".into(),
            }
            self.input.clear();
            return;
        }
        if let Some(rest) = text.strip_prefix("/mailbox") {
            self.mailbox_cmd(rest.trim());
            self.input.clear();
            return;
        }
        if let Some(rest) = text.strip_prefix("/tao") {
            if self.tao_wallet_cmd(rest.trim()) {
                self.input.clear();
                return;
            }
        }
        let Some(sel) = self.chats.get(self.selected).map(|c| c.id) else {
            self.status = "no chat selected — Ctrl-N to invite someone".into();
            return;
        };
        if let Some(rest) = text.strip_prefix("/tao") {
            self.tao_chat_cmd(sel, rest.trim());
            self.input.clear();
            return;
        }
        if let Some(rest) = text.strip_prefix("/group ") {
            let mut it = rest.splitn(2, ' ');
            let size: i64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(3);
            let name = it.next().unwrap_or("Group").trim().to_string();
            if !self.inviting && self.mailbox_ready() {
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
        if text == "/paycopy" || text == "/payopen" {
            let link = self.current().and_then(|c| c.msgs.iter().rev().find_map(|m| m.pay.first().cloned()));
            self.status = match link {
                None => "no payment in this chat".into(),
                Some(l) if text == "/paycopy" && copy_to_clipboard(&l.raw) => "copied".into(),
                Some(_) if text == "/paycopy" => "couldn't reach the clipboard".into(),
                Some(l) => match open_uri(&l.uri) {
                    Ok(()) => "handed to your wallet app".into(),
                    Err(e) => format!("error: {e}"),
                },
            };
            self.input.clear();
            return;
        }
        if text == "/safety" {
            self.show_safety = !self.show_safety;
            self.input.clear();
            return;
        }
        let mark = match text.as_str() {
            "/verify" => Some((Mark::Verified, true)),
            "/unverify" => Some((Mark::Verified, false)),
            "/block" => Some((Mark::Blocked, true)),
            "/unblock" => Some((Mark::Blocked, false)),
            "/fav" => Some((Mark::Favorite, !self.chats[self.selected].favorite)),
            _ => None,
        };
        if let Some((mark, on)) = mark {
            if sel < 0 {
                self.status = "direct chats only, for now".into();
            } else if matches!(mark, Mark::Verified) && on && self.chats[self.selected].safety.is_empty() {
                self.status = "no safety number until they join".into();
            } else {
                let _ = self.outbound.send(Cmd::Mark { contact_id: sel, mark, on });
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
        let mut text = text;
        if let Some(arg) = text.strip_prefix("/pay ") {
            match pay::pay_token(arg) {
                Ok((token, info)) => {
                    let amount = info.display_amount().unwrap_or_default();
                    self.status = format!("sending {amount}. {}", pay::BEARER_WARNING);
                    text = token;
                }
                Err(e) => {
                    self.status = format!("error: {e}");
                    self.input.clear();
                    return;
                }
            }
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

    fn tao(&self, cmd: TaoCmd) {
        let _ = self.outbound.send(Cmd::Tao(cmd));
    }

    fn net_label(&self) -> &'static str {
        match Net::from_id(&self.settings.tao_net) {
            Some(Net::Test) => "testnet",
            _ => "MAINNET",
        }
    }

    /// Ask before anything moves money; `/yes` sends `cmd`.
    fn confirm(&mut self, prompt: String, cmd: TaoCmd) {
        self.status = format!("confirm: {prompt} on {} · /yes to send", self.net_label());
        self.pending_tao = Some(cmd);
    }

    /// Starting a chat needs a funded mailbox account. Sets one up the first
    /// time (joining never needs it) and says what's missing.
    fn mailbox_ready(&mut self) -> bool {
        // Unknown yet: let the worker decide.
        let Some(w) = &self.wallet else { return true };
        let blocked = match &w.mailbox {
            None => {
                self.tao(TaoCmd::MailboxOpen { phrase: None });
                "setting up your mailbox account: it pays for the chats you start (joining is free)"
            }
            Some(m) if m.busy && m.credits.is_none() => "your mailbox account is still being set up…",
            Some(m) if m.credits.is_some_and(|c| c <= 0.0) => {
                "add $20 of credits first: /mailbox fund, or send TAO to the deposit address below"
            }
            Some(_) => return true,
        };
        self.status = blocked.into();
        self.show_wallet = true;
        false
    }

    fn mailbox_cmd(&mut self, args: &str) {
        let (verb, rest) = args.split_once(' ').unwrap_or((args, ""));
        let mailbox = self.wallet.as_ref().and_then(|w| w.mailbox.clone());
        self.show_wallet = true;
        match verb {
            "" | "refresh" => self.tao(TaoCmd::Refresh),
            "new" if mailbox.is_some() => self.status = "you already have a mailbox account".into(),
            "new" => self.tao(TaoCmd::MailboxOpen { phrase: None }),
            "restore" if rest.split_whitespace().count() < 12 => {
                self.status = "usage: /mailbox restore <12 words>".into()
            }
            "restore" => {
                let cmd = TaoCmd::MailboxOpen { phrase: Some(rest.trim().into()) };
                if mailbox.is_some() {
                    self.status = "confirm: switch this device to that account? Back up the current phrase first (/mailbox phrase) · /yes".into();
                    self.pending_tao = Some(cmd);
                } else {
                    self.tao(cmd);
                }
            }
            "phrase" if mailbox.as_ref().is_some_and(|m| m.address.is_some()) => self.tao(TaoCmd::MailboxReveal),
            "phrase" => self.status = "no mailbox phrase on this device".into(),
            "fund" => match mailbox.as_ref().and_then(|m| m.fund_rao.map(|r| (r, m.deposit.clone()))) {
                Some((rao, Some(_))) => {
                    let payer = self.wallet.as_ref().and_then(|w| {
                        w.accounts.iter().find(|a| w.active.as_deref() == Some(a.address.as_str()) && !a.watch_only)
                    });
                    let Some(from) = payer.map(|a| a.address.clone()) else {
                        self.status = "no TAO account to pay from: /tao new or /tao import, or send TAO to the deposit address from any wallet".into();
                        return;
                    };
                    self.status = format!(
                        "confirm: send {} (~$20) from {from} to your Hippius deposit address on MAINNET? Final · /yes",
                        taolib::format_tao(rao)
                    );
                    self.pending_tao = Some(TaoCmd::MailboxFund);
                }
                _ if mailbox.is_none() => self.status = "no mailbox account yet: /mailbox new".into(),
                _ => {
                    self.status = "fetching the deposit address and TAO price; try again in a moment".into();
                    self.tao(TaoCmd::Refresh);
                }
            },
            _ => self.status = "usage: /mailbox [new | fund | restore <words> | phrase]".into(),
        }
    }

    /// Wallet commands that need no chat. Returns false for chat commands.
    fn tao_wallet_cmd(&mut self, args: &str) -> bool {
        let (verb, rest) = args.split_once(' ').unwrap_or((args, ""));
        let rest = rest.trim();
        let pick = |n: &str, w: &Option<WalletView>| {
            let i: usize = n.parse().ok()?;
            w.as_ref()?.accounts.get(i.checked_sub(1)?).map(|a| a.address.clone())
        };
        match verb {
            "" | "wallet" | "refresh" => {
                self.show_wallet = true;
                self.tao(TaoCmd::Refresh);
            }
            "new" => {
                let name = if rest.is_empty() { "Main" } else { rest };
                self.tao(TaoCmd::New { name: name.into() });
                self.tao(TaoCmd::Refresh);
                self.show_wallet = true;
            }
            "import" => self.tao(TaoCmd::Import { name: "Imported".into(), phrase: rest.into() }),
            "watch" => {
                let (addr, name) = rest.split_once(' ').unwrap_or((rest, "Watch-only"));
                self.tao(TaoCmd::Watch { name: name.trim().into(), address: addr.into() });
            }
            "use" => match pick(rest, &self.wallet) {
                Some(address) => self.tao(TaoCmd::Use { address }),
                None => self.status = "usage: /tao use <n> (numbers in Ctrl-W)".into(),
            },
            "forget" => match pick(rest, &self.wallet) {
                Some(address) => self.confirm(
                    format!("forget account {rest} (its keys are deleted unless you saved the phrase)"),
                    TaoCmd::Forget { address },
                ),
                None => self.status = "usage: /tao forget <n>".into(),
            },
            "phrase" => match self.wallet.as_ref().and_then(|w| w.active.clone()) {
                Some(address) => self.tao(TaoCmd::Reveal { address }),
                None => self.status = "no wallet yet: /tao new".into(),
            },
            "net" => match Net::from_id(rest) {
                Some(net) => {
                    self.settings.tao_net = net.id().into();
                    self.tao(TaoCmd::SetNet { net });
                }
                None => self.status = "usage: /tao net main|test".into(),
            },
            _ => return false,
        }
        true
    }

    fn tao_chat_cmd(&mut self, sel: i64, args: &str) {
        let (verb, rest) = args.split_once(' ').unwrap_or((args, ""));
        let (amount, tail) = rest.trim().split_once(' ').unwrap_or((rest.trim(), ""));
        let wire_id = ulid::Ulid::new().to_string();
        let theirs = |kind: TaoKind, status: PayStatus, c: &Chat| {
            c.msgs
                .iter()
                .rev()
                .find(|m| !m.mine && m.tao.as_ref().is_some_and(|p| p.kind == kind && p.status == status))
                .map(|m| (m.wire_id.clone(), m.tao.clone().expect("checked")))
        };
        match verb {
            "share" => self.tao(TaoCmd::Share { contact_id: sel }),
            "send" | "cheque" => {
                let Some(rao) = taolib::parse_tao(amount) else {
                    self.status = format!("usage: /tao {verb} <amount> e.g. 1.5");
                    return;
                };
                let to = Some(tail.trim()).filter(|t| !t.is_empty()).map(String::from);
                let whom = to.clone().unwrap_or_else(|| "their shared address".into());
                let what = taolib::format_tao(rao);
                if verb == "send" {
                    let cmd = TaoCmd::Pay { contact_id: sel, wire_id, rao, to, re: None };
                    self.confirm(format!("send {what} to {whom}"), cmd);
                } else {
                    let cmd = TaoCmd::Cheque { contact_id: sel, wire_id, rao };
                    self.confirm(format!("sign a {what} cheque to them"), cmd);
                }
            }
            "request" => match taolib::parse_tao(amount) {
                Some(rao) => {
                    let memo = Some(tail.trim()).filter(|t| !t.is_empty()).map(String::from);
                    self.tao(TaoCmd::Request { contact_id: sel, wire_id, rao, memo });
                }
                None => self.status = "usage: /tao request <amount> [memo]".into(),
            },
            "pay" => match self.current().and_then(|c| theirs(TaoKind::Request, PayStatus::Open, c)) {
                Some((re, card)) => {
                    let rao = card.amount();
                    let what = format!("pay their request of {} to {}", taolib::format_tao(rao), card.to);
                    let cmd = TaoCmd::Pay { contact_id: sel, wire_id, rao, to: Some(card.to), re: Some(re) };
                    self.confirm(what, cmd);
                }
                None => self.status = "no open request from them".into(),
            },
            "cash" => match self.current().and_then(|c| theirs(TaoKind::Cheque, PayStatus::Pending, c)) {
                Some((wire_id, _)) => self.tao(TaoCmd::Cash { contact_id: sel, wire_id }),
                None => self.status = "no uncashed cheque from them".into(),
            },
            _ => self.status = "unknown /tao command; Ctrl-S lists them".into(),
        }
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

    /// Favorites first, otherwise as they were; the selection follows its chat.
    fn sort_chats(&mut self) {
        let sel = self.chats.get(self.selected).map(|c| c.id);
        self.chats.sort_by_key(|c| !c.favorite);
        if let Some(i) = sel.and_then(|id| self.chats.iter().position(|c| c.id == id)) {
            self.selected = i;
        }
    }

    /// Emergency wipe, then start over as on first launch with a new worker.
    fn wipe(&mut self, keep_wallets: bool) {
        let result = match contact::wipe(keep_wallets) {
            Ok(problem) => problem,
            Err(e) => return self.status = format!("error: {e}"), // nothing was erased
        };
        let (out_tx, out_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        spawn_worker(out_rx, ev_tx);
        self.outbound = out_tx;
        self.events = ev_rx;
        self.chats.clear();
        self.selected = 0;
        self.settings = Settings::load();
        self.previews.borrow_mut().clear();
        self.show_safety = false;
        self.inviting = false;
        self.wallet = None;
        let _ = self.outbound.send(Cmd::Tao(TaoCmd::Refresh));
        self.status = match result {
            None if keep_wallets => "wiped: only your TAO and mailbox accounts are left".into(),
            None => "wiped: nothing of yours is left on this device".into(),
            Some(e) => format!("error: {e}"),
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
        let scanned = pay::scan(&text);
        if let Some(c) = self.current() {
            c.msgs.push(Msg {
                wire_id: wire_id.into(),
                mine: true,
                delivered: false,
                failed: false,
                read: false,
                reaction: None,
                file: None,
                tao: None,
                pay: scanned.links,
                text: scanned.text,
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
            let star = if c.favorite { "★ " } else { "" };
            let flags = match (c.key_changed, c.blocked) {
                (true, _) => " ⚠",
                (_, true) => " ⊘",
                _ => "",
            };
            side.push(Line::from(Span::styled(format!("{marker}{star}{}{suffix}{flags}", c.name), style)));
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
            .map(|c| match (c.verified, c.blocked) {
                (_, true) => format!("{} (blocked)", c.name),
                (true, _) => format!("{} ✓ verified", c.name),
                _ => c.name.clone(),
            })
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
        if self.show_wallet {
            self.draw_wallet(f, body);
            f.render_widget(
                Paragraph::new(self.input.as_str()).block(Block::default().borders(Borders::ALL)),
                composer,
            );
            return;
        }

        let width = body.width.saturating_sub(2) as usize;
        let mut lines: Vec<Row> = Vec::new();
        if let Some(c) = self.chats.get(self.selected) {
            if c.key_changed {
                let warn = Style::default().fg(Color::Red).bold();
                lines.push(Row::Text(Line::from(Span::styled(
                    " ⚠ Someone wrote to this chat under a different identity key. It was refused.",
                    warn,
                ))));
                lines.push(Row::Text(Line::from(Span::styled(
                    "   Compare safety numbers in person (/safety), then /verify.",
                    warn,
                ))));
                lines.push(Row::Text(Line::raw("")));
            }
            if self.show_safety {
                let note = Style::default().fg(Color::Cyan);
                let head = if c.safety.is_empty() {
                    " No safety number yet: it appears once they join (groups: not yet).".to_string()
                } else {
                    format!(" Safety number with {}. Same digits on both screens? /verify", c.name)
                };
                lines.push(Row::Text(Line::from(Span::styled(head, note))));
                let groups: Vec<&str> = c.safety.split(' ').collect();
                for row in groups.chunks(4).filter(|r| !r[0].is_empty()) {
                    lines.push(Row::Text(Line::from(Span::styled(format!("   {}", row.join("  ")), note.bold()))));
                }
                lines.push(Row::Text(Line::raw("")));
            }
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
                for l in &m.pay {
                    let icon = match l.kind {
                        PayKind::Cashu => "🥜",
                        PayKind::Lightning => "⚡",
                        PayKind::Bitcoin => "₿",
                        PayKind::Zcash => "ⓩ",
                    };
                    let memo = l.memo.as_ref().map(|m| format!(" · {m}")).unwrap_or_default();
                    let chip = format!(" {icon} {}{memo} ", l.label);
                    let hint = match l.kind {
                        PayKind::Cashu => " bearer: whoever redeems first gets it · /paycopy /payopen",
                        _ => " /payopen in your wallet · /paycopy",
                    };
                    for (text, style) in [
                        (chip, Style::default().fg(Color::Black).bg(Color::Yellow)),
                        (hint.to_string(), Style::default().fg(Color::DarkGray)),
                    ] {
                        let pad = if m.mine { width.saturating_sub(text.chars().count()) } else { 0 };
                        lines.push(Row::Text(Line::from(vec![
                            Span::raw(" ".repeat(pad)),
                            Span::styled(text, style),
                        ])));
                    }
                }
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

    /// Mailbox account: what pays for the chats you start.
    fn mailbox_rows(&self, dim: Style) -> Vec<Line<'static>> {
        let label = |k: &str| Span::styled(format!("  {k:<9}"), dim);
        let mut rows = vec![Line::from(vec![
            Span::raw("  ✉ Mailbox account").bold(),
            Span::styled("  ·  pays for the chats you start; joining is free", dim),
        ])];
        let Some(m) = self.wallet.as_ref().and_then(|w| w.mailbox.as_ref()) else {
            rows.push(Line::raw("  None yet. Ctrl-N sets one up; /mailbox restore <words> brings yours here."));
            rows.push(Line::raw(""));
            return rows;
        };
        let account = m.address.clone().unwrap_or_else(|| "connected with an API token".into());
        rows.push(Line::from(vec![label("Account"), Span::raw(account)]));
        let credits = match m.credits {
            None if m.busy => Span::styled("…", dim),
            None => Span::styled("unknown (r to refresh)", dim),
            Some(c) if c > 0.0 => Span::styled(format!("${c:.2} · ready to start chats"), Style::default().fg(Color::Green)),
            Some(c) => Span::styled(format!("${c:.2} · add $20 to start chats"), Style::default().fg(Color::Yellow)),
        };
        rows.push(Line::from(vec![label("Credits"), credits]));
        if let Some(d) = &m.deposit {
            rows.push(Line::from(vec![label("Deposit"), Span::raw(d.clone())]));
            let cost = m.fund_rao.map(|r| format!(" ($20 ≈ {})", taolib::format_tao(r))).unwrap_or_default();
            rows.push(Line::from(vec![
                label(""),
                Span::styled(format!("send at least $20 of TAO here{cost}, never to the account address"), dim),
            ]));
        }
        if let Some(n) = &m.note {
            rows.push(Line::from(vec![label(""), Span::styled(n.clone(), Style::default().fg(Color::Cyan))]));
        }
        rows.push(Line::raw(""));
        rows
    }

    fn draw_wallet(&self, f: &mut Frame, body: Rect) {
        let dim = Style::default().fg(Color::DarkGray);
        let mut rows = vec![Line::raw("")];
        rows.extend(self.mailbox_rows(dim));
        rows.push(Line::from(vec![
            Span::raw("  τ TAO wallet").bold(),
            Span::styled(format!("  ·  {}  ·  r refresh · esc close", self.net_label()), dim),
        ]));
        rows.push(Line::raw(""));
        match &self.wallet {
            Some(w) if !w.accounts.is_empty() => {
                for (i, a) in w.accounts.iter().enumerate() {
                    let active = w.active.as_deref() == Some(a.address.as_str());
                    let bal = a.balance.map(taolib::format_tao).unwrap_or_else(|| "…".into());
                    let kind = if a.watch_only { " (watch-only)" } else { "" };
                    rows.push(Line::raw(format!(
                        "  {} {}{} {:<10} {}  {}",
                        i + 1,
                        if active { "▸" } else { " " },
                        kind,
                        a.name,
                        a.address,
                        bal
                    )));
                }
            }
            _ => rows.push(Line::raw("  No accounts yet. /tao new creates one; /tao import <words> restores.")),
        }
        if let Some((addr, phrase)) = &self.phrase {
            let mailbox = self.wallet.as_ref().and_then(|w| w.mailbox.as_ref()).and_then(|m| m.address.as_deref());
            let what = if mailbox == Some(addr.as_str()) { "your mailbox account and its credits" } else { "the funds" };
            rows.push(Line::raw(""));
            rows.push(Line::from(Span::styled(
                format!("  Recovery phrase for {addr}: anyone with it controls {what}"),
                Style::default().fg(Color::Yellow),
            )));
            rows.push(Line::raw(format!("  {phrase}")));
        }
        rows.push(Line::raw(""));
        for (cmd, what) in MAILBOX_COMMANDS.iter().chain(TAO_COMMANDS) {
            rows.push(Line::from(vec![Span::raw(format!("  {cmd:<26}")), Span::styled(*what, dim)]));
        }
        f.render_widget(Paragraph::new(rows).wrap(Wrap { trim: false }), body);
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
        let ttl = match () {
            _ if self.status.starts_with("confirm") => 30, // money: time to read it
            _ if self.status.starts_with("error") => 10,
            _ => 4,
        };
        let sticky = self.status == IDLE || self.status == "loading…";
        if !sticky && now.duration_since(self.status_since.1).as_secs() >= ttl {
            self.status = IDLE.into();
            self.pending_delete = None;
            self.pending_tao = None;
            self.pending_wipe = None;
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
                    if self.show_wallet && key.code != KeyCode::Enter && key.modifiers.is_empty() {
                        match key.code {
                            KeyCode::Esc => {
                                self.show_wallet = false;
                                self.phrase = None; // never leave a phrase on screen
                            }
                            KeyCode::Char('r') if self.input.is_empty() => self.tao(TaoCmd::Refresh),
                            KeyCode::Char(c) => self.input.push(c),
                            KeyCode::Backspace => {
                                self.input.pop();
                            }
                            _ => {}
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
                                self.show_safety = false;
                                self.mark_read();
                                if let Some(c) = self.current() {
                                    c.unread = 0;
                                }
                            }
                        }
                        (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                            if !self.inviting && self.mailbox_ready() {
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
                        (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                            self.show_wallet = true;
                            self.tao(TaoCmd::Refresh);
                        }
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
    // Pick the image protocol from the environment rather than querying the
    // terminal: when a terminal doesn't answer, ratatui-image's query leaves a
    // thread blocked on stdin that steals keystrokes. iTerm2/WezTerm/… are
    // detected by from_fontsize itself; kitty-protocol terminals here.
    let mut picker = Picker::from_fontsize((10, 20));
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    if env("TERM").contains("kitty") || env("TERM_PROGRAM") == "ghostty" || !env("KITTY_WINDOW_ID").is_empty() {
        picker.set_protocol_type(ratatui_image::picker::ProtocolType::Kitty);
    }
    let result = App::new(picker).run(terminal);
    let _ = crossterm::execute!(io::stdout(), crossterm::event::DisableFocusChange);
    ratatui::restore();
    result
}
