//! Sanctum desktop — iMessage-style chat over Hippius dead drops.
//! Demo: run two instances, `SANCTUM_ROLE=alice` and `SANCTUM_ROLE=bob`
//! (creds from experiment/hippius.env); they pair through the drop and chat.

mod chat;

use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use gpui_kit::assets::Assets;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    *,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

enum Event {
    Paired,
    Received(String),
    Error(String),
}

struct Msg {
    mine: bool,
    text: SharedString,
}

/// Worker thread: owns the blocking Session; UI talks to it via channels.
fn spawn_worker(outbound: Receiver<String>, events: Sender<Event>) {
    std::thread::spawn(move || {
        let mut session = match chat::Session::from_env() {
            Ok(s) => s,
            Err(e) => {
                let _ = events.send(Event::Error(e));
                return;
            }
        };
        let mut paired = false;
        loop {
            if !paired && session.pair() {
                paired = true;
                let _ = events.send(Event::Paired);
            }
            while let Ok(text) = outbound.try_recv() {
                if let Err(e) = session.send(&text) {
                    let _ = events.send(Event::Error(e));
                }
            }
            if paired {
                for text in session.poll() {
                    let _ = events.send(Event::Received(text));
                }
            }
            std::thread::sleep(Duration::from_millis(1500));
        }
    });
}

struct ChatApp {
    role: String,
    peer: String,
    paired: bool,
    status: SharedString,
    msgs: Vec<Msg>,
    input_state: Entity<InputState>,
    outbound: Sender<String>,
    events: Receiver<Event>,
    _subscriptions: Vec<Subscription>,
}

impl ChatApp {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let role = std::env::var("SANCTUM_ROLE").unwrap_or_else(|_| "alice".into());
        let peer = if role == "alice" { "Bob" } else { "Alice" }.to_string();

        let (out_tx, out_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        spawn_worker(out_rx, ev_tx);

        let input_state = cx.new(|cx| InputState::new(window, cx).placeholder("iMessage"));
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
            role,
            peer,
            paired: false,
            status: "connecting to Hippius…".into(),
            msgs: Vec::new(),
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
                Event::Received(text) => self.msgs.push(Msg { mine: false, text: text.into() }),
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
        self.msgs.push(Msg { mine: true, text: text.clone().into() });
        let _ = self.outbound.send(text);
        cx.notify();
    }

    fn bubble(msg: &Msg) -> Div {
        let bubble = div()
            .max_w(relative(0.72))
            .px_3()
            .py_1p5()
            .text_sm()
            .rounded(px(18.));
        let bubble = if msg.mine {
            bubble.bg(rgb(0x0A84FF)).text_color(rgb(0xFFFFFF))
        } else {
            bubble.bg(rgb(0xE9E9EB)).text_color(rgb(0x000000))
        };
        h_flex()
            .w_full()
            .when(msg.mine, |d| d.justify_end())
            .when(!msg.mine, |d| d.justify_start())
            .child(bubble.child(msg.text.clone()))
    }
}

impl Render for ChatApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = v_flex()
            .w_full()
            .items_center()
            .pt_3()
            .pb_2()
            .border_b_1()
            .border_color(rgb(0xDDDDDD))
            .child(div().text_sm().font_semibold().child(self.peer.clone()))
            .child(div().text_xs().text_color(rgb(0x8E8E93)).child(self.status.clone()));

        let messages = v_flex()
            .id("messages")
            .flex_1()
            .w_full()
            .gap_1()
            .p_3()
            .overflow_y_scroll()
            .children(self.msgs.iter().map(Self::bubble));

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
