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
}

fn pal(dark: bool) -> Pal {
    if dark {
        Pal {
            bg: 0x1C1C1E,
            surface: 0x242426,
            border: 0x3A3A3C,
            text: 0xF2F2F7,
            sub: 0x98989D,
            bubble: 0x3A3A3C,
            bubble_text: 0xF2F2F7,
            hover: 0x323234,
            selected: 0x3A3A3C,
            well: 0x323234,
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
    /// Own message the peer has read (read receipts on).
    read: bool,
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
            read: m.mine && m.read_ms.is_some(),
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
            read: false,
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
    show_plus_menu: bool,
    show_join: bool,
    show_group: bool,
    group_size: i64,
    inviting: bool,
    joining: bool,
    /// Armed by the first click on Emergency wipe.
    confirm_wipe: bool,
    picker_for: Option<usize>,
    /// Message whose delete actions are open (right-click a bubble).
    actions_for: Option<usize>,
    chat_menu_for: Option<usize>,
    copied_at: Option<std::time::Instant>,
    hovered_msg: Option<usize>,
    input_state: Entity<InputState>,
    rename_state: Entity<InputState>,
    join_state: Entity<InputState>,
    group_state: Entity<InputState>,
    timer_state: Entity<InputState>,
    /// TAO: amount for chat payments, phrase/address for adding accounts.
    tao_amount: Entity<InputState>,
    tao_key: Entity<InputState>,
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

        let settings = Settings::load();
        let mode = if settings.theme == "dark" { ThemeMode::Dark } else { ThemeMode::Light };
        Theme::change(mode, Some(window), cx);

        let input_state = cx.new(|cx| InputState::new(window, cx).placeholder("Sanctum"));
        let rename_state = cx.new(|cx| InputState::new(window, cx));
        let join_state = cx.new(|cx| InputState::new(window, cx).placeholder("sanctum://…"));
        let group_state = cx.new(|cx| InputState::new(window, cx).placeholder("Group name"));
        let timer_state = cx.new(|cx| InputState::new(window, cx).placeholder("90m, 2w, 6mo"));
        let tao_amount = cx.new(|cx| InputState::new(window, cx).placeholder("1.5"));
        let tao_key = cx.new(|cx| InputState::new(window, cx).placeholder("recovery phrase or address"));
        let mut _subscriptions = vec![cx.subscribe_in(&input_state, window, {
            move |this: &mut Self, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.send_current(window, cx);
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
            show_plus_menu: false,
            show_join: false,
            show_group: false,
            group_size: 3,
            inviting: false,
            joining: false,
            confirm_wipe: false,
            picker_for: None,
            actions_for: None,
            chat_menu_for: None,
            copied_at: None,
            hovered_msg: None,
            input_state,
            rename_state,
            join_state,
            group_state,
            timer_state,
            tao_amount,
            tao_key,
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
                    self.chat_menu_for = None;
                    self.chats.retain(|c| c.id != contact_id);
                    if self.selected >= self.chats.len() {
                        self.selected = self.chats.len().saturating_sub(1);
                    }
                    self.picker_for = None;
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
                    self.actions_for = None; // indices shifted
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
        self.chat_menu_for = None; // indices may have moved
    }

    fn mark(&self, contact_id: i64, mark: Mark, on: bool) {
        let _ = self.outbound.send(Cmd::Mark { contact_id, mark, on });
    }

    /// Emergency wipe, then start over as on first launch with a new worker.
    fn wipe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let result = contact::wipe();
        let (out_tx, out_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        spawn_worker(out_rx, ev_tx);
        self.outbound = out_tx;
        self.events = ev_rx;
        self.chats.clear();
        self.selected = 0;
        self.settings = Settings::load();
        Theme::change(ThemeMode::Light, Some(window), cx); // the default theme
        self.input_state.update(cx, |s, cx| s.set_value("", window, cx));
        self.confirm_wipe = false;
        self.show_settings = false;
        self.show_chat_info = false;
        (self.picker_for, self.actions_for, self.chat_menu_for) = (None, None, None);
        (self.inviting, self.joining) = (false, false);
        self.status = match result {
            Ok(()) => "wiped".into(),
            Err(e) => format!("error: wipe incomplete: {e}").into(),
        };
    }

    fn selected_id(&self) -> Option<i64> {
        self.chats.get(self.selected).map(|c| c.id)
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
        if let Some((addr, phrase)) = self.phrase.clone() {
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
                    let prompt = format!("Send {what} to their shared address on {}?", this.net_label());
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

    /// "Are you sure?" for anything that moves money.
    fn confirm_row(&mut self, prompt: String, cx: &mut Context<Self>) -> Div {
        h_flex()
            .gap_2()
            .items_center()
            .child(div().text_sm().text_color(rgb(0xFF9F0A)).child(prompt))
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
        let wire_id = ulid::Ulid::new().to_string();
        if let Some(c) = self.chats.get_mut(self.selected) {
            c.msgs.push(Msg::now(wire_id.clone(), true, text.clone(), None));
            self.scroll.scroll_to_bottom();
        }
        let _ = self.outbound.send(Cmd::Send { contact_id: sel, wire_id, text });
        cx.notify();
    }

    fn new_invite(&mut self, cx: &mut Context<Self>) {
        if self.inviting {
            return; // minting takes a few seconds; one at a time
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
        let mut col = v_flex()
            .w(px(172.))
            .h_full()
            .bg(rgb(p.surface))
            .border_r_1()
            .border_color(rgb(p.border))
            .px_2()
            .pb_2()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    // Same fixed height as the main header: centers align exactly.
                    .h(px(48.))
                    .items_center()
                    .justify_between()
                    .px_1()
                    .child(div().text_sm().font_semibold().text_color(rgb(p.text)).child("Chats"))
                    .child(
                        div()
                            .id("new-invite")
                            .cursor_pointer()
                            .w(px(24.))
                            .h(px(24.))
                            .rounded_md()
                            .hover(|d| d.bg(rgb(p.hover)))
                            .relative()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_plus_menu = !this.show_plus_menu;
                                cx.notify();
                            }))
                            .children(Self::plus_glyph(p.sub, 24.)),
                    ),
            );
        if self.show_plus_menu {
            let item = |label: &str, pal_text: u32, pal_hover: u32| {
                div()
                    .cursor_pointer()
                    .w_full()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .text_sm()
                    .text_color(rgb(pal_text))
                    .hover(move |d| d.bg(rgb(pal_hover)))
                    .child(label.to_string())
            };
            col = col.child(
                v_flex()
                    .w_full()
                    .mb_1()
                    .p_1()
                    .rounded_md()
                    .bg(rgb(p.bg))
                    .border_1()
                    .border_color(rgb(p.border))
                    .child(item("New invite", p.text, p.hover).id("pm-new").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.show_plus_menu = false;
                            this.new_invite(cx);
                        }),
                    ))
                    .child(item("New group…", p.text, p.hover).id("pm-group").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.show_plus_menu = false;
                            this.show_group = true;
                            cx.notify();
                        }),
                    ))
                    .child(item("Join with link…", p.text, p.hover).id("pm-join").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.show_plus_menu = false;
                            this.show_join = true;
                            cx.notify();
                        }),
                    )),
            );
        }
        if self.chats.is_empty() && !self.inviting {
            col = col
                .child(div().text_xs().text_color(rgb(p.sub)).child("Press + to invite someone"));
        }
        for (i, c) in self.chats.iter().enumerate() {
            let selected = i == self.selected;
            let trailing: Option<AnyElement> = if c.unread > 0 {
                Some(
                    div()
                        .px_1p5()
                        .rounded_full()
                        .text_xs()
                        .bg(rgb(accent))
                        .text_color(rgb(0xFFFFFF))
                        .child(format!("{}", c.unread))
                        .into_any_element(),
                )
            } else if c.waiting {
                Some(div().w(px(7.)).h(px(7.)).rounded_full().bg(rgb(p.sub)).into_any_element())
            } else {
                None
            };
            let dots = div()
                .id(("chat-menu-btn", i))
                .cursor_pointer()
                .w(px(20.))
                .h(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .text_color(rgb(p.sub))
                .hover(move |d| d.bg(rgb(chip_hover)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    // The row's own click handler would immediately close it.
                    cx.stop_propagation();
                    this.chat_menu_for =
                        if this.chat_menu_for == Some(i) { None } else { Some(i) };
                    cx.notify();
                }))
                .child("⋯");
            let row = h_flex()
                .id(("chat", i))
                .cursor_pointer()
                .w_full()
                .items_center()
                .justify_between()
                .px_2()
                .py_1p5()
                .rounded_md()
                .text_sm()
                .text_color(rgb(p.text))
                .when(selected, |d| d.bg(rgb(p.selected)))
                .when(!selected, |d| d.hover(|d| d.bg(rgb(p.hover))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected = i;
                    this.picker_for = None;
                    this.actions_for = None;
                    this.mark_read();
                    this.chat_menu_for = None;
                    if let Some(c) = this.chats.get_mut(i) {
                        c.unread = 0;
                    }
                    cx.notify();
                }))
                .child(format!(
                    "{}{}{}",
                    if c.favorite { "★ " } else { "" },
                    c.name,
                    match (c.key_changed, c.blocked) {
                        (true, _) => " ⚠",
                        (_, true) => " ⊘",
                        _ => "",
                    }
                ))
                .child(h_flex().gap_1().items_center().children(trailing).child(dots));
            col = col.child(row);

            if self.chat_menu_for == Some(i) {
                let id = c.id;
                let pending_link =
                    (!c.invite_link.trim().is_empty()).then(|| c.invite_link.clone());
                let item = |label: &str, danger: bool, pal_text: u32, pal_hover: u32| {
                    div()
                        .cursor_pointer()
                        .w_full()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .text_sm()
                        .text_color(rgb(if danger { 0xFF453A } else { pal_text }))
                        .hover(move |d| d.bg(rgb(pal_hover)))
                        .child(label.to_string())
                };
                let mut menu = v_flex()
                    .w_full()
                    .my_1()
                    .p_1()
                    .rounded_md()
                    .bg(rgb(p.bg))
                    .border_1()
                    .border_color(rgb(p.border))
                    .child(
                        item("Rename", false, p.text, p.hover).id(("m-rename", i)).on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.selected = i;
                                this.chat_menu_for = None;
                                this.show_chat_info = true;
                                this.show_settings = false;
                                let name = this
                                    .chats
                                    .get(i)
                                    .map(|c| c.name.to_string())
                                    .unwrap_or_default();
                                this.rename_state
                                    .update(cx, |s, cx| s.set_value(&name, window, cx));
                                cx.notify();
                            }),
                        ),
                    );
                if id >= 0 {
                    let (fav, blocked) = (c.favorite, c.blocked);
                    menu = menu
                        .child(
                            item(if fav { "Unfavorite" } else { "Favorite" }, false, p.text, p.hover)
                                .id(("m-fav", i))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.mark(id, Mark::Favorite, !fav);
                                    this.chat_menu_for = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            item(if blocked { "Unblock" } else { "Block" }, false, p.text, p.hover)
                                .id(("m-block", i))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.mark(id, Mark::Blocked, !blocked);
                                    this.chat_menu_for = None;
                                    cx.notify();
                                })),
                        );
                }
                if let Some(link) = pending_link {
                    let links: Vec<String> = link.split('\n').map(str::to_string).collect();
                    let many = links.len() > 1;
                    for (n, one) in links.into_iter().enumerate() {
                        if one.is_empty() {
                            continue;
                        }
                        let label = if many {
                            format!("Copy Member {} invite", n + 2)
                        } else {
                            "Copy invite link".to_string()
                        };
                        menu = menu.child(
                            div()
                                .cursor_pointer()
                                .w_full()
                                .px_2()
                                .py_1()
                                .rounded_sm()
                                .text_sm()
                                .text_color(rgb(p.text))
                                .hover(move |d| d.bg(rgb(p.hover)))
                                .id(("m-copy", i * 10 + n))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(one.clone()));
                                    this.chat_menu_for = None;
                                    cx.notify();
                                }))
                                .child(label),
                        );
                    }
                }
                menu = menu
                    .child(
                        item("Clear history", false, p.text, p.hover)
                            .id(("m-clear", i))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let _ = this.outbound.send(Cmd::ClearHistory {
                                    contact_id: id,
                                    older_than_secs: None,
                                });
                                this.chat_menu_for = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        item("Clear older than a week", false, p.text, p.hover)
                            .id(("m-clear-week", i))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let _ = this.outbound.send(Cmd::ClearHistory {
                                    contact_id: id,
                                    older_than_secs: Some(7 * 86400),
                                });
                                this.chat_menu_for = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        item("Delete chat", true, p.text, p.hover)
                            .id(("m-delete", i))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let _ =
                                    this.outbound.send(Cmd::DeleteContact { contact_id: id });
                                this.chat_menu_for = None;
                                cx.notify();
                            })),
                    );
                col = col.child(menu);
            }
        }
        let footer_settings = h_flex()
            .id("app-settings")
            .cursor_pointer()
            .w_full()
            .items_center()
            .gap_1p5()
            .px_2()
            .py_1p5()
            .rounded_md()
            .text_sm()
            .text_color(rgb(p.sub))
            .hover(|d| d.bg(rgb(p.hover)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_settings = !this.show_settings;
                this.show_chat_info = false;
                this.confirm_wipe = false;
                cx.notify();
            }))
            .child(
                Icon::new(gpui_kit::assets::IconName::Settings)
                    .with_size(px(14.))
                    .text_color(rgb(p.sub)),
            )
            .child("Settings");

        if self.inviting || self.joining {
            col = col.child(
                div()
                    .w_full()
                    .px_2()
                    .py_1p5()
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
        col.child(div().flex_1()).child(footer_settings)
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

        let header = h_flex()
            .w_full()
            .h(px(48.))
            .items_center()
            .px_3()
            .border_b_1()
            .border_color(rgb(p.border))
            .child(toggle)
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .child(
                        div().text_sm().font_semibold().text_color(rgb(p.text)).child(peer_name),
                    )
                    .children(error_line.map(|e| {
                        div().text_xs().text_color(rgb(0xFF453A)).child(e)
                    })),
            )
            .child(
                Button::new("chat-info").ghost().label("⋯").on_click(cx.listener(
                    |this, _, window, cx| {
                        this.show_chat_info = !this.show_chat_info;
                        this.show_settings = false;
                        if this.show_chat_info {
                            let name = this
                                .chats
                                .get(this.selected)
                                .map(|c| c.name.to_string())
                                .unwrap_or_default();
                            this.rename_state.update(cx, |s, cx| s.set_value(&name, window, cx));
                        }
                        cx.notify();
                    },
                )),
            );

        let settings_panel = self.show_settings.then(|| {
            let row = |label: &str, hint: Option<&str>| {
                let mut l = v_flex()
                    .flex_1()
                    .child(div().text_sm().text_color(rgb(p.text)).child(label.to_string()));
                if let Some(h) = hint {
                    l = l.child(div().text_xs().text_color(rgb(p.sub)).child(h.to_string()));
                }
                h_flex().w_full().items_center().gap_3().child(l)
            };

            // Accent swatches.
            let mut swatches = h_flex().gap_2();
            for (name, color) in ACCENTS {
                let active = accent == color;
                swatches = swatches.child(
                    div()
                        .id(name)
                        .cursor_pointer()
                        .w(px(20.))
                        .h(px(20.))
                        .rounded_full()
                        .bg(rgb(color))
                        .when(active, |d| d.border_2().border_color(rgb(p.text)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.settings.accent = color;
                            this.settings.save();
                            cx.notify();
                        })),
                );
            }

            v_flex()
                .w_full()
                .gap_3()
                .p_3()
                .bg(rgb(p.surface))
                .border_b_1()
                .border_color(rgb(p.border))
                .child(
                    row("Appearance", None).child(
                        Button::new("set-theme")
                            .outline()
                            .label(if dark { "Dark" } else { "Light" })
                            .on_click(cx.listener(|this, _, window, cx| {
                                let dark = this.settings.theme != "dark";
                                this.settings.theme =
                                    if dark { "dark".into() } else { "light".into() };
                                this.settings.save();
                                Theme::change(
                                    if dark { ThemeMode::Dark } else { ThemeMode::Light },
                                    Some(window),
                                    cx,
                                );
                                cx.notify();
                            })),
                    ),
                )
                .child(row("Accent", None).child(swatches))
                .child(
                    row("Timestamps", None).child(
                        Switch::new("set-ts").checked(self.settings.timestamps).on_click(
                            cx.listener(|this, v: &bool, _, cx| {
                                this.settings.timestamps = *v;
                                this.settings.save();
                                cx.notify();
                            }),
                        ),
                    ),
                )
                .child(
                    row("Read receipts", Some("send yours and see theirs · off hides both")).child(
                        Switch::new("set-read").checked(self.settings.read_receipts).on_click(
                            cx.listener(|this, v: &bool, _, cx| {
                                this.settings.read_receipts = *v;
                                this.settings.save();
                                cx.notify();
                            }),
                        ),
                    ),
                )
                .child(
                    row(
                        "Route via proxy (Tor / UR)",
                        Some(&format!("{} · applies on restart", self.settings.proxy_url)),
                    )
                    .child(
                        Switch::new("set-proxy").checked(self.settings.proxy_enabled).on_click(
                            cx.listener(|this, v: &bool, _, cx| {
                                this.settings.proxy_enabled = *v;
                                this.settings.save();
                                cx.notify();
                            }),
                        ),
                    ),
                )
                .child(
                    row(
                        "Emergency wipe",
                        Some("erase every chat, key and setting on this device · offline, instant"),
                    )
                    .child(
                        div()
                            .id("wipe")
                            .cursor_pointer()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(0xFF453A))
                            .text_sm()
                            .text_color(rgb(0xFF453A))
                            .when(self.confirm_wipe, |d| d.bg(rgb(0xFF453A)).text_color(rgb(0xFFFFFF)))
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.confirm_wipe {
                                    this.wipe(window, cx);
                                } else {
                                    this.confirm_wipe = true;
                                }
                                cx.notify();
                            }))
                            .child(if self.confirm_wipe { "Click again to erase everything" } else { "Wipe…" }),
                    ),
                )
                .child(
                    div()
                        .pt_1()
                        .text_xs()
                        .text_color(rgb(p.sub))
                        .child("End-to-end encrypted · mailboxes on Hippius (Bittensor SN75)"),
                )
                .child(self.wallet_section(&p, cx))
        });

        // Per-chat details behind the ⋯ button: name, timer, delete.
        let chat_info_panel = (self.show_chat_info && self.selected_id().is_some()).then(|| {
            let row = |label: &str, hint: Option<&str>| {
                let mut l = v_flex()
                    .flex_1()
                    .child(div().text_sm().text_color(rgb(p.text)).child(label.to_string()));
                if let Some(h) = hint {
                    l = l.child(div().text_xs().text_color(rgb(p.sub)).child(h.to_string()));
                }
                h_flex().w_full().items_center().gap_3().child(l)
            };
            v_flex()
                .w_full()
                .gap_3()
                .p_3()
                .bg(rgb(p.surface))
                .border_b_1()
                .border_color(rgb(p.border))
                .children(self.selected_id().filter(|id| *id >= 0).map(|id| self.tao_chat_row(id, &p, cx)))
                .child(
                    row("Disappearing messages", Some("timed local deletion, synced to contact"))
                        .child(
                            Button::new("set-disappear")
                                .outline()
                                .label(demo::disappear_label(self.settings.disappear_secs))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.set_timer(demo::next_disappear_preset(
                                        this.settings.disappear_secs,
                                    ));
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    row("Custom timer", Some("1 minute to 1 year"))
                        .child(div().w(px(160.)).child(Input::new(&self.timer_state)))
                        .child(
                            div()
                                .id("timer-set")
                                .cursor_pointer()
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(p.border))
                                .hover(|d| d.bg(rgb(p.hover)))
                                .text_sm()
                                .text_color(rgb(p.text))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    let text = this.timer_state.read(cx).value().to_string();
                                    match demo::parse_disappear(&text) {
                                        Some(secs) => {
                                            this.set_timer(secs);
                                            this.timer_state
                                                .update(cx, |s, cx| s.set_value("", window, cx));
                                            window.blur(cx);
                                        }
                                        None => {
                                            this.status =
                                                "error: timer must be off or 1 minute to 1 year, e.g. 90m, 2w, 6mo"
                                                    .into();
                                        }
                                    }
                                    cx.notify();
                                }))
                                .child("Set"),
                        ),
                )
                .children(self.selected_id().map(|id| {
                    row("Chat name", None)
                        .child(div().w(px(160.)).child(Input::new(&self.rename_state)))
                        .child(
                            div()
                                .id("rename-save")
                                .cursor_pointer()
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(rgb(p.border))
                                .hover(|d| d.bg(rgb(p.hover)))
                                .text_sm()
                                .text_color(rgb(p.text))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let name = this.rename_state.read(cx).value().trim().to_string();
                                    if !name.is_empty() {
                                        let _ = this
                                            .outbound
                                            .send(Cmd::Rename { contact_id: id, name: name.clone() });
                                        if let Some(c) =
                                            this.chats.iter_mut().find(|c| c.id == id)
                                        {
                                            c.name = name.into();
                                        }
                                        // Leave the field and close the panel —
                                        // the renamed sidebar row is the confirmation.
                                        window.blur(cx);
                                        this.show_chat_info = false;
                                        cx.notify();
                                    }
                                }))
                                .child("Save"),
                        )
                }))
                .children(self.chats.get(self.selected).filter(|c| c.id >= 0).map(|c| {
                    let (id, verified, blocked) = (c.id, c.verified, c.blocked);
                    let hint = if c.safety.is_empty() {
                        "appears once they join"
                    } else {
                        "same digits on both screens? then it's really them"
                    };
                    v_flex()
                        .w_full()
                        .gap_3()
                        .child(
                            row("Safety number", Some(hint)).when(!c.safety.is_empty(), |r| {
                                r.child(
                                    Button::new("verify")
                                        .outline()
                                        .label(if verified { "Verified ✓" } else { "Mark verified" })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.mark(id, Mark::Verified, !verified);
                                            cx.notify();
                                        })),
                                )
                            }),
                        )
                        .when(!c.safety.is_empty(), |d| {
                            d.child(
                                div()
                                    .font_family("Menlo")
                                    .text_sm()
                                    .text_color(rgb(p.text))
                                    .child(c.safety.clone()),
                            )
                        })
                        .child(
                            row("Block", Some("drop their messages unseen · kept on this device")).child(
                                Switch::new("set-block").checked(blocked).on_click(cx.listener(
                                    move |this, v: &bool, _, cx| {
                                        this.mark(id, Mark::Blocked, *v);
                                        cx.notify();
                                    },
                                )),
                            ),
                        )
                }))
                .children(self.selected_id().map(|id| {
                    row("Delete this chat", Some("retires its mailboxes; removes history")).child(
                        div()
                            .id("delete-chat")
                            .cursor_pointer()
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(0xFF453A))
                            .text_sm()
                            .text_color(rgb(0xFF453A))
                            .hover(|d| d.bg(rgb(if dark { 0x3A2A2A } else { 0xFFF0F0 })))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let _ = this.outbound.send(Cmd::DeleteContact { contact_id: id });
                                this.show_settings = false;
                                cx.notify();
                            }))
                            .child("Delete"),
                    )
                }))
                .child(
                    div()
                        .pt_1()
                        .text_xs()
                        .text_color(rgb(p.sub))
                        .child("End-to-end encrypted · mailboxes on Hippius (Bittensor SN75)"),
                )
        });

        let group_panel = self.show_group.then(|| {
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .px_3()
                .py_3()
                .bg(rgb(p.surface))
                .border_b_1()
                .border_color(rgb(p.border))
                .child(div().text_sm().text_color(rgb(p.text)).child("Group"))
                .child(div().flex_1().child(Input::new(&self.group_state)))
                .child(
                    div()
                        .id("group-size")
                        .cursor_pointer()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(rgb(p.border))
                        .text_sm()
                        .text_color(rgb(p.text))
                        .hover(|d| d.bg(rgb(p.hover)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.group_size =
                                if this.group_size >= 6 { 2 } else { this.group_size + 1 };
                            cx.notify();
                        }))
                        .child(format!("{} members", self.group_size)),
                )
                .child(
                    div()
                        .id("group-go")
                        .cursor_pointer()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(accent))
                        .text_sm()
                        .text_color(rgb(0xFFFFFF))
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.inviting {
                                return;
                            }
                            let name =
                                this.group_state.read(cx).value().trim().to_string();
                            let name =
                                if name.is_empty() { "Group".to_string() } else { name };
                            this.inviting = true;
                            let _ = this
                                .outbound
                                .send(Cmd::NewGroup { name, size: this.group_size });
                            this.group_state.update(cx, |s, cx| s.set_value("", window, cx));
                            window.blur(cx);
                            this.show_group = false;
                            cx.notify();
                        }))
                        .child("Create"),
                )
                .child(
                    div()
                        .id("group-cancel")
                        .cursor_pointer()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_sm()
                        .text_color(rgb(p.sub))
                        .hover(|d| d.bg(rgb(p.hover)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_group = false;
                            cx.notify();
                        }))
                        .child("Cancel"),
                )
        });

        let join_panel = self.show_join.then(|| {
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .px_3()
                .py_3()
                .bg(rgb(p.surface))
                .border_b_1()
                .border_color(rgb(p.border))
                .child(div().text_sm().text_color(rgb(p.text)).child("Join"))
                .child(div().flex_1().child(Input::new(&self.join_state)))
                .child(
                    div()
                        .id("join-go")
                        .cursor_pointer()
                        .px_3()
                        .py_1()
                        .rounded_md()
                        .bg(rgb(accent))
                        .text_sm()
                        .text_color(rgb(0xFFFFFF))
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
                        }))
                        .child(if self.joining { "Joining…" } else { "Join" }),
                )
                .child(
                    div()
                        .id("join-cancel")
                        .cursor_pointer()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .text_sm()
                        .text_color(rgb(p.sub))
                        .hover(|d| d.bg(rgb(p.hover)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_join = false;
                            cx.notify();
                        }))
                        .child("Cancel"),
                )
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
            let mut card = v_flex()
                .w_full()
                .gap_2()
                .px_3()
                .py_3()
                .bg(rgb(p.surface))
                .border_b_1()
                .border_color(rgb(p.border))
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(rgb(p.text))
                        .child("Invitation pending"),
                )
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
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(p.sub))
                                    .child(format!("Member {}", n + 2)),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(p.well))
                                .text_xs()
                                .text_color(rgb(p.sub))
                                .overflow_hidden()
                                .child(short),
                        )
                        .child({
                            let btn = div()
                                .id(("copy-invite", n))
                                .cursor_pointer()
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .text_sm()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        one.clone(),
                                    ));
                                    this.copied_at = Some(std::time::Instant::now());
                                    cx.notify();
                                }));
                            if copied {
                                btn.border_1()
                                    .border_color(rgb(accent))
                                    .text_color(rgb(accent))
                                    .child("Copied")
                                    .into_any_element()
                            } else {
                                btn.bg(rgb(p.bg))
                                    .border_1()
                                    .border_color(rgb(p.border))
                                    .hover(|d| d.bg(rgb(p.hover)))
                                    .text_color(rgb(p.text))
                                    .child("Copy")
                                    .into_any_element()
                            }
                        }),
                );
            }
            card
        });

        let key_warning = self.chats.get(self.selected).filter(|c| c.key_changed).map(|_| {
            v_flex()
                .w_full()
                .gap_1()
                .px_3()
                .py_2()
                .bg(rgb(if dark { 0x3A2A2A } else { 0xFFF0F0 }))
                .border_b_1()
                .border_color(rgb(0xFF453A))
                .child(div().text_sm().font_semibold().text_color(rgb(0xFF453A)).child("Identity key changed"))
                .child(div().text_xs().text_color(rgb(p.text)).child(
                    "Someone wrote to this chat under a different identity key. It was refused. \
                     Compare safety numbers in person (⋯ → Safety number), then mark verified.",
                ))
        });

        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(chat) = self.chats.get(self.selected) {
            // Only the newest finished send carries Sent / Delivered.
            let last_sent = chat.msgs.iter().rposition(|m| m.mine && m.sent && !m.failed);
            let chat_id = chat.id;
            for (i, m) in chat.msgs.iter().enumerate() {
                let prev = i.checked_sub(1).and_then(|j| chat.msgs.get(j));
                let new_day = prev.is_none_or(|pm| pm.day != m.day);
                let gap = prev.is_none_or(|pm| m.ts_ms - pm.ts_ms > 5 * 60 * 1000);
                if new_day || (show_time && gap) {
                    let label = if new_day {
                        format!("{} · {}", m.day, m.time)
                    } else {
                        m.time.to_string()
                    };
                    rows.push(
                        div()
                            .w_full()
                            .text_center()
                            .text_xs()
                            .text_color(rgb(p.sub))
                            .pt_2()
                            .pb_1()
                            .child(label)
                            .into_any_element(),
                    );
                }

                let bubble =
                    div().max_w(relative(0.72)).px_3().py_1p5().text_sm().rounded(px(18.));
                let bubble = if m.mine {
                    bubble.bg(rgb(accent)).text_color(rgb(0xFFFFFF))
                } else {
                    bubble.bg(rgb(p.bubble)).text_color(rgb(p.bubble_text))
                };

                let content: AnyElement = if let Some(card) = &m.tao {
                    let mut c = v_flex()
                        .gap_1()
                        .child(div().text_xs().child(match (card.kind, m.mine) {
                            (TaoKind::Request, true) => "τ YOU REQUESTED",
                            (TaoKind::Request, false) => "τ REQUEST",
                            (TaoKind::Cheque, _) => "τ CHEQUE",
                            (TaoKind::Paid, true) => "τ YOU PAID",
                            (TaoKind::Paid, false) => "τ PAYMENT",
                        }))
                        .child(div().text_xl().font_semibold().child(tao::format_tao(card.amount())))
                        .child(div().text_xs().child(m.text.clone()));
                    let wid = m.wire_id.clone();
                    if !m.mine && card.kind == TaoKind::Request && card.status == PayStatus::Open {
                        let (to, rao) = (card.to.clone(), card.amount());
                        let what = tao::format_tao(rao);
                        c = c.child(Button::new(("tao-pay", i)).primary().label(format!("Pay {what}")).on_click(
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
                        ));
                    } else if !m.mine
                        && card.kind == TaoKind::Cheque
                        && matches!(card.status, PayStatus::Pending | PayStatus::Bounced)
                    {
                        c = c.child(Button::new(("tao-cash", i)).primary().label("Cash cheque").on_click(
                            cx.listener(move |this, _, _, _| {
                                this.tao(TaoCmd::Cash { contact_id: chat_id, wire_id: wid.clone() })
                            }),
                        ));
                    }
                    c.into_any_element()
                } else if let Some((name, path)) = &m.file {
                    if is_image(name) {
                        img(std::path::PathBuf::from(path))
                            .max_w(px(240.))
                            .rounded(px(12.))
                            .into_any_element()
                    } else {
                        div().font_semibold().child(name.to_string()).into_any_element()
                    }
                } else {
                    div().child(m.text.clone()).into_any_element()
                };

                let mut caption: Vec<String> = Vec::new();
                if self.hovered_msg == Some(i) {
                    caption.push(m.time.to_string());
                }
                let unsendable = m.mine && !m.delivered && !m.failed && chat.id >= 0;
                if Some(i) == last_sent {
                    caption.push(if m.read && self.settings.read_receipts {
                        "Read".into()
                    } else if m.delivered {
                        "Delivered".into()
                    } else {
                        "Sent".into()
                    });
                } else if unsendable {
                    caption.push("click bubble to unsend".into());
                }

                let contact_id = chat.id;
                let reactable = !m.mine && !m.wire_id.is_empty();
                let bubble: AnyElement = if unsendable {
                    let w = m.wire_id.clone();
                    bubble
                        .id(("msg", i))
                        .on_click(cx.listener(move |this, _, _, _| {
                            let _ =
                                this.outbound.send(Cmd::Unsend { contact_id, wire_id: w.clone() });
                        }))
                        .child(content)
                        .into_any_element()
                } else if reactable {
                    bubble
                        .id(("msg", i))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.picker_for =
                                if this.picker_for == Some(i) { None } else { Some(i) };
                            cx.notify();
                        }))
                        .child(content)
                        .into_any_element()
                } else {
                    bubble.child(content).into_any_element()
                };

                let picker = (self.picker_for == Some(i)).then(|| {
                    let mut pill = h_flex()
                        .gap_2()
                        .px_3()
                        .py_1()
                        .rounded_full()
                        .bg(rgb(p.well))
                        .border_1()
                        .border_color(rgb(p.border));
                    for (n, emoji) in REACTIONS.iter().enumerate() {
                        let w = m.wire_id.clone();
                        pill = pill.child(
                            div()
                                .id(("react", i * 10 + n))
                                .cursor_pointer()
                                .text_lg()
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
                                    this.picker_for = None;
                                    cx.notify();
                                }))
                                .child(*emoji),
                        );
                    }
                    pill
                });

                let mut row = h_flex()
                    .w_full()
                    .items_end()
                    .gap_1()
                    .when(m.mine, |d| d.justify_end())
                    .when(!m.mine, |d| d.justify_start());
                row = if m.mine {
                    row.child(div().when_some(m.reaction.clone(), |d, r| d.child(r)).text_sm())
                        .child(bubble)
                } else {
                    row.child(bubble)
                        .child(div().when_some(m.reaction.clone(), |d, r| d.child(r)).text_sm())
                };

                let same_sender = prev.is_some_and(|pm| {
                    !pm.mine && pm.sender_seat == m.sender_seat && !new_day && !gap
                });
                let sender_label = (!m.mine && !same_sender)
                    .then(|| m.sender_seat)
                    .flatten()
                    .map(|seat| {
                        div()
                            .text_xs()
                            .text_color(rgb(p.sub))
                            .child(format!("Member {}", seat + 1))
                    });
                let mut col = v_flex()
                    .w_full()
                    .when(m.mine, |d| d.items_end())
                    .when(!m.mine, |d| d.items_start())
                    .children(sender_label)
                    .children(picker)
                    .child(row);
                for (n, l) in m.pay.iter().enumerate() {
                    let icon = match l.kind {
                        PayKind::Cashu => "🥜",
                        PayKind::Lightning => "⚡",
                        PayKind::Bitcoin => "₿",
                    };
                    let (uri, raw) = (l.uri.clone(), l.raw.clone());
                    let action = |id: &'static str, label: &'static str| {
                        div()
                            .id((id, i * 10 + n))
                            .cursor_pointer()
                            .text_xs()
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
                            .py_1()
                            .rounded(px(12.))
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
                if !caption.is_empty() {
                    col = col
                        .child(div().text_xs().text_color(rgb(p.sub)).child(caption.join(" · ")));
                }
                if self.actions_for == Some(i) {
                    let link = |id: &'static str, label: &'static str, everyone: bool| {
                        let w = m.wire_id.clone();
                        div()
                            .id((id, i))
                            .cursor_pointer()
                            .text_xs()
                            .text_color(rgb(0xFF453A))
                            .hover(|d| d.underline())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let _ = this.outbound.send(Cmd::DeleteMsg {
                                    contact_id,
                                    wire_id: w.clone(),
                                    everyone,
                                });
                                this.actions_for = None;
                                cx.notify();
                            }))
                            .child(label)
                    };
                    // Undelivered own messages already have unsend.
                    let for_all = m.mine && m.delivered && contact_id >= 0;
                    col = col.child(
                        h_flex()
                            .gap_3()
                            .child(link("del-me", "Delete for me", false))
                            .when(for_all, |d| {
                                d.child(link("del-all", "Delete for everyone", true))
                            }),
                    );
                }
                // Each send shows its own upload bar (easing toward 90%) until
                // it lands; one that never left gets a red mark instead.
                if m.failed {
                    col = col.child(div().text_xs().text_color(rgb(0xFF453A)).child("! Not sent"));
                } else if m.mine && !m.sent {
                    col = col.child(div().w(px(64.)).h(px(2.)).child(
                        div().h_full().bg(rgb(accent)).with_animation(
                            ("send-bar", i),
                            Animation::new(Duration::from_secs(8)),
                            |d, t| d.w(relative(0.9 * (1.0 - (1.0 - t).powi(3)))),
                        ),
                    ));
                }
                let col = col.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, _, cx| {
                        this.actions_for = if this.actions_for == Some(i) { None } else { Some(i) };
                        cx.notify();
                    }),
                );
                rows.push(col.into_any_element());
            }
        }

        let messages = v_flex()
            .id("messages")
            .flex_1()
            .w_full()
            .gap_1()
            .p_3()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .children(rows);

        let composer = h_flex()
            .w_full()
            .gap_2()
            .p_3()
            .border_t_1()
            .border_color(rgb(p.border))
            .child(
                div()
                    .id("attach")
                    .cursor_pointer()
                    .w(px(28.))
                    .h(px(28.))
                    .rounded_full()
                    .bg(rgb(p.bubble))
                    .hover(|d| d.bg(rgb(p.hover)))
                    .relative()
                    .on_click(cx.listener(|this, _, _, cx| this.attach_file(cx)))
                    .children(Self::plus_glyph(p.sub, 28.)),
            )
            .child(div().flex_1().child(Input::new(&self.input_state)))
            .child(
                div()
                    .id("send")
                    .cursor_pointer()
                    .w(px(44.))
                    .h(px(30.))
                    .rounded_full()
                    .bg(rgb(accent))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(0xFFFFFF))
                    .on_click(cx.listener(|this, _, window, cx| this.send_current(window, cx)))
                    .child(
                        Icon::new(gpui_kit::assets::IconName::ArrowUp)
                            .with_size(px(18.))
                            .text_color(rgb(0xFFFFFF)),
                    ),
            );

        let main = v_flex()
            .flex_1()
            .h_full()
            .bg(rgb(p.bg))
            .child(header)
            .children(settings_panel)
            .children(chat_info_panel)
            .children(group_panel)
            .children(join_panel)
            .children(key_warning)
            .children(banner)
            .child(messages)
            .child(composer);

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
            window_bounds: Some(WindowBounds::centered(size(px(620.), px(680.)), cx)),
            titlebar: Some(TitlebarOptions { title: Some("Sanctum".into()), ..Default::default() }),
            ..Default::default()
        };
        gpui_kit::open_window(window_options, cx, |window, cx| {
            cx.new(|cx| ChatApp::new(window, cx))
        })
        .expect("Failed to open window");
    });
}
