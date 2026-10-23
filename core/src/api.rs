//! Mobile FFI facade (UniFFI): the same contact worker the desktop and TUI
//! use, behind a thread-safe polled interface that Swift/Kotlin drive with a
//! timer. All state lives in the encrypted store; HOME on iOS/Android is the
//! app sandbox, so paths and the OS keychain resolve correctly as-is.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;

use crate::contact::{spawn_worker, Cmd, Event};
use crate::store::StoredMsg;



#[derive(uniffi::Record, Clone)]
pub struct FfiMsg {
    pub wire_id: String,
    pub mine: bool,
    pub delivered: bool,
    pub reaction: Option<String>,
    pub text: String,
    pub file_path: Option<String>,
    pub ts_ms: i64,
}

impl From<StoredMsg> for FfiMsg {
    fn from(m: StoredMsg) -> Self {
        Self {
            wire_id: m.wire_id,
            mine: m.mine,
            delivered: m.delivered,
            reaction: m.reaction,
            text: m.text,
            file_path: m.file_path,
            ts_ms: m.ts_ms,
        }
    }
}

#[derive(uniffi::Record, Clone)]
pub struct FfiContact {
    pub id: i64,
    pub name: String,
    pub waiting: bool,
    pub invite_link: String,
}

#[derive(uniffi::Enum)]
pub enum FfiEvent {
    Snapshot { chats: Vec<FfiContact> },
    ContactAdded { contact: FfiContact },
    ContactRemoved { contact_id: i64 },
    PeerJoined { contact_id: i64 },
    Received { contact_id: i64, msg: FfiMsg },
    Delivered { contact_id: i64, wire_id: String },
    Reaction { contact_id: i64, ref_id: String, emoji: String },
    History { contact_id: i64, msgs: Vec<FfiMsg> },
    ErrorEvent { message: String },
}

#[derive(uniffi::Object)]
pub struct Client {
    outbound: Mutex<Sender<Cmd>>,
    events: Mutex<Receiver<Event>>,
    /// Snapshot history kept here so the UI can lazily fetch per chat.
    history: Mutex<std::collections::HashMap<i64, Vec<FfiMsg>>>,
}

#[uniffi::export]
impl Client {
    #[uniffi::constructor]
    pub fn new() -> Self {
        let (out_tx, out_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        spawn_worker(out_rx, ev_tx);
        Self {
            outbound: Mutex::new(out_tx),
            events: Mutex::new(ev_rx),
            history: Mutex::new(Default::default()),
        }
    }

    /// Drain pending events; the app calls this on a ~1 s timer.
    pub fn poll_events(&self) -> Vec<FfiEvent> {
        let rx = self.events.lock().expect("events lock");
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(match ev {
                Event::Snapshot(all) => {
                    let mut hist = self.history.lock().expect("history lock");
                    let mut chats = Vec::new();
                    for (c, msgs) in all {
                        hist.insert(c.id, msgs.into_iter().map(Into::into).collect());
                        chats.push(FfiContact {
                            id: c.id,
                            name: c.name,
                            waiting: c.peer_pk.is_empty(),
                            invite_link: c.invite_link,
                        });
                    }
                    FfiEvent::Snapshot { chats }
                }
                Event::ContactAdded(c) => FfiEvent::ContactAdded {
                    contact: FfiContact {
                        id: c.id,
                        name: c.name,
                        waiting: c.peer_pk.is_empty(),
                        invite_link: c.invite_link,
                    },
                },
                Event::ContactRemoved { contact_id } => FfiEvent::ContactRemoved { contact_id },
                Event::PeerJoined { contact_id } => FfiEvent::PeerJoined { contact_id },
                Event::Received { contact_id, msg } => {
                    FfiEvent::Received { contact_id, msg: msg.into() }
                }
                Event::Delivered { contact_id, wire_id } => {
                    FfiEvent::Delivered { contact_id, wire_id }
                }
                Event::Reaction { contact_id, ref_id, emoji } => {
                    FfiEvent::Reaction { contact_id, ref_id, emoji }
                }
                Event::TimerChanged { .. } => continue_marker(),
                Event::Unsent { contact_id, wire_id } => {
                    // Mobile MVP folds unsend into a history reload later;
                    // surface it as a delivered-style no-op for now.
                    FfiEvent::Delivered { contact_id, wire_id }
                }
                Event::History { contact_id, msgs } => FfiEvent::History {
                    contact_id,
                    msgs: msgs.into_iter().map(Into::into).collect(),
                },
                Event::Error(message) => FfiEvent::ErrorEvent { message },
            });
        }
        out.retain(|e| !matches!(e, FfiEvent::ErrorEvent { message } if message == "__skip__"));
        out
    }

    /// History captured with the startup snapshot, per chat.
    pub fn snapshot_messages(&self, contact_id: i64) -> Vec<FfiMsg> {
        self.history
            .lock()
            .expect("history lock")
            .get(&contact_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn send(&self, contact_id: i64, text: String) -> String {
        let wire_id = ulid::Ulid::new().to_string();
        let _ = self.outbound.lock().expect("outbound lock").send(Cmd::Send {
            contact_id,
            wire_id: wire_id.clone(),
            text,
        });
        wire_id
    }

    pub fn accept_invite(&self, link: String, name: String) {
        let _ = self
            .outbound
            .lock()
            .expect("outbound lock")
            .send(Cmd::AcceptInvite { link, name });
    }

    pub fn new_invite(&self, name: String) {
        let _ = self.outbound.lock().expect("outbound lock").send(Cmd::NewInvite { name });
    }

    pub fn react(&self, contact_id: i64, ref_id: String, emoji: String) {
        let _ = self
            .outbound
            .lock()
            .expect("outbound lock")
            .send(Cmd::React { contact_id, ref_id, emoji });
    }

    pub fn rename(&self, contact_id: i64, name: String) {
        let _ = self.outbound.lock().expect("outbound lock").send(Cmd::Rename { contact_id, name });
    }

    pub fn delete_contact(&self, contact_id: i64) {
        let _ = self
            .outbound
            .lock()
            .expect("outbound lock")
            .send(Cmd::DeleteContact { contact_id });
    }

    /// Store the Hippius management token (enables creating invites).
    pub fn set_api_token(&self, token: String) {
        let mut s = crate::demo::Settings::load();
        s.hippius_api_token = if token.trim().is_empty() { None } else { Some(token.trim().into()) };
        s.save();
    }

    pub fn has_api_token(&self) -> bool {
        crate::demo::Settings::load().hippius_api_token.is_some()
    }

    pub fn set_timer(&self, contact_id: i64, secs: u64) {
        let _ = self
            .outbound
            .lock()
            .expect("outbound lock")
            .send(Cmd::SetTimer { contact_id, secs });
    }

    pub fn clear_history(&self, contact_id: i64, older_than_secs: Option<u64>) {
        let _ = self
            .outbound
            .lock()
            .expect("outbound lock")
            .send(Cmd::ClearHistory { contact_id, older_than_secs });
    }

    pub fn unsend(&self, contact_id: i64, wire_id: String) {
        let _ = self
            .outbound
            .lock()
            .expect("outbound lock")
            .send(Cmd::Unsend { contact_id, wire_id });
    }
}

/// Placeholder for events the mobile MVP ignores (filtered out above).
fn continue_marker() -> FfiEvent {
    FfiEvent::ErrorEvent { message: "__skip__".into() }
}
