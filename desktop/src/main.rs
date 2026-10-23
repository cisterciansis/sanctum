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
    *,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use sanctum_core::contact::{spawn_worker, Cmd, Event};
use sanctum_core::demo::Settings;
use sanctum_core::store::StoredMsg;

const REACTIONS: [&str; 6] = ["👍", "👎", "❤️", "‼️", "❓", "😂"];
const BLUE: u32 = 0x0A84FF;
const GRAY_TXT: u32 = 0x8E8E93;

struct Msg {
    wire_id: String,
    mine: bool,
    delivered: bool,
    reaction: Option<SharedString>,
    text: SharedString,
    file: Option<(SharedString, String)>,
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
    picker_for: Option<usize>,
    input_state: Entity<InputState>,
    outbound: std::sync::mpsc::Sender<Cmd>,
    events: std::sync::mpsc::Receiver<Event>,
    _subscriptions: Vec<Subscription>,
}

impl ChatApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (out_tx, out_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        spawn_worker(out_rx, ev_tx);

        let input_state = cx.new(|cx| InputState::new(window, cx));
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
            settings: Settings::load(),
            show_settings: false,
            picker_for: None,
            input_state,
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
                    self.status = "end-to-end encrypted · hippius".into();
                }
                Event::ContactAdded(c) => {
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
                Event::Error(e) => self.status = format!("error: {e}").into(),
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn selected_id(&self) -> Option<i64> {
        self.chats.get(self.selected).map(|c| c.id)
    }

    fn send_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input_state.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        if text.starts_with("sanctum://v2/") {
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

    fn sidebar(&mut self, cx: &mut Context<Self>) -> Div {
        let mut col = v_flex()
            .w(px(150.))
            .h_full()
            .bg(rgb(0xF5F5F7))
            .border_r_1()
            .border_color(rgb(0xDDDDDD))
            .p_2()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(div().text_sm().font_bold().child("Sanctum"))
                    .child(
                        Button::new("new-invite").ghost().label("+").on_click(cx.listener(
                            |this, _, _, _| {
                                let n = this.chats.len();
                                let _ = this
                                    .outbound
                                    .send(Cmd::NewInvite { name: format!("Contact {}", n + 1) });
                            },
                        )),
                    ),
            );
        if self.chats.is_empty() {
            col = col
                .child(div().text_xs().text_color(rgb(GRAY_TXT)).child("Press + to invite someone"));
        }
        for (i, c) in self.chats.iter().enumerate() {
            let selected = i == self.selected;
            let label: String = if c.waiting {
                format!("{} ⏳", c.name)
            } else if c.unread > 0 {
                format!("{} ({})", c.name, c.unread)
            } else {
                c.name.to_string()
            };
            let row = div()
                .id(("chat", i))
                .cursor_pointer()
                .w_full()
                .px_2()
                .py_1()
                .rounded_md()
                .text_sm()
                .when(selected, |d| d.bg(rgb(BLUE)).text_color(rgb(0xFFFFFF)))
                .when(!selected, |d| d.hover(|d| d.bg(rgb(0xE8E8ED))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected = i;
                    this.picker_for = None;
                    if let Some(c) = this.chats.get_mut(i) {
                        c.unread = 0;
                    }
                    cx.notify();
                }))
                .child(label);
            col = col.child(row);
        }
        col
    }
}

impl Render for ChatApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show_time = self.settings.timestamps;
        let peer_name = self
            .chats
            .get(self.selected)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "Sanctum".into());

        let header = h_flex()
            .w_full()
            .items_center()
            .pt_3()
            .pb_2()
            .px_3()
            .border_b_1()
            .border_color(rgb(0xDDDDDD))
            .child(div().w_8())
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .child(div().text_sm().font_semibold().child(peer_name))
                    .child(div().text_xs().text_color(rgb(GRAY_TXT)).child(self.status.clone())),
            )
            .child(
                Button::new("settings-menu").ghost().label("⋯").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.show_settings = !this.show_settings;
                        cx.notify();
                    },
                )),
            );

        let settings_panel = self.show_settings.then(|| {
            let row = |label: &str, hint: Option<&str>| {
                let mut l = v_flex().flex_1().child(div().text_sm().child(label.to_string()));
                if let Some(h) = hint {
                    l = l.child(div().text_xs().text_color(rgb(GRAY_TXT)).child(h.to_string()));
                }
                h_flex().w_full().items_center().gap_3().child(l)
            };
            v_flex()
                .w_full()
                .gap_3()
                .p_3()
                .bg(rgb(0xF5F5F7))
                .border_b_1()
                .border_color(rgb(0xDDDDDD))
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
                    row("Disappearing messages", Some("timed local deletion, synced to contact"))
                        .child(
                            Button::new("set-disappear")
                                .outline()
                                .label(match self.settings.disappear_secs {
                                    0 => "Off",
                                    60 => "1 min",
                                    3600 => "1 hour",
                                    _ => "1 day",
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.settings.disappear_secs =
                                        match this.settings.disappear_secs {
                                            0 => 60,
                                            60 => 3600,
                                            3600 => 86400,
                                            _ => 0,
                                        };
                                    this.settings.save();
                                    if let Some(id) = this.selected_id() {
                                        let _ = this.outbound.send(Cmd::SetTimer {
                                            contact_id: id,
                                            secs: this.settings.disappear_secs,
                                        });
                                    }
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    row("Typing indicators", Some("coming soon · off keeps traffic quiet"))
                        .child(Switch::new("set-typing").checked(false).disabled(true)),
                )
        });

        // Invite banner for a pending contact.
        let banner = self
            .chats
            .get(self.selected)
            .filter(|c| c.waiting && !c.invite_link.is_empty())
            .map(|c| {
                let link = c.invite_link.clone();
                v_flex()
                    .w_full()
                    .gap_2()
                    .p_3()
                    .bg(rgb(0xFFF8E1))
                    .border_b_1()
                    .border_color(rgb(0xEEE0A8))
                    .child(
                        div()
                            .text_sm()
                            .child("Waiting for them to join — send this invite link:"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(GRAY_TXT))
                            .child(format!("{}…", &link[..link.len().min(56)])),
                    )
                    .child(
                        Button::new("copy-invite").outline().label("Copy invite link").on_click(
                            cx.listener(move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
                            }),
                        ),
                    )
            });

        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(chat) = self.chats.get(self.selected) {
            let last_delivered = chat.msgs.iter().rposition(|m| m.mine && m.delivered);
            let mut prev_day: Option<&str> = None;
            for (i, m) in chat.msgs.iter().enumerate() {
                if prev_day != Some(m.day.as_str()) {
                    prev_day = Some(m.day.as_str());
                    rows.push(
                        div()
                            .w_full()
                            .text_center()
                            .text_xs()
                            .text_color(rgb(GRAY_TXT))
                            .py_1()
                            .child(m.day.clone())
                            .into_any_element(),
                    );
                }

                let bubble =
                    div().max_w(relative(0.72)).px_3().py_1p5().text_sm().rounded(px(18.));
                let bubble = if m.mine {
                    bubble.bg(rgb(BLUE)).text_color(rgb(0xFFFFFF))
                } else {
                    bubble.bg(rgb(0xE9E9EB)).text_color(rgb(0x000000))
                };

                let content: AnyElement = if let Some((name, path)) = &m.file {
                    if is_image(name) {
                        img(std::path::PathBuf::from(path))
                            .max_w(px(240.))
                            .rounded(px(12.))
                            .into_any_element()
                    } else {
                        div().child(format!("📎 {name}")).into_any_element()
                    }
                } else {
                    div().child(m.text.clone()).into_any_element()
                };

                let mut caption: Vec<String> = Vec::new();
                if show_time {
                    caption.push(m.time.to_string());
                }
                let unsendable = m.mine && !m.delivered;
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
                        .bg(rgb(0xF2F2F7))
                        .border_1()
                        .border_color(rgb(0xDDDDDD));
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

                let mut row = h_flex().items_end().gap_1();
                row = if m.mine {
                    row.child(div().when_some(m.reaction.clone(), |d, r| d.child(r)).text_sm())
                        .child(bubble)
                } else {
                    row.child(bubble)
                        .child(div().when_some(m.reaction.clone(), |d, r| d.child(r)).text_sm())
                };

                let mut col = v_flex()
                    .w_full()
                    .when(m.mine, |d| d.items_end())
                    .when(!m.mine, |d| d.items_start())
                    .children(picker)
                    .child(row);
                if !caption.is_empty() {
                    col = col.child(
                        div().text_xs().text_color(rgb(GRAY_TXT)).child(caption.join(" · ")),
                    );
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
            .border_color(rgb(0xDDDDDD))
            .child(
                // Drawn glyph, not an icon asset: can't fail to render.
                div()
                    .id("attach")
                    .cursor_pointer()
                    .w(px(28.))
                    .h(px(28.))
                    .rounded_full()
                    .bg(rgb(0xE9E9EB))
                    .hover(|d| d.bg(rgb(0xDCDCE0)))
                    .relative()
                    .on_click(cx.listener(|this, _, _, cx| this.attach_file(cx)))
                    .child(
                        div()
                            .absolute()
                            .top(px(13.))
                            .left(px(8.))
                            .w(px(12.))
                            .h(px(2.))
                            .rounded_full()
                            .bg(rgb(0x6C6C70)),
                    )
                    .child(
                        div()
                            .absolute()
                            .top(px(8.))
                            .left(px(13.))
                            .w(px(2.))
                            .h(px(12.))
                            .rounded_full()
                            .bg(rgb(0x6C6C70)),
                    ),
            )
            .child(div().flex_1().child(Input::new(&self.input_state)))
            .child(
                div()
                    .id("send")
                    .cursor_pointer()
                    .w(px(44.))
                    .h(px(30.))
                    .rounded_full()
                    .bg(rgb(BLUE))
                    .hover(|d| d.bg(rgb(0x0071E3)))
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
            .bg(rgb(0xFFFFFF))
            .child(header)
            .children(settings_panel)
            .children(banner)
            .child(messages)
            .child(composer);

        h_flex().size_full().child(self.sidebar(cx)).child(main)
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
