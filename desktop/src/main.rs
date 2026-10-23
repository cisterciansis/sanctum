//! Sanctum desktop — iMessage-style chat over Hippius dead drops.
//! Demo: run two instances, `SANCTUM_ROLE=alice` and `SANCTUM_ROLE=bob`
//! (creds from experiment/hippius.env); they pair through the drop and chat.

use std::time::Duration;

use chrono::Local;
use gpui_kit::assets::Assets;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    *,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use sanctum_core::demo::{spawn_worker, Cmd, Event, Settings};

struct Msg {
    id: u64,
    mine: bool,
    delivered: bool,
    text: SharedString,
    time: SharedString,
    day: String,
}

impl Msg {
    fn now(id: u64, mine: bool, text: String) -> Self {
        let now = Local::now();
        Self {
            id,
            mine,
            delivered: false,
            text: text.into(),
            time: now.format("%-I:%M %p").to_string().into(),
            day: now.format("%A, %b %-d").to_string(),
        }
    }
}

struct ChatApp {
    peer: String,
    paired: bool,
    status: SharedString,
    msgs: Vec<Msg>,
    next_id: u64,
    settings: Settings,
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
                Event::Received(text) => self.msgs.push(Msg::now(u64::MAX, false, text)),
                Event::Delivered(id) => {
                    if let Some(m) = self.msgs.iter_mut().find(|m| m.mine && m.id == id) {
                        m.delivered = true;
                    }
                }
                Event::Unsent(id) => self.msgs.retain(|m| !(m.mine && m.id == id)),
                Event::Error(e) => self.status = format!("error: {e}").into(),
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn send_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input_state.read(cx).value().trim().to_string();
        if text.is_empty() || !self.paired {
            return;
        }
        self.input_state.update(cx, |s, cx| s.set_value("", window, cx));
        let id = self.next_id;
        self.next_id += 1;
        self.msgs.push(Msg::now(id, true, text.clone()));
        let _ = self.outbound.send(Cmd::Send(id, text));
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
                // Settings: toggle timestamps (persisted).
                Button::new("ts-toggle")
                    .ghost()
                    .label("🕒")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings.timestamps = !this.settings.timestamps;
                        this.settings.save();
                        cx.notify();
                    })),
            );

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

            let id = m.id;
            let bubble: AnyElement = if unsendable {
                bubble
                    .id(("msg", i))
                    .on_click(cx.listener(move |this, _, _, _| {
                        let _ = this.outbound.send(Cmd::Unsend(id));
                    }))
                    .child(m.text.clone())
                    .into_any_element()
            } else {
                bubble.child(m.text.clone()).into_any_element()
            };

            let mut col = v_flex()
                .w_full()
                .when(m.mine, |d| d.items_end())
                .when(!m.mine, |d| d.items_start())
                .child(bubble);
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
            .child(div().flex_1().child(Input::new(&self.input_state)))
            .child(
                Button::new("send")
                    .primary()
                    .label("↑")
                    .on_click(cx.listener(|this, _, window, cx| this.send_current(window, cx))),
            );

        v_flex()
            .size_full()
            .bg(rgb(0xFFFFFF))
            .child(header)
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
