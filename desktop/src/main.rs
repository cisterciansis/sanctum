//! Sanctum desktop — iMessage-style chat over Hippius dead drops.
//! Demo: run two instances, `SANCTUM_ROLE=alice` and `SANCTUM_ROLE=bob`
//! (creds from experiment/hippius.env); they pair through the drop and chat.

use std::time::Duration;

use chrono::Local;
use gpui_kit::assets::Assets;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    switch::Switch,
    *,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use sanctum_core::demo::{spawn_worker, Cmd, Event, Settings};

struct Msg {
    id: u64,
    /// Cross-device name for this message (reaction/edit references).
    wire_id: String,
    mine: bool,
    delivered: bool,
    reaction: Option<SharedString>,
    text: SharedString,
    /// Attachment (display name, local path).
    file: Option<(SharedString, String)>,
    created: std::time::Instant,
    time: SharedString,
    day: String,
}

impl Msg {
    fn now(id: u64, wire_id: String, mine: bool, text: String) -> Self {
        let now = Local::now();
        Self {
            id,
            wire_id,
            mine,
            delivered: false,
            reaction: None,
            text: text.into(),
            file: None,
            created: std::time::Instant::now(),
            time: now.format("%-I:%M %p").to_string().into(),
            day: now.format("%A, %b %-d").to_string(),
        }
    }

    fn with_file(mut self, name: String, path: String) -> Self {
        self.file = Some((name.into(), path));
        self
    }
}

fn is_image(name: &str) -> bool {
    let lower = name.to_lowercase();
    ["png", "jpg", "jpeg", "gif", "webp", "bmp"]
        .iter()
        .any(|e| lower.ends_with(&format!(".{e}")))
}

const REACTIONS: [&str; 6] = ["👍", "👎", "❤️", "‼️", "❓", "😂"];

struct ChatApp {
    peer: String,
    paired: bool,
    status: SharedString,
    msgs: Vec<Msg>,
    next_id: u64,
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
        let role = std::env::var("SANCTUM_ROLE").unwrap_or_else(|_| "alice".into());
        let peer = if role == "alice" { "Bob" } else { "Alice" }.to_string();

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

        // Drain worker events twice a second.
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
            peer,
            paired: false,
            status: "connecting to Hippius…".into(),
            msgs: Vec::new(),
            next_id: 0,
            settings: Settings::load(),
            show_settings: false,
            picker_for: None,
            input_state,
            outbound: out_tx,
            events: ev_rx,
            _subscriptions,
        }
    }

    fn drain_events(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        while let Ok(ev) = self.events.try_recv() {
            changed = true;
            match ev {
                Event::Paired => {
                    self.paired = true;
                    self.status = "end-to-end encrypted · hippius".into();
                }
                Event::Received { wire_id, text } => {
                    self.msgs.push(Msg::now(u64::MAX, wire_id, false, text))
                }
                Event::ReceivedFile { wire_id, name, path } => self
                    .msgs
                    .push(Msg::now(u64::MAX, wire_id, false, String::new()).with_file(name, path)),
                Event::TimerChanged(secs) => {
                    self.settings.disappear_secs = secs;
                    self.settings.save();
                }
                Event::Reaction { ref_id, emoji } => {
                    if let Some(m) =
                        self.msgs.iter_mut().find(|m| !m.wire_id.is_empty() && m.wire_id == ref_id)
                    {
                        m.reaction = Some(emoji.into());
                    }
                }
                Event::Delivered(id) => {
                    if let Some(m) = self.msgs.iter_mut().find(|m| m.mine && m.id == id) {
                        m.delivered = true;
                    }
                }
                Event::Unsent(id) => self.msgs.retain(|m| !(m.mine && m.id == id)),
                Event::Error(e) => self.status = format!("error: {e}").into(),
            }
        }
        // Disappearing messages: purely local timed deletion (the drop copy
        // was already deleted at pickup).
        let secs = self.settings.disappear_secs;
        if secs > 0 {
            let before = self.msgs.len();
            self.msgs.retain(|m| m.created.elapsed().as_secs() < secs);
            if self.msgs.len() != before {
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn attach_file(&mut self, cx: &mut Context<Self>) {
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
                        let id = this.next_id;
                        this.next_id += 1;
                        let wire_id = ulid::Ulid::new().to_string();
                        this.msgs.push(
                            Msg::now(id, wire_id.clone(), true, String::new())
                                .with_file(name, path.clone()),
                        );
                        let _ = this.outbound.send(Cmd::SendFile { id, wire_id, path });
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    fn send_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input_state.read(cx).value().trim().to_string();
        if text.is_empty() || !self.paired {
            return;
        }
        self.input_state.update(cx, |s, cx| s.set_value("", window, cx));
        let id = self.next_id;
        self.next_id += 1;
        let wire_id = ulid::Ulid::new().to_string();
        self.msgs.push(Msg::now(id, wire_id.clone(), true, text.clone()));
        let _ = self.outbound.send(Cmd::Send { id, wire_id, text });
        cx.notify();
    }
}

impl Render for ChatApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let show_time = self.settings.timestamps;
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
                    .child(div().text_sm().font_semibold().child(self.peer.clone()))
                    .child(div().text_xs().text_color(rgb(0x8E8E93)).child(self.status.clone())),
            )
            .child(
                Button::new("settings-menu")
                    .ghost()
                    .label("⋯")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_settings = !this.show_settings;
                        cx.notify();
                    })),
            );

        let settings_panel = if self.show_settings {
            let row = |label: &str, hint: Option<&str>| {
                let mut l = v_flex().flex_1().child(div().text_sm().child(label.to_string()));
                if let Some(h) = hint {
                    l = l.child(div().text_xs().text_color(rgb(0x8E8E93)).child(h.to_string()));
                }
                h_flex().w_full().items_center().gap_3().child(l)
            };
            Some(
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
                        row(
                            "Disappearing messages",
                            Some("timed local deletion, synced to your contact"),
                        )
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
                                    let _ = this
                                        .outbound
                                        .send(Cmd::SetTimer(this.settings.disappear_secs));
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        row("Typing indicators", Some("coming soon · off keeps traffic quiet"))
                            .child(Switch::new("set-typing").checked(false).disabled(true)),
                    ),
            )
        } else {
            None
        };

        let last_delivered = self.msgs.iter().rposition(|m| m.mine && m.delivered);

        let mut rows: Vec<AnyElement> = Vec::new();
        let mut prev_day: Option<&str> = None;
        for (i, m) in self.msgs.iter().enumerate() {
            if prev_day != Some(m.day.as_str()) {
                prev_day = Some(m.day.as_str());
                rows.push(
                    div()
                        .w_full()
                        .text_center()
                        .text_xs()
                        .text_color(rgb(0x8E8E93))
                        .py_1()
                        .child(m.day.clone())
                        .into_any_element(),
                );
            }

            let bubble = div()
                .max_w(relative(0.72))
                .px_3()
                .py_1p5()
                .text_sm()
                .rounded(px(18.));
            let bubble = if m.mine {
                bubble.bg(rgb(0x0A84FF)).text_color(rgb(0xFFFFFF))
            } else {
                bubble.bg(rgb(0xE9E9EB)).text_color(rgb(0x000000))
            };

            // Caption line under the bubble: time, receipt, unsend affordance.
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

            // Attachment bubbles: inline image, or a file chip.
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

            let id = m.id;
            let reactable = !m.mine && !m.wire_id.is_empty();
            let wire_id = m.wire_id.clone();
            let bubble: AnyElement = if unsendable {
                bubble
                    .id(("msg", i))
                    .on_click(cx.listener(move |this, _, _, _| {
                        let _ = this.outbound.send(Cmd::Unsend(id));
                    }))
                    .child(content)
                    .into_any_element()
            } else if reactable {
                // Click a received bubble to open the tapback picker.
                bubble
                    .id(("msg", i))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.picker_for = if this.picker_for == Some(i) { None } else { Some(i) };
                        cx.notify();
                    }))
                    .child(content)
                    .into_any_element()
            } else {
                bubble.child(content).into_any_element()
            };

            // Tapback picker: iMessage-style pill above the bubble.
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
                    let wire_id = wire_id.clone();
                    pill = pill.child(
                        div()
                            .id(("react", i * 10 + n))
                            .cursor_pointer()
                            .text_lg()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let _ = this.outbound.send(Cmd::React {
                                    ref_id: wire_id.clone(),
                                    emoji: (*emoji).into(),
                                });
                                if let Some(m) = this.msgs.get_mut(i) {
                                    m.reaction = Some((*emoji).into());
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
                    div()
                        .text_xs()
                        .text_color(rgb(0x8E8E93))
                        .child(caption.join(" · ")),
                );
            }
            rows.push(col.into_any_element());
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
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(0x6C6C70))
                    .text_lg()
                    .on_click(cx.listener(|this, _, _, cx| this.attach_file(cx)))
                    .child("+"),
            )
            .child(div().flex_1().child(Input::new(&self.input_state)))
            .child(
                // iMessage-style send: blue capsule, bold white up arrow.
                div()
                    .id("send")
                    .cursor_pointer()
                    .w(px(44.))
                    .h(px(30.))
                    .rounded_full()
                    .bg(rgb(0x0A84FF))
                    .hover(|d| d.bg(rgb(0x0071E3)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(0xFFFFFF))
                    .text_lg()
                    .font_bold()
                    .on_click(cx.listener(|this, _, window, cx| this.send_current(window, cx)))
                    .child(
                        Icon::new(gpui_kit::assets::IconName::ArrowUp)
                            .with_size(px(18.))
                            .text_color(rgb(0xFFFFFF)),
                    ),
            );

        v_flex()
            .size_full()
            .bg(rgb(0xFFFFFF))
            .child(header)
            .children(settings_panel)
            .child(messages)
            .child(composer)
    }
}

fn main() {
    let app = gpui_kit::application().with_assets(Assets);
    app.run(move |cx| {
        gpui_kit::init(cx);
        let role = std::env::var("SANCTUM_ROLE").unwrap_or_else(|_| "alice".into());
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(420.), px(640.)), cx)),
            titlebar: Some(TitlebarOptions {
                title: Some(format!("Sanctum — {role}").into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        gpui_kit::open_window(window_options, cx, |window, cx| {
            cx.new(|cx| ChatApp::new(window, cx))
        })
        .expect("Failed to open window");
    });
}
