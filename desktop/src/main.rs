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
use sanctum_core::contact::{spawn_worker, Cmd, Event};
use sanctum_core::demo::{self, Settings};
use sanctum_core::store::StoredMsg;

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
    reaction: Option<SharedString>,
    text: SharedString,
    file: Option<(SharedString, String)>,
    sender_seat: Option<i64>,
    ts_ms: i64,
    time: SharedString,
    day: String,
}

impl Msg {
    fn from_stored(m: StoredMsg) -> Self {
        let ts = Local.timestamp_millis_opt(m.ts_ms).single().unwrap_or_else(Local::now);
        Self {
            wire_id: m.wire_id,
            mine: m.mine,
            delivered: m.delivered,
            reaction: m.reaction.map(Into::into),
            text: m.text.clone().into(),
            file: m.file_path.map(|p| (m.text.into(), p)),
            sender_seat: m.sender_seat,
            ts_ms: m.ts_ms,
            time: ts.format("%-I:%M %p").to_string().into(),
            day: ts.format("%A, %b %-d").to_string(),
        }
    }

    fn now(wire_id: String, mine: bool, text: String, file: Option<(String, String)>) -> Self {
        let now = Local::now();
        Self {
            wire_id,
            mine,
            delivered: false,
            reaction: None,
            text: text.into(),
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
    picker_for: Option<usize>,
    chat_menu_for: Option<usize>,
    copied_at: Option<std::time::Instant>,
    hovered_msg: Option<usize>,
    input_state: Entity<InputState>,
    rename_state: Entity<InputState>,
    join_state: Entity<InputState>,
    group_state: Entity<InputState>,
    timer_state: Entity<InputState>,
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
        let _subscriptions = vec![cx.subscribe_in(&input_state, window, {
            move |this: &mut Self, _, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    this.send_current(window, cx);
                }
            }
        })];

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
            picker_for: None,
            chat_menu_for: None,
            copied_at: None,
            hovered_msg: None,
            input_state,
            rename_state,
            join_state,
            group_state,
            timer_state,
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
                    self.chats = all
                        .into_iter()
                        .map(|(c, msgs)| Chat {
                            id: c.id,
                            name: c.name.into(),
                            waiting: c.peer_pk.is_empty(),
                            invite_link: c.invite_link,
                            msgs: msgs.into_iter().map(Msg::from_stored).collect(),
                            unread: 0,
                        })
                        .collect();
                    self.status = "ready".into();
                }
                Event::ContactAdded(c) => {
                    self.inviting = false;
                    if self.joining {
                        self.joining = false;
                        self.show_join = false;
                    }
                    self.chats.push(Chat {
                        id: c.id,
                        name: c.name.into(),
                        waiting: c.peer_pk.is_empty(),
                        invite_link: c.invite_link,
                        msgs: Vec::new(),
                        unread: 0,
                    });
                    self.selected = self.chats.len() - 1;
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
                            m.reaction = Some(emoji.into());
                        }
                    }
                }
                Event::TimerChanged { secs, .. } => {
                    self.settings.disappear_secs = secs;
                    self.settings.save();
                }
                Event::Unsent { contact_id, wire_id } => {
                    if let Some(c) = self.chat_by_id(contact_id) {
                        c.msgs.retain(|m| m.wire_id != wire_id);
                    }
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

    fn selected_id(&self) -> Option<i64> {
        self.chats.get(self.selected).map(|c| c.id)
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
        self.input_state.update(cx, |s, cx| s.set_value("", window, cx));
        let wire_id = ulid::Ulid::new().to_string();
        if let Some(c) = self.chats.get_mut(self.selected) {
            c.msgs.push(Msg::now(wire_id.clone(), true, text.clone(), None));
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
                    this.chat_menu_for = None;
                    if let Some(c) = this.chats.get_mut(i) {
                        c.unread = 0;
                    }
                    cx.notify();
                }))
                .child(c.name.clone())
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
        let peer_name = self
            .chats
            .get(self.selected)
            .map(|c| c.name.clone())
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
                    div()
                        .pt_1()
                        .text_xs()
                        .text_color(rgb(p.sub))
                        .child("End-to-end encrypted · mailboxes on Hippius (Bittensor SN75)"),
                )
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

        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(chat) = self.chats.get(self.selected) {
            let last_delivered = chat.msgs.iter().rposition(|m| m.mine && m.delivered);
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

                let content: AnyElement = if let Some((name, path)) = &m.file {
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
                let unsendable = m.mine && !m.delivered && chat.id >= 0;
                if Some(i) == last_delivered {
                    caption.push("Delivered".into());
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
                if !caption.is_empty() {
                    col = col
                        .child(div().text_xs().text_color(rgb(p.sub)).child(caption.join(" · ")));
                }
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
