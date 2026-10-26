//! Sanctum desktop — iMessage-style multi-chat client over the encrypted
//! store. New Invite mints a contact's queue pair (needs hippius_api_token);
//! pasting a sanctum://v2/ link into the composer joins one.

use std::time::Duration;

use chrono::{Local, TimeZone};
use gpui_kit::assets::Assets;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    switch::Switch,
    Theme, ThemeMode, *,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use sanctum_core::contact::{self, spawn_worker, Cmd, Event};
use sanctum_core::demo::{self, Settings};
use sanctum_core::pay::{self, PayKind, PayLink};
use sanctum_core::store::{Contact, Mark, StoredMsg};
use sanctum_core::tao::{self, Net};
use sanctum_core::wallet::{PayCard, PayKind as TaoKind, PayStatus, TaoCmd, WalletView};

const REACTIONS: [&str; 6] = ["👍", "👎", "❤️", "‼️", "❓", "😂"];
const ACCENTS: [(&str, u32); 6] = [
    ("Blue", 0x0A84FF),
    ("Purple", 0xBF5AF2),
    ("Green", 0x30D158),
    ("Orange", 0xFF9F0A),
    ("Pink", 0xFF375F),
    ("Graphite", 0x6C6C70),
];

/// Surface palette; kit components follow Theme::change separately.
struct Pal {
    bg: u32,
    surface: u32,
    border: u32,
    text: u32,
    sub: u32,
    bubble: u32,
    bubble_text: u32,
    hover: u32,
    selected: u32,
    well: u32,
    /// Floating surfaces: menus, badges.
    raised: u32,
}

fn pal(dark: bool) -> Pal {
    if dark {
        Pal {
            bg: 0x1C1C1E,
            surface: 0x242426,
            border: 0x313133,
            text: 0xF2F2F7,
            sub: 0x98989D,
            bubble: 0x3A3A3C,
            bubble_text: 0xF2F2F7,
            hover: 0x2E2E30,
            selected: 0x3A3A3C,
            well: 0x2C2C2E,
            raised: 0x2C2C2E,
        }
    } else {
        Pal {
            bg: 0xFFFFFF,
            surface: 0xF5F5F7,
            border: 0xE5E5EA,
            text: 0x1D1D1F,
            sub: 0x8E8E93,
            bubble: 0xE9E9EB,
            bubble_text: 0x000000,
            hover: 0xE8E8ED,
            selected: 0xE3E3E8,
            well: 0xF2F2F7,
            raised: 0xFFFFFF,
        }
    }
}

struct Msg {
    wire_id: String,
    mine: bool,
    delivered: bool,
    /// Never left this device (only known for this session's sends).
    failed: bool,
    /// Upload finished (history is always sent; new sends start false).
    sent: bool,
    /// When the peer read this own message (read receipts on).
    read_ms: Option<i64>,
    reaction: Option<SharedString>,
    text: SharedString,
    /// TAO payment card, if this message is one.
    tao: Option<PayCard>,
    /// Payment chips found in the text (which shows them shortened).
    pay: Vec<PayLink>,
    file: Option<(SharedString, String)>,
    sender_seat: Option<i64>,
    ts_ms: i64,
    time: SharedString,
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
            sent: true,
            read_ms: m.read_ms.filter(|_| m.mine),
            reaction: m.reaction.map(Into::into),
            text: if m.file_path.is_none() { scanned.text.into() } else { m.text.clone().into() },
            tao: m.pay.as_deref().and_then(PayCard::parse),
            pay: scanned.links,
            file: m.file_path.map(|p| (m.text.into(), p)),
            sender_seat: m.sender_seat,
            ts_ms: m.ts_ms,
            time: ts.format("%-I:%M %p").to_string().into(),
            day: ts.format("%A, %b %-d").to_string(),
        }
    }

    fn now(wire_id: String, mine: bool, text: String, file: Option<(String, String)>) -> Self {
        let now = Local::now();
        let scanned = if file.is_none() { pay::scan(&text) } else { Default::default() };
        Self {
            wire_id,
            mine,
            delivered: false,
            failed: false,
            sent: !mine,
            read_ms: None,
            reaction: None,
            text: if file.is_none() { scanned.text.into() } else { text.into() },
            tao: None,
            pay: scanned.links,
            file: file.map(|(n, p)| (n.into(), p)),
            sender_seat: None,
            ts_ms: now.timestamp_millis(),
            time: now.format("%-I:%M %p").to_string().into(),
            day: now.format("%A, %b %-d").to_string(),
        }
    }
}

/// A timestamp and how many calendar days ago it was.
fn local(ts_ms: i64) -> (chrono::DateTime<Local>, i64) {
    let ts = Local.timestamp_millis_opt(ts_ms).single().unwrap_or_else(Local::now);
    (ts, (Local::now().date_naive() - ts.date_naive()).num_days())
}

/// Chat list time: clock time today, then Yesterday, a weekday, a date.
fn list_time(ts_ms: i64) -> String {
    let (ts, days) = local(ts_ms);
    match days {
        ..=0 => ts.format("%-I:%M %p").to_string(),
        1 => "Yesterday".into(),
        2..=6 => ts.format("%A").to_string(),
        _ => ts.format("%b %-d").to_string(),
    }
}

/// In-chat divider, as on iOS: "Today 3:41 PM", "Sat, Oct 25 3:41 PM".
fn time_header(ts_ms: i64) -> String {
    let (ts, days) = local(ts_ms);
    let time = ts.format("%-I:%M %p");
    match days {
        ..=0 => format!("Today {time}"),
        1 => format!("Yesterday {time}"),
        _ => format!("{} {time}", ts.format("%a, %b %-d")),
    }
}

/// A popup menu; only one is open at a time.
#[derive(Clone, Copy, PartialEq)]
enum Menu {
    /// Sidebar +: invite, group, join.
    New,
    /// Composer +: file, money.
    Attach,
    /// A chat list row.
    Chat(usize),
    /// A message: tapbacks, copy, unsend, delete.
    Msg(usize),
}

/// Floating menu surface.
fn menu_card(p: &Pal) -> Div {
    v_flex()
        .min_w(px(190.))
        .p_1()
        .rounded(px(10.))
        .bg(rgb(p.raised))
        .border_1()
        .border_color(rgb(p.border))
        .shadow_lg()
}

/// Menu row; highlights in the accent color like a native menu.
fn menu_item(id: impl Into<ElementId>, label: impl Into<SharedString>, color: u32, accent: u32) -> Stateful<Div> {
    div()
        .id(id)
        .cursor_pointer()
        .w_full()
        .px_2()
        .py(px(5.))
        .rounded(px(6.))
        .text_sm()
        .text_color(rgb(color))
        .hover(move |d| d.bg(rgb(accent)).text_color(rgb(0xFFFFFF)))
        .child(label.into())
}

fn menu_sep(p: &Pal) -> Div {
    div().mx_2().my_1().h(px(1.)).bg(rgb(p.border))
}

/// Kit controls (switches, primary buttons, focus rings) in the accent color.
fn tint(accent: u32, cx: &mut App) {
    let c: Hsla = rgb(accent).into();
    Theme::update(cx, |t| {
        t.primary = c;
        t.primary_hover = c.opacity(0.88);
        t.primary_active = c.opacity(0.76);
        t.primary_foreground = rgb(0xFFFFFF).into();
        t.button_primary = c;
        t.button_primary_hover = c.opacity(0.88);
        t.button_primary_active = c.opacity(0.76);
        t.button_primary_foreground = rgb(0xFFFFFF).into();
        t.ring = c;
    });
}

/// A titled group of rows on a rounded card, like an iOS form section.
fn section(p: &Pal, title: &str, rows: Vec<AnyElement>) -> Div {
    let mut card = v_flex().w_full().px_4().rounded(px(12.)).bg(rgb(p.raised));
    for (n, r) in rows.into_iter().enumerate() {
        if n > 0 {
            card = card.child(div().h(px(1.)).bg(rgb(p.border)));
        }
        card = card.child(div().w_full().py_3().child(r));
    }
    v_flex()
        .w_full()
        .gap_1p5()
        .child(div().px_4().text_xs().font_medium().text_color(rgb(p.sub)).child(title.to_uppercase()))
        .child(card)
}

/// Scrolling column of sections for Settings and Details.
fn pane(sections: Vec<Div>) -> Div {
    v_flex().w_full().max_w(px(600.)).gap_6().px_5().py_6().children(sections)
}

/// Centered placeholder for an empty list or chat: a padlock, a title, a line.
fn empty_state(p: &Pal, title: &'static str, body: &'static str) -> Div {
    v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .gap_1()
        .child(
            v_flex()
                .items_center()
                .mb_3()
                .opacity(0.6)
                .child(div().size(px(24.)).rounded_full().border_3().border_color(rgb(p.sub)))
                .child(div().mt(px(-10.)).w(px(36.)).h(px(28.)).rounded(px(7.)).bg(rgb(p.sub))),
        )
        .child(div().text_lg().font_semibold().text_color(rgb(p.text)).child(title))
        .child(div().text_sm().text_color(rgb(p.sub)).child(body))
}

fn is_image(name: &str) -> bool {
    let lower = name.to_lowercase();
    ["png", "jpg", "jpeg", "gif", "webp", "bmp"].iter().any(|e| lower.ends_with(&format!(".{e}")))
}

struct Chat {
    id: i64,
    name: SharedString,
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
            name: c.name.clone().into(),
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

    /// One line for the chat list.
    fn preview(&self) -> String {
        match self.msgs.last() {
            None if self.waiting => "Invitation pending".into(),
            None => String::new(),
            Some(Msg { file: Some((name, _)), .. }) if is_image(name) => "Photo".into(),
            Some(m) => m.text.replace('\n', " "),
        }
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

struct ChatApp {
    chats: Vec<Chat>,
    selected: usize,
    status: SharedString,
    settings: Settings,
    show_settings: bool,
    show_chat_info: bool,
    show_sidebar: bool,
    show_join: bool,
    show_group: bool,
    group_size: i64,
    inviting: bool,
    joining: bool,
    /// Armed by the first click on Emergency wipe.
    /// Wipe armed by a first click: Some(keep_wallets).
    confirm_wipe: Option<bool>,
    /// The one open popup menu, and where it was opened (window coords).
    menu: Option<Menu>,
    menu_at: Point<Pixels>,
    copied_at: Option<std::time::Instant>,
    /// Composer is empty: the send button rests dimmed.
    draft_empty: bool,
    input_state: Entity<InputState>,
    rename_state: Entity<InputState>,
    join_state: Entity<InputState>,
    group_state: Entity<InputState>,
    timer_state: Entity<InputState>,
    /// TAO: amount for chat payments, phrase/address for adding accounts.
    tao_amount: Entity<InputState>,
    tao_key: Entity<InputState>,
    /// Recovery phrase or API token for an existing mailbox account.
    mailbox_key: Entity<InputState>,
    wallet: Option<WalletView>,
    phrase: Option<(String, String)>,
    /// A money action shown for confirmation before it's sent.
    pending_tao: Option<(String, TaoCmd)>,
    /// Message list scroll, so new messages follow only a reader at the bottom.
    scroll: ScrollHandle,
    window_active: bool,
    outbound: std::sync::mpsc::Sender<Cmd>,
    events: std::sync::mpsc::Receiver<Event>,
    _subscriptions: Vec<Subscription>,
}

impl ChatApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (out_tx, out_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        spawn_worker(out_rx, ev_tx);
        // Wallet and mailbox state, so New Invite knows whether we can invite.
        let _ = out_tx.send(Cmd::Tao(TaoCmd::Refresh));

        let settings = Settings::load();
        let mode = if settings.theme == "dark" { ThemeMode::Dark } else { ThemeMode::Light };
        Theme::change(mode, Some(window), cx);
        tint(settings.accent, cx);

        let input_state = cx.new(|cx| InputState::new(window, cx).placeholder("Sanctum"));
        let rename_state = cx.new(|cx| InputState::new(window, cx));
        let join_state = cx.new(|cx| InputState::new(window, cx).placeholder("sanctum://…"));
        let group_state = cx.new(|cx| InputState::new(window, cx).placeholder("Group name"));
        let timer_state = cx.new(|cx| InputState::new(window, cx).placeholder("90m, 2w, 6mo"));
        let tao_amount = cx.new(|cx| InputState::new(window, cx).placeholder("1.5"));
        let tao_key = cx.new(|cx| InputState::new(window, cx).placeholder("recovery phrase or address"));
        let mailbox_key =
            cx.new(|cx| InputState::new(window, cx).placeholder("recovery phrase or Hippius API token"));
        let mut _subscriptions = vec![cx.subscribe_in(&input_state, window, {
            move |this: &mut Self, _, ev: &InputEvent, window, cx| {
                match ev {
                    InputEvent::PressEnter { .. } => this.send_current(window, cx),
                    // Redraw only when the send button's state flips.
                    InputEvent::Change => {
                        let empty = this.input_state.read(cx).value().trim().is_empty();
                        if empty != this.draft_empty {
                            this.draft_empty = empty;
                            cx.notify();
                        }
                    }
                    _ => {}
                }
            }
        })];

        // Read receipts only count what's on screen: track window focus.
        _subscriptions.push(cx.observe_window_activation(window, |this, window, _| {
            this.window_active = window.is_window_active();
            this.mark_read();
        }));

        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                if this.update(cx, |this, cx| this.drain_events(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        Self {
            chats: Vec::new(),
            selected: 0,
            status: "loading…".into(),
            settings,
            show_settings: false,
            show_chat_info: false,
            show_sidebar: true,
            show_join: false,
            show_group: false,
            group_size: 3,
            inviting: false,
            joining: false,
            confirm_wipe: None,
            menu: None,
            menu_at: Point::default(),
            copied_at: None,
            draft_empty: true,
            input_state,
            rename_state,
            join_state,
            group_state,
            timer_state,
            tao_amount,
            tao_key,
            mailbox_key,
            wallet: None,
            phrase: None,
            pending_tao: None,
            scroll: ScrollHandle::new(),
            window_active: true,
            outbound: out_tx,
            events: ev_rx,
            _subscriptions,
        }
    }

    fn chat_by_id(&mut self, id: i64) -> Option<&mut Chat> {
        self.chats.iter_mut().find(|c| c.id == id)
    }

    fn drain_events(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        while let Ok(ev) = self.events.try_recv() {
            changed = true;
            match ev {
                Event::Snapshot(all) => {
                    self.chats = all.into_iter().map(|(c, msgs)| Chat::new(c, msgs)).collect();
                    self.sort_chats();
                    self.scroll.scroll_to_bottom(); // chats open on their newest message
                    self.status = "ready".into();
                    self.mark_read();
                }
                Event::ContactAdded(c) => {
                    self.inviting = false;
                    if self.joining {
                        self.joining = false;
                        self.show_join = false;
                    }
                    self.chats.push(Chat::new(c, Vec::new()));
                    self.selected = self.chats.len() - 1;
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
                    self.menu = None;
                    self.chats.retain(|c| c.id != contact_id);
                    if self.selected >= self.chats.len() {
                        self.selected = self.chats.len().saturating_sub(1);
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
                    // Within a few px of the end counts as "at the bottom".
                    let at_bottom = -self.scroll.offset().y >= self.scroll.max_offset().y - px(24.);
                    if let Some(c) = self.chat_by_id(contact_id) {
                        c.msgs.push(Msg::from_stored(msg));
                        if !focused {
                            c.unread += 1;
                        }
                    }
                    if focused && at_bottom {
                        self.scroll.scroll_to_bottom();
                    }
                    if focused {
                        self.mark_read();
                    }
                }
                Event::Read { contact_id, wire_ids, ts_ms } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        for m in c.msgs.iter_mut().filter(|m| wire_ids.contains(&m.wire_id)) {
                            m.read_ms = Some(ts_ms);
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
                Event::Sent { contact_id, wire_id } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        if let Some(m) = c.msgs.iter_mut().find(|m| m.wire_id == wire_id) {
                            m.sent = true;
                        }
                    }
                }
                Event::SendFailed { contact_id, wire_id, error } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        if let Some(m) = c.msgs.iter_mut().find(|m| m.wire_id == wire_id) {
                            m.failed = true;
                        }
                    }
                    self.status = format!("error: not sent: {error}").into();
                }
                Event::Reaction { contact_id, ref_id, emoji } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        if let Some(m) = c.msgs.iter_mut().find(|m| m.wire_id == ref_id) {
                            m.reaction = Some(emoji.into());
                        }
                    }
                }
                Event::MsgUpdated { contact_id, wire_id, text, pay } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        if let Some(m) = c.msgs.iter_mut().find(|m| m.wire_id == wire_id) {
                            m.text = text.into();
                            m.tao = pay.as_deref().and_then(PayCard::parse);
                        }
                    }
                }
                Event::TaoWallet(w) => self.wallet = Some(w),
                Event::TaoPhrase { address, phrase } => {
                    self.phrase = Some((address, phrase));
                    self.show_settings = true;
                }
                Event::TimerChanged { secs, .. } => {
                    self.settings.disappear_secs = secs;
                    self.settings.save();
                }
                Event::Unsent { contact_id, wire_id } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        c.msgs.retain(|m| m.wire_id != wire_id);
                    }
                    self.menu = None; // indices shifted
                }
                Event::Error(e) => {
                    self.inviting = false;
                    self.joining = false;
                    self.status = format!("error: {e}").into();
                }
            }
        }
        // Auto-revert the "Copied" flash (the poll tick redraws us).
        if self.copied_at.is_some_and(|t| t.elapsed().as_millis() > 1400) {
            self.copied_at = None;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    /// Favorites first, otherwise as they were; the selection follows its chat.
    fn sort_chats(&mut self) {
        let sel = self.selected_id();
        self.chats.sort_by_key(|c| !c.favorite);
        if let Some(i) = sel.and_then(|id| self.chats.iter().position(|c| c.id == id)) {
            self.selected = i;
        }
        self.menu = None; // indices may have moved
    }

    fn mark(&self, contact_id: i64, mark: Mark, on: bool) {
        let _ = self.outbound.send(Cmd::Mark { contact_id, mark, on });
    }

    /// Emergency wipe, then start over as on first launch with a new worker.
    fn wipe(&mut self, keep_wallets: bool, window: &mut Window, cx: &mut Context<Self>) {
        let result = match contact::wipe(keep_wallets) {
            Ok(problem) => problem,
            Err(e) => {
                // Nothing was erased.
                self.confirm_wipe = None;
                self.status = format!("error: {e}").into();
                return;
            }
        };
        let (out_tx, out_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        spawn_worker(out_rx, ev_tx);
        let _ = out_tx.send(Cmd::Tao(TaoCmd::Refresh));
        self.wallet = None;
        self.outbound = out_tx;
        self.events = ev_rx;
        self.chats.clear();
        self.selected = 0;
        self.settings = Settings::load();
        Theme::change(ThemeMode::Light, Some(window), cx); // the default theme
        tint(self.settings.accent, cx);
        self.input_state.update(cx, |s, cx| s.set_value("", window, cx));
        self.draft_empty = true;
        self.confirm_wipe = None;
        self.show_settings = false;
        self.show_chat_info = false;
        self.menu = None;
        (self.inviting, self.joining) = (false, false);
        self.status = match result {
            None if keep_wallets => "wiped; your wallets were kept".into(),
            None => "wiped".into(),
            Some(e) => format!("error: {e}").into(),
        };
    }

    fn selected_id(&self) -> Option<i64> {
        self.chats.get(self.selected).map(|c| c.id)
    }

    /// Open a popup menu where the mouse went down.
    fn open_menu(&mut self, menu: Menu, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.menu = Some(menu);
        self.menu_at = at;
        cx.notify();
    }

    /// The open menu, floated over everything at `menu_at` (flipping to stay
    /// on screen); any press outside it closes it.
    fn popup(&self, card: Div, cx: &mut Context<Self>) -> AnyElement {
        let card = card.occlude().on_mouse_down_out(cx.listener(|this, _, _, cx| {
            this.menu = None;
            cx.notify();
        }));
        deferred(anchored().position(self.menu_at).child(card)).with_priority(1).into_any_element()
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

    /// Settings → TAO wallet: accounts, balances, add, phrase, network.
    fn wallet_section(&mut self, p: &Pal, cx: &mut Context<Self>) -> Div {
        let small = |t: String, c: u32| div().text_xs().text_color(rgb(c)).child(t);
        let mut col = v_flex().w_full().gap_2().pt_2().child(
            h_flex()
                .gap_3()
                .items_center()
                .child(div().text_sm().font_semibold().text_color(rgb(p.text)).child("τ TAO wallet"))
                .child(small(self.net_label().into(), p.sub))
                .child(div().flex_1())
                .child(
                    Button::new("tao-refresh")
                        .outline()
                        .label("Refresh")
                        .on_click(cx.listener(|this, _, _, _| this.tao(TaoCmd::Refresh))),
                )
                .child(Button::new("tao-new").outline().label("New account").on_click(cx.listener(
                    |this, _, _, _| {
                        this.tao(TaoCmd::New { name: "Main".into() });
                        this.tao(TaoCmd::Refresh);
                    },
                )))
                .child(
                    Switch::new("tao-testnet")
                        .label("Testnet")
                        .checked(self.settings.tao_net == "test")
                        .on_click(cx.listener(|this, v: &bool, _, cx| {
                            let net = if *v { Net::Test } else { Net::Main };
                            this.settings.tao_net = net.id().into();
                            this.tao(TaoCmd::SetNet { net });
                            cx.notify();
                        })),
                ),
        );
        let private = self.settings.private_payments;
        col = col.child(
            h_flex()
                .w_full()
                .gap_3()
                .items_center()
                .child(
                    v_flex()
                        .flex_1()
                        .gap_0p5()
                        .child(div().text_sm().text_color(rgb(p.text)).child("Private payments"))
                        .child(small(
                            match (private, self.settings.proxy_enabled) {
                                (false, _) => "stealth addresses, split parts, MEV Shield and random delays".into(),
                                (true, true) => "on: contacts get a private address; payments go out in shielded parts".into(),
                                (true, false) => "on · turn on the proxy (Tor) above to hide your IP too".into(),
                            },
                            if private && !self.settings.proxy_enabled { 0xFF9F0A } else { p.sub },
                        )),
                )
                .child(Switch::new("tao-private").checked(private).on_click(cx.listener(|this, v: &bool, _, cx| {
                    let mut s = Settings::load();
                    s.private_payments = *v;
                    s.save();
                    this.settings.private_payments = *v;
                    cx.notify();
                }))),
        );
        let w = self.wallet.clone();
        let active = w.as_ref().and_then(|w| w.active.clone());
        for (i, a) in w.iter().flat_map(|w| w.accounts.iter()).enumerate() {
            let is_active = active.as_deref() == Some(a.address.as_str());
            let (addr, addr2, addr3) = (a.address.clone(), a.address.clone(), a.address.clone());
            col = col.child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(small(if is_active { "▸".into() } else { " ".into() }, p.text))
                    .child(div().text_sm().text_color(rgb(p.text)).child(format!(
                        "{}{}",
                        a.name,
                        if a.watch_only { " (watch-only)" } else { "" }
                    )))
                    .child(div().flex_1().text_xs().text_color(rgb(p.sub)).child(a.address.clone()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(p.text))
                            .child(a.balance.map(tao::format_tao).unwrap_or_else(|| "…".into())),
                    )
                    .when_some(a.private_balance, |d, b| d.child(small(format!("+ {} private", tao::format_tao(b)), p.sub)))
                    .child(Button::new(("tao-copy", i)).ghost().label("Copy").on_click(cx.listener(
                        move |_, _, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(addr.clone())),
                    )))
                    .when(!is_active, |d| {
                        d.child(Button::new(("tao-use", i)).ghost().label("Use").on_click(cx.listener(
                            move |this, _, _, _| this.tao(TaoCmd::Use { address: addr2.clone() }),
                        )))
                    })
                    .when(!a.watch_only, |d| {
                        d.child(Button::new(("tao-phrase", i)).ghost().label("Phrase").on_click(
                            cx.listener(move |this, _, _, _| {
                                this.tao(TaoCmd::Reveal { address: addr3.clone() })
                            }),
                        ))
                    }),
            );
            if let (true, true, Some(meta)) = (private, is_active, a.meta.clone()) {
                let copy = meta.clone();
                col = col.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .pl_4()
                        .child(small("Private address".into(), p.sub))
                        .child(div().flex_1().min_w_0().text_xs().font_family("Menlo").text_color(rgb(p.text)).child(meta))
                        .child(Button::new(("tao-copy-meta", i)).ghost().label("Copy").on_click(cx.listener(
                            move |_, _, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone())),
                        ))),
                );
            }
        }
        col = col.child(
            h_flex()
                .w_full()
                .gap_2()
                .child(div().flex_1().child(Input::new(&self.tao_key)))
                .child(Button::new("tao-import").outline().label("Import phrase").on_click(cx.listener(
                    |this, _, window, cx| {
                        let phrase = this.tao_key.read(cx).value().to_string();
                        this.tao(TaoCmd::Import { name: "Imported".into(), phrase });
                        this.tao(TaoCmd::Refresh);
                        this.tao_key.update(cx, |s, cx| s.set_value("", window, cx));
                    },
                )))
                .child(Button::new("tao-watch").outline().label("Watch address").on_click(cx.listener(
                    |this, _, window, cx| {
                        let address = this.tao_key.read(cx).value().trim().to_string();
                        this.tao(TaoCmd::Watch { name: "Watch-only".into(), address });
                        this.tao(TaoCmd::Refresh);
                        this.tao_key.update(cx, |s, cx| s.set_value("", window, cx));
                    },
                ))),
        );
        let mailbox = w.as_ref().and_then(|w| w.mailbox.as_ref()).and_then(|m| m.address.clone());
        if let Some((addr, phrase)) = self.phrase.clone().filter(|(a, _)| Some(a) != mailbox.as_ref()) {
            col = col.child(
                v_flex()
                    .gap_1()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0xFF9F0A))
                    .child(small(
                        format!("Recovery phrase for {addr}: anyone with it controls the funds"),
                        0xFF9F0A,
                    ))
                    .child(div().text_sm().text_color(rgb(p.text)).child(phrase))
                    .child(Button::new("tao-hide").outline().label("Hide").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.phrase = None;
                            cx.notify();
                        },
                    ))),
            );
        }
        col
    }

    /// Chat details → TAO: amount, then send / request / cheque / share.
    fn tao_chat_row(&mut self, contact_id: i64, p: &Pal, cx: &mut Context<Self>) -> Div {
        fn act(this: &mut ChatApp, cx: &mut Context<ChatApp>, contact_id: i64, verb: &str) {
            let amount = this.tao_amount.read(cx).value().to_string();
            let Some(rao) = tao::parse_tao(&amount) else {
                this.status = "error: enter an amount like 1.5".into();
                return;
            };
            let wire_id = ulid::Ulid::new().to_string();
            let what = tao::format_tao(rao);
            match verb {
                "send" => {
                    let how = if this.settings.private_payments { " privately, in shielded parts over a few minutes" } else { "" };
                    let prompt = format!("Send {what} to their shared address on {}{how}?", this.net_label());
                    let cmd = TaoCmd::Pay { contact_id, wire_id, rao, to: None, re: None };
                    this.pending_tao = Some((prompt, cmd));
                }
                "cheque" => {
                    let prompt = format!("Sign a {what} cheque to them on {}?", this.net_label());
                    this.pending_tao = Some((prompt, TaoCmd::Cheque { contact_id, wire_id, rao }));
                }
                _ => this.tao(TaoCmd::Request { contact_id, wire_id, rao, memo: None }),
            }
        }
        let button = |id: &'static str, label: &'static str, verb: &'static str, cx: &mut Context<Self>| {
            Button::new(id).outline().label(label).on_click(cx.listener(move |this, _, _, cx| {
                act(this, cx, contact_id, verb);
                cx.notify();
            }))
        };
        let mut col = v_flex().w_full().gap_2().child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(div().text_sm().text_color(rgb(p.text)).child("TAO"))
                .child(div().w(px(110.)).child(Input::new(&self.tao_amount)))
                .child(button("tao-send", "Send", "send", cx))
                .child(button("tao-request", "Request", "request", cx))
                .child(button("tao-cheque", "Cheque", "cheque", cx))
                .child(Button::new("tao-share").ghost().label("Share address").on_click(cx.listener(
                    move |this, _, _, _| this.tao(TaoCmd::Share { contact_id }),
                ))),
        );
        if let Some((prompt, _)) = self.pending_tao.as_ref().map(|(p, _)| (p.clone(), ())) {
            col = col.child(self.confirm_row(prompt, cx));
        }
        col
    }

    /// Settings → Mailbox account: credits, deposit address, top-up, phrase.
    fn mailbox_rows(&mut self, p: &Pal, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let row = |label: &str, hint: Option<String>| {
            let mut l = v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().text_color(rgb(p.text)).child(label.to_string()));
            if let Some(h) = hint {
                l = l.child(div().text_xs().text_color(rgb(p.sub)).child(h));
            }
            h_flex().w_full().items_center().gap_3().child(l)
        };
        let w = self.wallet.clone();
        let Some(m) = w.as_ref().and_then(|w| w.mailbox.clone()) else {
            return vec![
                row(
                    "Set up your mailbox account",
                    Some("Pays for the chats you start. Joining someone's invite never needs one.".into()),
                )
                .child(Button::new("mb-new").primary().label("Set Up").on_click(cx.listener(|this, _, _, cx| {
                    this.tao(TaoCmd::MailboxOpen { phrase: None });
                    cx.notify();
                })))
                .into_any_element(),
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(div().text_xs().text_color(rgb(p.sub)).child("Already have one? Paste its recovery phrase, or a Hippius API token."))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.mailbox_key)))
                            .child(Button::new("mb-connect").outline().label("Connect").on_click(cx.listener(
                                |this, _, window, cx| {
                                    let key = this.mailbox_key.read(cx).value().trim().to_string();
                                    if key.split_whitespace().count() >= 12 {
                                        this.tao(TaoCmd::MailboxOpen { phrase: Some(key) });
                                    } else if !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric()) {
                                        this.settings.hippius_api_token = Some(key);
                                        this.settings.save();
                                        this.tao(TaoCmd::Refresh);
                                    } else {
                                        this.status = "error: that's neither a 12-word phrase nor an API token".into();
                                    }
                                    this.mailbox_key.update(cx, |s, cx| s.set_value("", window, cx));
                                    cx.notify();
                                },
                            ))),
                    )
                    .into_any_element(),
            ];
        };
        if m.busy && m.credits.is_none() {
            return vec![row("Setting up your account…", m.note.clone())
                .child(div().text_sm().text_color(rgb(p.sub)).child("…"))
                .into_any_element()];
        }
        let funded = m.credits.is_some_and(|c| c > 0.0);
        let credits = m.credits.map(|c| format!("${c:.2}")).unwrap_or_else(|| "…".into());
        let mut rows = vec![row(
            "Credits",
            Some(if funded { "Ready to start chats".into() } else { "Add at least $20 of TAO to start chats".into() }),
        )
        .child(
            div()
                .text_lg()
                .font_semibold()
                .text_color(rgb(if funded { 0x30D158 } else { 0xFF9F0A }))
                .child(credits),
        )
        .child(Button::new("mb-refresh").ghost().label("Refresh").on_click(cx.listener(|this, _, _, _| {
            this.tao(TaoCmd::Refresh)
        })))
        .into_any_element()];
        if let Some(n) = m.note.clone() {
            rows.push(div().text_sm().text_color(rgb(self.settings.accent)).child(n).into_any_element());
        }
        let payer = w.as_ref().and_then(|w| {
            w.accounts.iter().find(|a| w.active.as_deref() == Some(a.address.as_str()) && !a.watch_only)
        });
        if let (Some(rao), Some(a), Some(_)) = (m.fund_rao, payer, &m.deposit) {
            let prompt = format!(
                "Send {} (~$20) from {} to your Hippius deposit address on mainnet? TAO transfers are final.",
                tao::format_tao(rao),
                a.name
            );
            rows.push(
                row("Add $20 from TAO wallet", Some(format!("{} from {}", tao::format_tao(rao), a.name)))
                    .child(Button::new("mb-fund").primary().label("Add $20").disabled(m.busy).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.pending_tao = Some((prompt.clone(), TaoCmd::MailboxFund));
                            cx.notify();
                        },
                    )))
                    .into_any_element(),
            );
            if let Some((prompt, TaoCmd::MailboxFund)) = &self.pending_tao {
                rows.push(self.confirm_row(prompt.clone(), cx).into_any_element());
            }
        }
        if let Some(d) = m.deposit.clone() {
            let copy = d.clone();
            rows.push(
                v_flex()
                    .w_full()
                    .gap_1p5()
                    .child(
                        row("Deposit address", Some("Or send at least $20 of TAO here from any wallet. Never to the account address.".into()))
                            .child(Button::new("mb-copy").ghost().label("Copy").on_click(cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                                this.status = "Deposit address copied".into();
                                cx.notify();
                            }))),
                    )
                    .child(div().text_sm().font_family("Menlo").text_color(rgb(p.text)).child(d))
                    .into_any_element(),
            );
        }
        match (&m.address, self.phrase.clone()) {
            (Some(addr), Some((a, phrase))) if *addr == a => rows.push(
                v_flex()
                    .gap_1()
                    .child(div().text_xs().text_color(rgb(0xFF9F0A)).child(
                        "Recovery phrase: anyone with it controls your mailbox account and its credits",
                    ))
                    .child(div().text_sm().font_family("Menlo").text_color(rgb(p.text)).child(phrase))
                    .child(Button::new("mb-hide").outline().label("Hide").on_click(cx.listener(|this, _, _, cx| {
                        this.phrase = None;
                        cx.notify();
                    })))
                    .into_any_element(),
            ),
            (Some(addr), _) => rows.push(
                row("Recovery phrase", Some(format!("Account {addr} · back it up: a wipe erases it from this device")))
                    .child(Button::new("mb-phrase").outline().label("Show").on_click(cx.listener(|this, _, _, _| {
                        this.tao(TaoCmd::MailboxReveal)
                    })))
                    .into_any_element(),
            ),
            (None, _) => rows.push(row("Connected with an API token", None).into_any_element()),
        }
        rows
    }

    /// "Are you sure?" for anything that moves money.
    fn confirm_row(&mut self, prompt: String, cx: &mut Context<Self>) -> Div {
        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(div().flex_1().min_w_0().text_sm().text_color(rgb(0xFF9F0A)).child(prompt))
            .child(Button::new("tao-confirm").primary().label("Confirm").on_click(cx.listener(
                |this, _, _, cx| {
                    if let Some((_, cmd)) = this.pending_tao.take() {
                        this.tao(cmd);
                    }
                    cx.notify();
                },
            )))
            .child(Button::new("tao-cancel").ghost().label("Cancel").on_click(cx.listener(
                |this, _, _, cx| {
                    this.pending_tao = None;
                    cx.notify();
                },
            )))
    }

    /// The selected chat is on screen (if the window is focused).
    fn mark_read(&self) {
        if let (true, Some(id)) = (self.window_active, self.selected_id()) {
            let _ = self.outbound.send(Cmd::MarkRead { contact_id: id });
        }
    }

    /// Apply a validated timer locally and sync it to the selected chat.
    fn set_timer(&mut self, secs: u64) {
        self.settings.disappear_secs = secs;
        self.settings.save();
        if let Some(id) = self.selected_id() {
            let _ = self.outbound.send(Cmd::SetTimer { contact_id: id, secs });
        }
    }

    fn send_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input_state.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        if text.starts_with("sanctum://v2/") || text.starts_with("sanctum://g1/") {
            self.input_state.update(cx, |s, cx| s.set_value("", window, cx));
            self.draft_empty = true;
            let n = self.chats.len();
            let _ = self
                .outbound
                .send(Cmd::AcceptInvite { link: text, name: format!("Contact {}", n + 1) });
            return;
        }
        let Some(sel) = self.selected_id() else { return };
        let mut text = text;
        if let Some(arg) = text.strip_prefix("/pay ") {
            match pay::pay_token(arg) {
                Ok((token, _)) => text = token, // the chip carries the bearer warning
                Err(e) => {
                    self.status = format!("error: {e}").into();
                    cx.notify();
                    return;
                }
            }
        }
        self.input_state.update(cx, |s, cx| s.set_value("", window, cx));
        self.draft_empty = true;
        let wire_id = ulid::Ulid::new().to_string();
        if let Some(c) = self.chats.get_mut(self.selected) {
            c.msgs.push(Msg::now(wire_id.clone(), true, text.clone(), None));
            self.scroll.scroll_to_bottom();
        }
        let _ = self.outbound.send(Cmd::Send { contact_id: sel, wire_id, text });
        cx.notify();
    }

    /// Starting a chat needs a funded mailbox account. Sets one up the first
    /// time (joining never needs it) and opens Settings to show what's missing.
    fn mailbox_ready(&mut self) -> bool {
        // Unknown yet: let the worker decide.
        let Some(w) = &self.wallet else { return true };
        let why = match &w.mailbox {
            None => {
                self.tao(TaoCmd::MailboxOpen { phrase: None });
                "Setting up your mailbox account. It pays for the chats you start; joining is free."
            }
            Some(m) if m.busy && m.credits.is_none() => "Your mailbox account is still being set up…",
            Some(m) if m.credits.is_some_and(|c| c <= 0.0) => "Add $20 of credits to start chats.",
            Some(_) => return true,
        };
        self.status = why.into();
        self.show_settings = true;
        false
    }

    fn new_invite(&mut self, cx: &mut Context<Self>) {
        if self.inviting {
            return; // minting takes a few seconds; one at a time
        }
        if !self.mailbox_ready() {
            return cx.notify();
        }
        self.inviting = true;
        let n = self.chats.len();
        let _ = self.outbound.send(Cmd::NewInvite { name: format!("Contact {}", n + 1) });
        cx.notify();
    }

    fn attach_file(&mut self, cx: &mut Context<Self>) {
        let Some(sel) = self.selected_id() else { return };
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                if let Some(path) = paths.first() {
                    let path = path.to_string_lossy().into_owned();
                    let _ = this.update(cx, |this, cx| {
                        let name = std::path::Path::new(&path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "file".into());
                        let wire_id = ulid::Ulid::new().to_string();
                        if let Some(c) = this.chats.iter_mut().find(|c| c.id == sel) {
                            c.msgs.push(Msg::now(
                                wire_id.clone(),
                                true,
                                name.clone(),
                                Some((name, path.clone())),
                            ));
                            this.scroll.scroll_to_bottom();
                        }
                        let _ =
                            this.outbound.send(Cmd::SendFile { contact_id: sel, wire_id, path });
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn plus_glyph(color: u32, cx_size: f32) -> Vec<Div> {
        let mid = cx_size / 2.;
        let arm = cx_size * 0.45;
        vec![
            div()
                .absolute()
                .top(px(mid - 1.))
                .left(px(mid - arm / 2.))
                .w(px(arm))
                .h(px(2.))
                .rounded_full()
                .bg(rgb(color)),
            div()
                .absolute()
                .top(px(mid - arm / 2.))
                .left(px(mid - 1.))
                .w(px(2.))
                .h(px(arm))
                .rounded_full()
                .bg(rgb(color)),
        ]
    }

    fn sidebar(&mut self, p: &Pal, accent: u32, cx: &mut Context<Self>) -> Div {
        // Hover chip for row controls: must contrast with BOTH row states
        // (hover and selected), which p.hover/p.selected cannot.
        let chip_hover: u32 = if self.settings.theme == "dark" { 0x505053 } else { 0xCFCFD4 };
        let header = h_flex()
            .w_full()
            // Same fixed height as the main header: centers align exactly.
            .h(px(48.))
            .flex_none()
            .items_center()
            .justify_between()
            .px_3()
            .child(div().text_base().font_semibold().text_color(rgb(p.text)).child("Chats"))
            .child(
                div()
                    .id("new-invite")
                    .cursor_pointer()
                    .w(px(24.))
                    .h(px(24.))
                    .rounded_md()
                    .hover(|d| d.bg(rgb(p.hover)))
                    .relative()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                            this.open_menu(Menu::New, ev.position, cx)
                        }),
                    )
                    .children(Self::plus_glyph(p.sub, 24.)),
            );
        let mut col = v_flex()
            .w(px(248.))
            .h_full()
            .flex_none()
            .bg(rgb(p.surface))
            .border_r_1()
            .border_color(rgb(p.border))
            .child(header);
        if self.menu == Some(Menu::New) {
            let card = menu_card(p)
                .child(menu_item("pm-new", "New Invite", p.text, accent).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.menu = None;
                        this.new_invite(cx);
                    },
                )))
                .child(menu_item("pm-group", "New Group…", p.text, accent).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.menu = None;
                        this.show_group = this.mailbox_ready();
                        cx.notify();
                    },
                )))
                .child(menu_item("pm-join", "Join with Link…", p.text, accent).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        this.show_join = true;
                        cx.notify();
                    }),
                ));
            col = col.child(self.popup(card, cx));
        }

        let mut list =
            v_flex().id("chat-list").flex_1().min_h_0().overflow_y_scroll().px_2().gap_px();
        for (i, c) in self.chats.iter().enumerate() {
            let selected = i == self.selected;
            let menu_open = self.menu == Some(Menu::Chat(i));
            let status: Option<AnyElement> = if c.key_changed {
                Some(div().text_xs().text_color(rgb(0xFF453A)).child("⚠").into_any_element())
            } else if c.blocked {
                Some(div().text_xs().text_color(rgb(p.sub)).child("⊘").into_any_element())
            } else if c.unread > 0 {
                Some(
                    div()
                        .flex_none()
                        .min_w(px(18.))
                        .h(px(18.))
                        .px_1p5()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(rgb(accent))
                        .text_xs()
                        .font_semibold()
                        .text_color(rgb(0xFFFFFF))
                        .child(c.unread.to_string())
                        .into_any_element(),
                )
            } else if c.waiting {
                Some(div().size(px(8.)).rounded_full().bg(rgb(p.sub)).opacity(0.5).into_any_element())
            } else {
                None
            };
            let dots = div()
                .id(("chat-menu-btn", i))
                .cursor_pointer()
                .flex_none()
                .w(px(20.))
                .h(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .text_color(rgb(p.sub))
                .hover(move |d| d.bg(rgb(chip_hover)))
                // Shown on the hovered or selected row only: a calm list.
                .when(!selected && !menu_open, |d| {
                    d.opacity(0.).group_hover("chat-row", |s| s.opacity(1.))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                        this.open_menu(Menu::Chat(i), ev.position, cx)
                    }),
                )
                .child("⋯");
            let row = h_flex()
                .id(("chat", i))
                .group("chat-row")
                .cursor_pointer()
                .w_full()
                .px_2p5()
                .py_2()
                .rounded(px(10.))
                .when(selected, |d| d.bg(rgb(p.selected)))
                .when(!selected, |d| d.hover(|d| d.bg(rgb(p.hover))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected = i;
                    this.scroll.scroll_to_bottom();
                    this.mark_read();
                    if let Some(c) = this.chats.get_mut(i) {
                        c.unread = 0;
                    }
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                        this.open_menu(Menu::Chat(i), ev.position, cx)
                    }),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(1.))
                        .child(
                            h_flex()
                                .gap_1()
                                .items_center()
                                .when(c.favorite, |d| {
                                    d.child(div().text_xs().text_color(rgb(0xFFCC00)).child("★"))
                                })
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .font_medium()
                                        .text_color(rgb(p.text))
                                        .child(c.name.clone()),
                                )
                                .when(c.verified, |d| {
                                    d.child(div().text_xs().text_color(rgb(0x30D158)).child("✓"))
                                })
                                .child(div().flex_1())
                                .child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(rgb(p.sub))
                                        .children(c.msgs.last().map(|m| list_time(m.ts_ms))),
                                )
                                .child(dots),
                        )
                        .child(
                            h_flex()
                                .h(px(18.))
                                .gap_1()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(rgb(p.sub))
                                        .child(c.preview()),
                                )
                                .children(status),
                        ),
                );
            list = list.child(row);

            if menu_open {
                let id = c.id;
                let mut card = menu_card(p).child(
                    menu_item(("m-rename", i), "Rename…", p.text, accent).on_click(cx.listener(
                        move |this, _, window, cx| {
                            this.selected = i;
                            this.menu = None;
                            this.show_chat_info = true;
                            this.show_settings = false;
                            let name =
                                this.chats.get(i).map(|c| c.name.to_string()).unwrap_or_default();
                            this.rename_state.update(cx, |s, cx| s.set_value(&name, window, cx));
                            cx.notify();
                        },
                    )),
                );
                if id >= 0 {
                    let (fav, blocked) = (c.favorite, c.blocked);
                    card = card
                        .child(
                            menu_item(("m-fav", i), if fav { "Unfavorite" } else { "Favorite" }, p.text, accent)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.mark(id, Mark::Favorite, !fav);
                                    this.menu = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            menu_item(("m-block", i), if blocked { "Unblock" } else { "Block" }, p.text, accent)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.mark(id, Mark::Blocked, !blocked);
                                    this.menu = None;
                                    cx.notify();
                                })),
                        );
                }
                let links: Vec<String> = c.invite_link.split('\n').map(str::to_string).collect();
                let many = links.len() > 1;
                for (n, one) in links.into_iter().enumerate() {
                    if one.trim().is_empty() {
                        continue; // seat already claimed
                    }
                    let label = if many {
                        format!("Copy Member {} Invite", n + 2)
                    } else {
                        "Copy Invite Link".to_string()
                    };
                    card = card.child(menu_item(("m-copy", i * 10 + n), label, p.text, accent).on_click(
                        cx.listener(move |this, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(one.clone()));
                            this.menu = None;
                            cx.notify();
                        }),
                    ));
                }
                let clear = move |older_than_secs: Option<u64>| {
                    move |this: &mut Self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>| {
                        let _ = this.outbound.send(Cmd::ClearHistory { contact_id: id, older_than_secs });
                        this.menu = None;
                        cx.notify();
                    }
                };
                card = card
                    .child(menu_sep(p))
                    .child(menu_item(("m-clear", i), "Clear History", p.text, accent).on_click(cx.listener(clear(None))))
                    .child(
                        menu_item(("m-clear-week", i), "Clear Older Than a Week", p.text, accent)
                            .on_click(cx.listener(clear(Some(7 * 86400)))),
                    )
                    .child(menu_sep(p))
                    .child(
                        menu_item(("m-delete", i), "Delete Chat", 0xFF453A, 0xFF453A).on_click(
                            cx.listener(move |this, _, _, cx| {
                                let _ = this.outbound.send(Cmd::DeleteContact { contact_id: id });
                                this.menu = None;
                                cx.notify();
                            }),
                        ),
                    );
                col = col.child(self.popup(card, cx));
            }
        }
        if self.inviting || self.joining {
            list = list.child(
                div()
                    .w_full()
                    .px_2p5()
                    .py_2()
                    .text_sm()
                    .text_color(rgb(p.sub))
                    .child(if self.joining { "Joining…" } else { "Inviting…" })
                    .with_animation(
                        "inviting-shimmer",
                        Animation::new(Duration::from_millis(900)).repeat(),
                        |d, delta| d.opacity(0.35 + 0.65 * (1.0 - (delta - 0.5).abs() * 2.0)),
                    ),
            );
        }
        let footer_settings = h_flex()
            .id("app-settings")
            .cursor_pointer()
            .flex_none()
            .m_2()
            .items_center()
            .gap_1p5()
            .px_2p5()
            .py_1p5()
            .rounded(px(8.))
            .text_sm()
            .text_color(rgb(p.sub))
            .when(self.show_settings, |d| d.bg(rgb(p.selected)).text_color(rgb(p.text)))
            .hover(|d| d.bg(rgb(p.hover)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_settings = !this.show_settings;
                this.show_chat_info = false;
                this.confirm_wipe = None;
                cx.notify();
            }))
            .child(
                Icon::new(gpui_kit::assets::IconName::Settings)
                    .with_size(px(14.))
                    .text_color(rgb(p.sub)),
            )
            .child("Settings");
        col.child(list).child(footer_settings)
    }
}

impl Render for ChatApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = self.settings.theme == "dark";
        let p = pal(dark);
        let accent = self.settings.accent;
        let show_time = self.settings.timestamps;
        let peer_name: SharedString = self
            .chats
            .get(self.selected)
            .map(|c| match (c.verified, c.blocked) {
                (_, true) => format!("{} (blocked)", c.name).into(),
                (true, _) => format!("{} ✓", c.name).into(),
                _ => c.name.clone(),
            })
            .unwrap_or_else(|| "Sanctum".into());
        let error_line = self.status.starts_with("error").then(|| self.status.clone());

        // Sidebar toggle: two bars suggesting a panel edge.
        let toggle = div()
            .id("sidebar-toggle")
            .cursor_pointer()
            .w(px(24.))
            .h(px(24.))
            .rounded_md()
            .hover(|d| d.bg(rgb(p.hover)))
            .relative()
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_sidebar = !this.show_sidebar;
                cx.notify();
            }))
            .child(
                div()
                    .absolute()
                    .top(px(6.))
                    .left(px(5.))
                    .w(px(14.))
                    .h(px(12.))
                    .rounded_sm()
                    .border_1()
                    .border_color(rgb(p.sub)),
            )
            .child(div().absolute().top(px(6.)).left(px(10.)).w(px(1.)).h(px(12.)).bg(rgb(p.sub)));

        let panel_open = self.show_settings || (self.show_chat_info && self.selected_id().is_some());
        let title: SharedString = if self.show_settings {
            "Settings".into()
        } else if panel_open {
            "Details".into()
        } else {
            peer_name
        };
        let header = h_flex()
            .w_full()
            .h(px(48.))
            .flex_none()
            .items_center()
            .px_3()
            .border_b_1()
            .border_color(rgb(p.border))
            .child(toggle)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .child(div().text_sm().font_semibold().text_color(rgb(p.text)).child(title))
                    .children(error_line.map(|e| {
                        div().max_w_full().truncate().text_xs().text_color(rgb(0xFF453A)).child(e)
                    })),
            )
            .child(if panel_open {
                Button::new("panel-done").ghost().label("Done").on_click(cx.listener(|this, _, _, cx| {
                    this.show_settings = false;
                    this.show_chat_info = false;
                    this.confirm_wipe = None;
                    cx.notify();
                }))
            } else {
                Button::new("chat-info").ghost().label("⋯").on_click(cx.listener(
                    |this, _, window, cx| {
                        this.show_chat_info = true;
                        let name =
                            this.chats.get(this.selected).map(|c| c.name.to_string()).unwrap_or_default();
                        this.rename_state.update(cx, |s, cx| s.set_value(&name, window, cx));
                        cx.notify();
                    },
                ))
            });

        let row = |label: &str, hint: Option<&str>| {
            let mut l = v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(div().text_sm().text_color(rgb(p.text)).child(label.to_string()));
            if let Some(h) = hint {
                l = l.child(div().text_xs().text_color(rgb(p.sub)).child(h.to_string()));
            }
            h_flex().w_full().items_center().gap_3().child(l)
        };
        // Small bordered text button (Set, Save, Wipe…).
        let pill = |id: &'static str, label: &'static str, color: u32| {
            div()
                .id(id)
                .flex_none()
                .cursor_pointer()
                .px_3()
                .py_1()
                .rounded(px(7.))
                .border_1()
                .border_color(rgb(if color == p.text { p.border } else { color }))
                .hover(|d| d.bg(rgb(p.hover)))
                .text_sm()
                .text_color(rgb(color))
                .child(label)
        };
        let footer = || {
            div()
                .w_full()
                .text_center()
                .text_xs()
                .text_color(rgb(p.sub))
                .child("End-to-end encrypted · mailboxes on Hippius (Bittensor SN75)")
        };

        let settings_panel = self.show_settings.then(|| {
            // Light | Dark, as a segmented control.
            let (track, knob) = if dark { (0x1C1C1E, 0x48484A) } else { (0xE9E9EB, 0xFFFFFF) };
            let seg = |id: &'static str, label: &'static str, on: bool| {
                div()
                    .id(id)
                    .cursor_pointer()
                    .px_3()
                    .py(px(3.))
                    .rounded(px(6.))
                    .text_sm()
                    .text_color(rgb(p.text))
                    .when(on, |d| d.bg(rgb(knob)).shadow_sm())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let dark = label == "Dark";
                        this.settings.theme = if dark { "dark".into() } else { "light".into() };
                        this.settings.save();
                        Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, Some(window), cx);
                        tint(this.settings.accent, cx);
                        cx.notify();
                    }))
                    .child(label)
            };
            let theme = h_flex()
                .p(px(2.))
                .rounded(px(8.))
                .bg(rgb(track))
                .child(seg("theme-light", "Light", !dark))
                .child(seg("theme-dark", "Dark", dark));

            // Accent swatches.
            let mut swatches = h_flex().gap_2();
            for (name, color) in ACCENTS {
                let active = accent == color;
                swatches = swatches.child(
                    div()
                        .id(name)
                        .cursor_pointer()
                        .size(px(22.))
                        .rounded_full()
                        .border_2()
                        .border_color(rgb(if active { p.text } else { p.raised }))
                        .bg(rgb(color))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.settings.accent = color;
                            tint(color, cx);
                            this.settings.save();
                            cx.notify();
                        })),
                );
            }

            let wipe = |id: &'static str, keep: bool| {
                let armed = self.confirm_wipe == Some(keep);
                pill(id, if armed { "Click again to erase" } else { "Erase…" }, 0xFF453A)
                    .when(armed, |d| d.bg(rgb(0xFF453A)).text_color(rgb(0xFFFFFF)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if this.confirm_wipe == Some(keep) {
                            this.wipe(keep, window, cx);
                        } else {
                            this.confirm_wipe = Some(keep);
                        }
                        cx.notify();
                    }))
            };
            let (wipe_keep, wipe_all) = (wipe("wipe-keep", true), wipe("wipe-all", false));

            let mailbox = self.mailbox_rows(&p, cx);
            pane(vec![
                section(&p, "Mailbox account", mailbox),
                section(&p, "Appearance", vec![
                    row("Theme", None).child(theme).into_any_element(),
                    row("Accent", None).child(swatches).into_any_element(),
                ]),
                section(&p, "Messages & network", vec![
                    row("Timestamps", Some("a time divider after five quiet minutes"))
                        .child(Switch::new("set-ts").checked(self.settings.timestamps).on_click(cx.listener(
                            |this, v: &bool, _, cx| {
                                this.settings.timestamps = *v;
                                this.settings.save();
                                cx.notify();
                            },
                        )))
                        .into_any_element(),
                    row("Read receipts", Some("send yours and see theirs · off hides both"))
                        .child(Switch::new("set-read").checked(self.settings.read_receipts).on_click(
                            cx.listener(|this, v: &bool, _, cx| {
                                this.settings.read_receipts = *v;
                                this.settings.save();
                                cx.notify();
                            }),
                        ))
                        .into_any_element(),
                    row(
                        "Route via proxy (Tor / UR)",
                        Some(&format!("{} · applies on restart", self.settings.proxy_url)),
                    )
                    .child(Switch::new("set-proxy").checked(self.settings.proxy_enabled).on_click(
                        cx.listener(|this, v: &bool, _, cx| {
                            this.settings.proxy_enabled = *v;
                            this.settings.save();
                            cx.notify();
                        }),
                    ))
                    .into_any_element(),
                ]),
                section(&p, "TAO wallet", vec![self.wallet_section(&p, cx).into_any_element()]),
                section(&p, "Emergency wipe · offline, instant", vec![
                    row("Erase chats, keep wallets", Some("every chat, contact, key, file and setting goes; your TAO and mailbox accounts stay, so no money is lost"))
                        .child(wipe_keep)
                        .into_any_element(),
                    row("Erase everything", Some("every trace of Sanctum on this device, wallets included: funds are gone unless you saved the recovery phrases"))
                        .child(wipe_all)
                        .into_any_element(),
                ]),
                footer(),
            ])
        });

        // Per-chat details behind the ⋯ button.
        let chat_info_panel = (self.show_chat_info && self.selected_id().is_some()).then(|| {
            let (id, name) =
                self.chats.get(self.selected).map(|c| (c.id, c.name.clone())).unwrap_or_default();
            let mut sections = vec![div()
                .w_full()
                .pb_1()
                .text_center()
                .text_xl()
                .font_semibold()
                .text_color(rgb(p.text))
                .child(name)];
            if id >= 0 {
                sections.push(section(&p, "Payments", vec![self.tao_chat_row(id, &p, cx).into_any_element()]));
            }
            let rename = pill("rename-save", "Save", p.text).on_click(cx.listener(move |this, _, window, cx| {
                let name = this.rename_state.read(cx).value().trim().to_string();
                if !name.is_empty() {
                    let _ = this.outbound.send(Cmd::Rename { contact_id: id, name: name.clone() });
                    if let Some(c) = this.chats.iter_mut().find(|c| c.id == id) {
                        c.name = name.into();
                    }
                    // Leave the field and close the pane —
                    // the renamed sidebar row is the confirmation.
                    window.blur(cx);
                    this.show_chat_info = false;
                    cx.notify();
                }
            }));
            let timer_set = pill("timer-set", "Set", p.text).on_click(cx.listener(|this, _, window, cx| {
                let text = this.timer_state.read(cx).value().to_string();
                match demo::parse_disappear(&text) {
                    Some(secs) => {
                        this.set_timer(secs);
                        this.timer_state.update(cx, |s, cx| s.set_value("", window, cx));
                        window.blur(cx);
                    }
                    None => {
                        this.status = "error: timer must be off or 1 minute to 1 year, e.g. 90m, 2w, 6mo".into();
                    }
                }
                cx.notify();
            }));
            sections.push(section(&p, "Chat", vec![
                row("Name", None)
                    .child(div().w(px(180.)).child(Input::new(&self.rename_state)))
                    .child(rename)
                    .into_any_element(),
                row("Disappearing messages", Some("timed local deletion, synced to contact"))
                    .child(
                        Button::new("set-disappear")
                            .outline()
                            .label(demo::disappear_label(self.settings.disappear_secs))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_timer(demo::next_disappear_preset(this.settings.disappear_secs));
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
                row("Custom timer", Some("1 minute to 1 year"))
                    .child(div().w(px(140.)).child(Input::new(&self.timer_state)))
                    .child(timer_set)
                    .into_any_element(),
            ]));
            if let Some(c) = self.chats.get(self.selected).filter(|c| c.id >= 0) {
                let (verified, blocked) = (c.verified, c.blocked);
                let hint = if c.safety.is_empty() {
                    "appears once they join"
                } else {
                    "same digits on both screens? then it's really them"
                };
                let mut rows = vec![row("Safety number", Some(hint))
                    .when(!c.safety.is_empty(), |r| {
                        r.child(
                            Button::new("verify")
                                .outline()
                                .label(if verified { "Verified ✓" } else { "Mark Verified" })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.mark(id, Mark::Verified, !verified);
                                    cx.notify();
                                })),
                        )
                    })
                    .into_any_element()];
                if !c.safety.is_empty() {
                    rows.push(
                        div()
                            .w_full()
                            .text_center()
                            .font_family("Menlo")
                            .text_base()
                            .text_color(rgb(p.text))
                            .child(c.safety.clone())
                            .into_any_element(),
                    );
                }
                rows.push(
                    row("Block", Some("drop their messages unseen · kept on this device"))
                        .child(Switch::new("set-block").checked(blocked).on_click(cx.listener(
                            move |this, v: &bool, _, cx| {
                                this.mark(id, Mark::Blocked, *v);
                                cx.notify();
                            },
                        )))
                        .into_any_element(),
                );
                sections.push(section(&p, "Security", rows));
            }
            sections.push(section(&p, "Danger zone", vec![row("Delete this chat", Some("retires its mailboxes; removes history"))
                .child(pill("delete-chat", "Delete", 0xFF453A).on_click(cx.listener(move |this, _, _, cx| {
                    let _ = this.outbound.send(Cmd::DeleteContact { contact_id: id });
                    this.show_chat_info = false;
                    cx.notify();
                })))
                .into_any_element()]));
            sections.push(footer());
            pane(sections)
        });

        // Inline strip under the header: an inset card, not a bordered band.
        let strip = || {
            h_flex()
                .flex_none()
                .gap_2()
                .items_center()
                .mx_3()
                .mt_2()
                .px_3()
                .py_2()
                .rounded(px(12.))
                .bg(rgb(p.surface))
        };
        let group_panel = self.show_group.then(|| {
            strip()
                .child(div().text_sm().font_medium().text_color(rgb(p.text)).child("New group"))
                .child(div().flex_1().child(Input::new(&self.group_state)))
                .child(
                    pill("group-size", "", p.text)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.group_size = if this.group_size >= 6 { 2 } else { this.group_size + 1 };
                            cx.notify();
                        }))
                        .child(format!("{} members", self.group_size)),
                )
                .child(Button::new("group-go").primary().label("Create").on_click(cx.listener(
                    |this, _, window, cx| {
                        if this.inviting {
                            return;
                        }
                        let name = this.group_state.read(cx).value().trim().to_string();
                        let name = if name.is_empty() { "Group".to_string() } else { name };
                        this.inviting = true;
                        let _ = this.outbound.send(Cmd::NewGroup { name, size: this.group_size });
                        this.group_state.update(cx, |s, cx| s.set_value("", window, cx));
                        window.blur(cx);
                        this.show_group = false;
                        cx.notify();
                    },
                )))
                .child(Button::new("group-cancel").ghost().label("Cancel").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.show_group = false;
                        cx.notify();
                    },
                )))
        });

        let join_panel = self.show_join.then(|| {
            strip()
                .child(div().text_sm().font_medium().text_color(rgb(p.text)).child("Join"))
                .child(div().flex_1().child(Input::new(&self.join_state)))
                .child(
                    Button::new("join-go")
                        .primary()
                        .label(if self.joining { "Joining…" } else { "Join" })
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.joining {
                                return;
                            }
                            let link = this.join_state.read(cx).value().trim().to_string();
                            if link.starts_with("sanctum://v2/") || link.starts_with("sanctum://g1/") {
                                this.joining = true;
                                let n = this.chats.len();
                                let _ = this.outbound.send(Cmd::AcceptInvite {
                                    link,
                                    name: format!("Contact {}", n + 1),
                                });
                                this.join_state.update(cx, |s, cx| s.set_value("", window, cx));
                                window.blur(cx);
                                cx.notify();
                            }
                        })),
                )
                .child(Button::new("join-cancel").ghost().label("Cancel").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.show_join = false;
                        cx.notify();
                    },
                )))
        });

        let banner_link = self
            .chats
            .get(self.selected)
            .filter(|c| !c.invite_link.trim().is_empty())
            .map(|c| c.invite_link.clone());
        let copied = self.copied_at.is_some();
        let banner = banner_link.map(|link| {
            let links: Vec<String> = link.split('\n').map(str::to_string).collect();
            let many = links.len() > 1;
            let mut card = strip()
                .flex_col()
                .items_start()
                .py_3()
                .child(div().text_sm().font_semibold().text_color(rgb(p.text)).child("Invitation pending"))
                .child(div().text_xs().text_color(rgb(p.sub)).child(if many {
                    "One link per member — each seat is single-use. No accounts needed."
                } else {
                    "Share this link. It carries everything they need — no account, no signup."
                }));
            for (n, one) in links.into_iter().enumerate() {
                if one.is_empty() {
                    continue; // seat already claimed
                }
                let short = format!("{}…", &one[..one.len().min(40)]);
                card = card.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .when(many, |d| {
                            d.child(div().text_xs().text_color(rgb(p.sub)).child(format!("Member {}", n + 2)))
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .px_2()
                                .py_1()
                                .rounded(px(7.))
                                .bg(rgb(p.bg))
                                .font_family("Menlo")
                                .text_xs()
                                .text_color(rgb(p.sub))
                                .child(short),
                        )
                        .child(
                            div()
                                .id(("copy-invite", n))
                                .flex_none()
                                .cursor_pointer()
                                .px_3()
                                .py_1()
                                .rounded(px(7.))
                                .text_sm()
                                .when(copied, |d| d.bg(rgb(0x30D158)).text_color(rgb(0xFFFFFF)).child("Copied"))
                                .when(!copied, |d| {
                                    d.bg(rgb(accent)).text_color(rgb(0xFFFFFF)).hover(|d| d.opacity(0.85)).child("Copy")
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(one.clone()));
                                    this.copied_at = Some(std::time::Instant::now());
                                    cx.notify();
                                })),
                        ),
                );
            }
            card
        });

        let key_warning = self.chats.get(self.selected).filter(|c| c.key_changed).map(|_| {
            strip()
                .flex_col()
                .items_start()
                .gap_1()
                .bg(rgb(if dark { 0x3A2A2A } else { 0xFFF0F0 }))
                .child(div().text_sm().font_semibold().text_color(rgb(0xFF453A)).child("Identity key changed"))
                .child(div().text_xs().text_color(rgb(p.text)).child(
                    "Someone wrote to this chat under a different identity key. It was refused. \
                     Compare safety numbers in person (⋯ → Safety number), then mark verified.",
                ))
        });

        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(chat) = self.chats.get(self.selected) {
            // Only the newest finished send carries Sent / Delivered / Read.
            let last_sent = chat.msgs.iter().rposition(|m| m.mine && m.sent && !m.failed);
            let chat_id = chat.id;
            // One sender's messages close together form a run: tight spacing
            // and flattened inner corners, as on iOS.
            let same_run = |a: &Msg, b: &Msg| {
                a.mine == b.mine
                    && a.tao.is_none()
                    && b.tao.is_none()
                    && a.sender_seat == b.sender_seat
                    && a.day == b.day
                    && b.ts_ms - a.ts_ms <= 5 * 60 * 1000
            };
            for (i, m) in chat.msgs.iter().enumerate() {
                let prev = i.checked_sub(1).and_then(|j| chat.msgs.get(j));
                let new_day = prev.is_none_or(|pm| pm.day != m.day);
                let gap = prev.is_none_or(|pm| m.ts_ms - pm.ts_ms > 5 * 60 * 1000);
                if new_day || (show_time && gap) {
                    rows.push(
                        div()
                            .w_full()
                            .text_center()
                            .text_xs()
                            .font_medium()
                            .text_color(rgb(p.sub))
                            .pt_4()
                            .pb_1()
                            .child(time_header(m.ts_ms))
                            .into_any_element(),
                    );
                }
                let first = !prev.is_some_and(|pm| same_run(pm, m));
                let last = !chat.msgs.get(i + 1).is_some_and(|nm| same_run(m, nm));
                let image = m.file.as_ref().is_some_and(|(name, _)| is_image(name));
                let contact_id = chat.id;
                let reactable = !m.mine && !m.wire_id.is_empty();
                let unsendable = m.mine && !m.delivered && !m.failed && chat.id >= 0;

                let (big, small) = (px(18.), px(5.));
                let bubble = div()
                    .id(("msg", i))
                    .relative()
                    .max_w(relative(0.72))
                    .text_sm()
                    .rounded(big)
                    .when(m.mine && !first, |d| d.rounded_tr(small))
                    .when(m.mine && !last, |d| d.rounded_br(small))
                    .when(!m.mine && !first, |d| d.rounded_tl(small))
                    .when(!m.mine && !last, |d| d.rounded_bl(small));
                let bubble = if m.tao.is_some() {
                    // Payment cards: a quiet surface with an accent edge.
                    bubble
                        .min_w(px(200.))
                        .p_3()
                        .bg(rgb(p.surface))
                        .border_1()
                        .border_color(rgba(accent << 8 | 0x66))
                        .text_color(rgb(p.text))
                } else if image {
                    bubble
                } else if m.mine {
                    bubble.px_3().py(px(7.)).bg(rgb(accent)).text_color(rgb(0xFFFFFF))
                } else {
                    bubble.px_3().py(px(7.)).bg(rgb(p.bubble)).text_color(rgb(p.bubble_text))
                };

                let label = |t: &'static str| {
                    div().text_xs().font_semibold().text_color(rgb(p.sub)).child(t)
                };
                let content: AnyElement = if let Some(card) = m.tao.as_ref().filter(|c| c.kind == TaoKind::Address) {
                    let addr = card.to.clone();
                    // Two clean halves rather than an arbitrary wrap.
                    let mid = addr.char_indices().nth(addr.chars().count() / 2).map_or(addr.len(), |(b, _)| b);
                    v_flex()
                        .gap_1p5()
                        .child(label(match (m.mine, addr.starts_with(sanctum_core::stealth::META_PREFIX)) {
                            (true, true) => "τ  YOUR PRIVATE TAO ADDRESS",
                            (false, true) => "τ  PRIVATE TAO ADDRESS",
                            (true, false) => "τ  YOUR TAO ADDRESS",
                            (false, false) => "τ  TAO ADDRESS",
                        }))
                        .child(
                            div()
                                .text_sm()
                                .font_family("Menlo")
                                .child(format!("{}\n{}", &addr[..mid], &addr[mid..])),
                        )
                        .child(
                            h_flex().child(
                                Button::new(("tao-addr-copy", i)).outline().small().label("Copy").on_click(
                                    cx.listener(move |_, _, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(addr.clone()))
                                    }),
                                ),
                            ),
                        )
                        .into_any_element()
                } else if let Some(card) = &m.tao {
                    let mut c = v_flex()
                        .gap_1()
                        .child(label(match (card.kind, m.mine) {
                            (TaoKind::Request, true) => "τ  YOU REQUESTED",
                            (TaoKind::Request, false) => "τ  REQUEST",
                            (TaoKind::Cheque, _) => "τ  CHEQUE",
                            (TaoKind::Paid, true) => "τ  YOU PAID",
                            (TaoKind::Paid, false) => "τ  PAYMENT",
                            (TaoKind::Address, _) => "τ  TAO ADDRESS", // handled above
                        }))
                        .child(div().text_2xl().font_semibold().child(tao::format_tao(card.amount())))
                        .child(div().text_xs().text_color(rgb(p.sub)).child(m.text.clone()));
                    let wid = m.wire_id.clone();
                    if !m.mine && card.kind == TaoKind::Request && card.status == PayStatus::Open {
                        let (to, rao) = (card.to.clone(), card.amount());
                        let what = tao::format_tao(rao);
                        c = c.child(div().pt_1().child(Button::new(("tao-pay", i)).primary().w_full().label(format!("Pay {what}")).on_click(
                            cx.listener(move |this, _, _, cx| {
                                let prompt = format!("Pay {what} to {to} on {}?", this.net_label());
                                let cmd = TaoCmd::Pay {
                                    contact_id: chat_id,
                                    wire_id: ulid::Ulid::new().to_string(),
                                    rao,
                                    to: Some(to.clone()),
                                    re: Some(wid.clone()),
                                };
                                this.pending_tao = Some((prompt, cmd));
                                this.show_chat_info = true;
                                cx.notify();
                            }),
                        )));
                    } else if !m.mine
                        && card.kind == TaoKind::Cheque
                        && matches!(card.status, PayStatus::Pending | PayStatus::Bounced)
                    {
                        c = c.child(div().pt_1().child(Button::new(("tao-cash", i)).primary().w_full().label("Cash Cheque").on_click(
                            cx.listener(move |this, _, _, _| {
                                this.tao(TaoCmd::Cash { contact_id: chat_id, wire_id: wid.clone() })
                            }),
                        )));
                    }
                    c.into_any_element()
                } else if let Some((name, path)) = &m.file {
                    if image {
                        img(std::path::PathBuf::from(path))
                            .max_w(px(240.))
                            .max_h(px(320.))
                            .rounded(px(14.))
                            .into_any_element()
                    } else {
                        div().font_semibold().child(format!("📄 {name}")).into_any_element()
                    }
                } else {
                    div().child(m.text.clone()).into_any_element()
                };

                // A tapback sits on the bubble's outer top corner, iMessage-style.
                let badge = m.reaction.clone().map(|r| {
                    div()
                        .absolute()
                        .top(px(-14.))
                        .when(m.mine, |d| d.left(px(-12.)))
                        .when(!m.mine, |d| d.right(px(-12.)))
                        .px(px(5.))
                        .py(px(1.))
                        .rounded_full()
                        .bg(rgb(p.bubble))
                        .border_2()
                        .border_color(rgb(p.bg))
                        .text_xs()
                        .child(r)
                });
                let open_at = move |this: &mut Self, ev: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>| {
                    this.open_menu(Menu::Msg(i), ev.position, cx)
                };
                let bubble = bubble
                    .child(content)
                    .children(badge)
                    .on_mouse_down(MouseButton::Right, cx.listener(open_at))
                    // Click a received message to react; cards keep clicks for their buttons.
                    .when(reactable && m.tao.is_none(), |d| {
                        d.cursor_pointer().on_mouse_down(MouseButton::Left, cx.listener(open_at))
                    });

                // Hover a message to see its time beside it.
                let time = div()
                    .flex_none()
                    .text_xs()
                    .text_color(rgb(p.sub))
                    .opacity(0.)
                    .group_hover("msg-row", |s| s.opacity(1.))
                    .child(m.time.clone());
                let row = h_flex().group("msg-row").w_full().items_center().gap_2();
                let row = if m.mine {
                    row.justify_end().child(time).child(bubble)
                } else {
                    row.justify_start().child(bubble).child(time)
                };

                let sender_label = (!m.mine && first).then_some(m.sender_seat).flatten().map(|seat| {
                    div()
                        .ml_3()
                        .mb(px(2.))
                        .text_xs()
                        .text_color(rgb(p.sub))
                        .child(format!("Member {}", seat + 1))
                });
                let top = match (first, m.reaction.is_some()) {
                    (_, true) => px(14.),
                    (true, false) => px(8.),
                    (false, false) => px(2.),
                };
                let mut col = v_flex()
                    .w_full()
                    .mt(top)
                    .when(m.mine, |d| d.items_end())
                    .when(!m.mine, |d| d.items_start())
                    .children(sender_label)
                    .child(row);
                for (n, l) in m.pay.iter().enumerate() {
                    let icon = match l.kind {
                        PayKind::Cashu => "🥜",
                        PayKind::Lightning => "⚡",
                        PayKind::Bitcoin => "₿",
                        PayKind::Zcash => "ⓩ",
                    };
                    let (uri, raw) = (l.uri.clone(), l.raw.clone());
                    let action = |id: &'static str, label: &'static str| {
                        div()
                            .id((id, i * 10 + n))
                            .cursor_pointer()
                            .text_xs()
                            .font_medium()
                            .text_color(rgb(accent))
                            .hover(|d| d.underline())
                            .child(label)
                    };
                    col = col.child(
                        h_flex()
                            .mt_1()
                            .gap_2()
                            .items_center()
                            .px_3()
                            .py_1p5()
                            .rounded(px(14.))
                            .border_1()
                            .border_color(rgb(p.border))
                            .bg(rgb(p.well))
                            .text_sm()
                            .text_color(rgb(p.text))
                            .child(icon)
                            .child(
                                v_flex()
                                    .child(div().font_semibold().child(l.label.clone()))
                                    .children(l.memo.clone().map(|memo| {
                                        div().text_xs().text_color(rgb(p.sub)).child(memo)
                                    })),
                            )
                            // Hands the URI to whatever wallet claims the scheme.
                            .child(action("pay-open", "Open").on_click(cx.listener(
                                move |_, _, _, cx| cx.open_url(&uri),
                            )))
                            .child(action("pay-copy", "Copy").on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(raw.clone()));
                                    this.status = "copied".into();
                                    cx.notify();
                                },
                            ))),
                    );
                    if l.kind == PayKind::Cashu {
                        col = col.child(div().text_xs().text_color(rgb(p.sub)).child(pay::BEARER_WARNING));
                    }
                }
                // Each send shows its own upload bar (easing toward 90%) until
                // it lands; one that never left gets a red mark instead.
                if m.failed {
                    col = col.child(
                        div().mt_1().mr_1().text_xs().text_color(rgb(0xFF453A)).child("Not sent"),
                    );
                } else if m.mine && !m.sent {
                    col = col.child(
                        div().mt_1().mr_1().w(px(64.)).h(px(3.)).rounded_full().bg(rgb(p.well)).child(
                            div().h_full().rounded_full().bg(rgb(accent)).with_animation(
                                ("send-bar", i),
                                Animation::new(Duration::from_secs(8)),
                                |d, t| d.w(relative(0.9 * (1.0 - (1.0 - t).powi(3)))),
                            ),
                        ),
                    );
                } else if Some(i) == last_sent {
                    let receipt = match m.read_ms.filter(|_| self.settings.read_receipts) {
                        Some(at) => format!("Read {}", local(at).0.format("%-I:%M %p")),
                        None if m.delivered => "Delivered".into(),
                        None => "Sent".into(),
                    };
                    col = col.child(
                        div().mt_1().mr_1().text_xs().font_medium().text_color(rgb(p.sub)).child(receipt),
                    );
                }

                if self.menu == Some(Menu::Msg(i)) {
                    let mut card = menu_card(&p);
                    if reactable {
                        let mut tapbacks = h_flex().gap_0p5().p_0p5();
                        for (n, emoji) in REACTIONS.iter().enumerate() {
                            let w = m.wire_id.clone();
                            let current = m.reaction.as_deref() == Some(*emoji);
                            tapbacks = tapbacks.child(
                                div()
                                    .id(("react", i * 10 + n))
                                    .cursor_pointer()
                                    .size(px(30.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .text_lg()
                                    .when(current, |d| d.bg(rgba(accent << 8 | 0x40)))
                                    .hover(|d| d.bg(rgb(p.hover)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let _ = this.outbound.send(Cmd::React {
                                            contact_id,
                                            ref_id: w.clone(),
                                            emoji: (*emoji).into(),
                                        });
                                        if let Some(c) =
                                            this.chats.iter_mut().find(|c| c.id == contact_id)
                                        {
                                            if let Some(m) = c.msgs.get_mut(i) {
                                                m.reaction = Some((*emoji).into());
                                            }
                                        }
                                        this.menu = None;
                                        cx.notify();
                                    }))
                                    .child(*emoji),
                            );
                        }
                        card = card.child(tapbacks).child(menu_sep(&p));
                    }
                    if m.file.is_none() {
                        let text = m.text.to_string();
                        card = card.child(menu_item(("msg-copy", i), "Copy", p.text, accent).on_click(
                            cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                this.menu = None;
                                cx.notify();
                            }),
                        ));
                    }
                    if unsendable {
                        let w = m.wire_id.clone();
                        card = card.child(menu_item(("msg-unsend", i), "Unsend", 0xFF453A, 0xFF453A).on_click(
                            cx.listener(move |this, _, _, cx| {
                                let _ = this.outbound.send(Cmd::Unsend { contact_id, wire_id: w.clone() });
                                this.menu = None;
                                cx.notify();
                            }),
                        ));
                    }
                    let delete = |id: &'static str, label: &'static str, everyone: bool| {
                        let w = m.wire_id.clone();
                        menu_item((id, i), label, 0xFF453A, 0xFF453A).on_click(cx.listener(
                            move |this, _, _, cx| {
                                let _ = this.outbound.send(Cmd::DeleteMsg {
                                    contact_id,
                                    wire_id: w.clone(),
                                    everyone,
                                });
                                this.menu = None;
                                cx.notify();
                            },
                        ))
                    };
                    // Undelivered own messages already have unsend.
                    if m.mine && m.delivered && contact_id >= 0 {
                        card = card.child(delete("del-all", "Delete for Everyone", true));
                    }
                    card = card.child(delete("del-me", "Delete for Me", false));
                    col = col.child(self.popup(card, cx));
                }
                rows.push(col.into_any_element());
            }
        }

        let messages = v_flex()
            .id("messages")
            .flex_1()
            .min_h_0()
            .w_full()
            .px_4()
            .pt_1()
            .pb_3()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .children(rows);

        let chat_id = self.selected_id();
        let mut attach = div()
            .id("attach")
            .cursor_pointer()
            .flex_none()
            .size(px(30.))
            .rounded_full()
            .bg(rgb(p.well))
            .hover(|d| d.bg(rgb(p.selected)))
            .relative()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _, cx| this.open_menu(Menu::Attach, ev.position, cx)),
            )
            .children(Self::plus_glyph(p.sub, 30.));
        if self.menu == Some(Menu::Attach) {
            let mut card = menu_card(&p).child(
                menu_item("att-file", "Photo or File…", p.text, accent).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.menu = None;
                        this.attach_file(cx);
                    },
                )),
            );
            if let Some(contact_id) = chat_id.filter(|id| *id >= 0) {
                card = card
                    .child(menu_sep(&p))
                    .child(menu_item("att-tao", "Send or Request TAO…", p.text, accent).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.menu = None;
                            this.show_chat_info = true;
                            this.show_settings = false;
                            cx.notify();
                        }),
                    ))
                    .child(menu_item("att-share", "Share My TAO Address", p.text, accent).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.menu = None;
                            this.tao(TaoCmd::Share { contact_id });
                            cx.notify();
                        }),
                    ));
            }
            attach = attach.child(self.popup(card, cx));
        }
        let field_border = if dark { 0x3A3A3C } else { 0xD1D1D6 };
        let composer = h_flex()
            .w_full()
            .flex_none()
            .items_center()
            .gap_2()
            .px_3()
            .pt_1()
            .pb_3()
            .child(attach)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h(px(34.))
                    .pl_1p5()
                    .pr(px(3.))
                    .items_center()
                    .rounded_full()
                    .border_1()
                    .border_color(rgb(field_border))
                    .child(div().flex_1().min_w_0().child(Input::new(&self.input_state).appearance(false)))
                    .child(
                        div()
                            .id("send")
                            .flex_none()
                            .size(px(26.))
                            .rounded_full()
                            .bg(rgb(accent))
                            .flex()
                            .items_center()
                            .justify_center()
                            // Dimmed until there's something to send.
                            .when(self.draft_empty, |d| d.opacity(0.35))
                            .when(!self.draft_empty, |d| {
                                d.cursor_pointer().hover(|d| d.opacity(0.85)).on_click(cx.listener(
                                    |this, _, window, cx| this.send_current(window, cx),
                                ))
                            })
                            .child(
                                Icon::new(gpui_kit::assets::IconName::ArrowUp)
                                    .with_size(px(16.))
                                    .text_color(rgb(0xFFFFFF)),
                            ),
                    ),
            );

        let mut main = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(rgb(p.bg))
            .child(header)
            .children(group_panel)
            .children(join_panel);
        main = if let Some(pane) = settings_panel.or(chat_info_panel) {
            main.child(
                div()
                    .id("pane")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .bg(rgb(p.surface))
                    .child(h_flex().w_full().justify_center().child(pane)),
            )
        } else if self.chats.is_empty() {
            main.child(if self.status == "loading…" {
                div().flex_1()
            } else {
                empty_state(&p, "No chats yet", "Create an invite, or join one with a link.").child(
                    h_flex()
                        .pt_3()
                        .gap_2()
                        .child(Button::new("empty-invite").primary().label("New Invite").on_click(
                            cx.listener(|this, _, _, cx| this.new_invite(cx)),
                        ))
                        .child(Button::new("empty-join").outline().label("Join with Link").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.show_join = true;
                                cx.notify();
                            }),
                        )),
                )
            })
        } else {
            let quiet = self.chats.get(self.selected).is_some_and(|c| c.msgs.is_empty());
            main.children(key_warning)
                .children(banner)
                .child(if quiet {
                    empty_state(&p, "No messages yet", "Everything here is end-to-end encrypted.")
                        .into_any_element()
                } else {
                    messages.into_any_element()
                })
                .child(composer)
        };

        let mut root = h_flex().size_full();
        if self.show_sidebar {
            root = root.child(self.sidebar(&p, accent, cx));
        }
        root.child(main)
    }
}

fn main() {
    let app = gpui_kit::application().with_assets(Assets);
    app.run(move |cx| {
        gpui_kit::init(cx);
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(880.), px(680.)), cx)),
            titlebar: Some(TitlebarOptions { title: Some("Sanctum".into()), ..Default::default() }),
            ..Default::default()
        };
        gpui_kit::open_window(window_options, cx, |window, cx| {
            cx.new(|cx| ChatApp::new(window, cx))
        })
        .expect("Failed to open window");
    });
}
