//! Mobile FFI facade (UniFFI): the same contact worker the desktop and TUI
//! use, behind a thread-safe polled interface that Swift/Kotlin drive with a
//! timer. All state lives in the encrypted store; HOME on iOS/Android is the
//! app sandbox, so paths and the OS keychain resolve correctly as-is.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;

use crate::contact::{spawn_worker, Cmd, Event};
use crate::store::{Contact, Mark, StoredMsg};



#[derive(uniffi::Record, Clone)]
pub struct FfiMsg {
    pub wire_id: String,
    pub mine: bool,
    pub delivered: bool,
    pub reaction: Option<String>,
    pub text: String,
    pub file_path: Option<String>,
    pub ts_ms: i64,
    /// Group messages: sender's seat number (None in direct chats).
    pub sender_seat: Option<i64>,
    /// Own messages: when the peer read them (read receipts on).
    pub read_ms: Option<i64>,
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
            sender_seat: m.sender_seat,
            read_ms: m.read_ms,
        }
    }
}

#[derive(uniffi::Record, Clone)]
pub struct FfiContact {
    pub id: i64,
    pub name: String,
    pub waiting: bool,
    pub invite_link: String,
    pub verified: bool,
    pub blocked: bool,
    pub favorite: bool,
    /// Someone wrote under a different identity key (refused); cleared by
    /// verifying again.
    pub key_changed: bool,
    /// "12345 67890 …" (12 groups of 5); empty until the peer joins.
    pub safety: String,
}

impl From<Contact> for FfiContact {
    fn from(c: Contact) -> Self {
        Self {
            id: c.id,
            waiting: c.peer_pk.is_empty(),
            name: c.name,
            invite_link: c.invite_link,
            verified: c.verified,
            blocked: c.blocked,
            favorite: c.favorite,
            key_changed: c.key_changed,
            safety: c.safety,
        }
    }
}

#[derive(uniffi::Enum)]
pub enum FfiEvent {
    Snapshot { chats: Vec<FfiContact> },
    ContactAdded { contact: FfiContact },
    ContactRemoved { contact_id: i64 },
    ContactUpdated { contact: FfiContact },
    PeerJoined { contact_id: i64 },
    Received { contact_id: i64, msg: FfiMsg },
    Delivered { contact_id: i64, wire_id: String },
    Sent { contact_id: i64, wire_id: String },
    Read { contact_id: i64, wire_ids: Vec<String>, ts_ms: i64 },
    SendFailed { contact_id: i64, wire_id: String, message: String },
    /// Unsent, or deleted here or by the peer: drop it from the view.
    Removed { contact_id: i64, wire_id: String },
    Reaction { contact_id: i64, ref_id: String, emoji: String },
    History { contact_id: i64, msgs: Vec<FfiMsg> },
    ErrorEvent { message: String },
}

#[derive(uniffi::Record, Clone)]
pub struct FfiSettings {
    pub theme: String,
    pub timestamps: bool,
    pub proxy_enabled: bool,
    pub proxy_url: String,
    pub disappear_secs: u64,
    pub read_receipts: bool,
}

#[derive(uniffi::Object)]
pub struct Client {
    outbound: Mutex<Sender<Cmd>>,
    events: Mutex<Receiver<Event>>,
    /// Snapshot history kept here so the UI can lazily fetch per chat.
    history: Mutex<std::collections::HashMap<i64, Vec<FfiMsg>>>,
}

/// Disappearing-message presets (seconds; 0 = off), shared by all clients.
#[uniffi::export]
pub fn disappear_presets() -> Vec<u64> {
    crate::demo::DISAPPEAR_PRESETS.to_vec()
}

/// Custom timer like "90m" or "6mo"; None unless off or 1 minute to 1 year.
#[uniffi::export]
pub fn parse_disappear(input: String) -> Option<u64> {
    crate::demo::parse_disappear(&input)
}

#[uniffi::export]
pub fn disappear_label(secs: u64) -> String {
    crate::demo::disappear_label(secs)
}

/// A message's payment chips (Cashu, lightning:, bitcoin:, BOLT11) and its
/// text with long payment strings shortened.
#[uniffi::export]
pub fn pay_scan(text: String) -> crate::pay::Payments {
    crate::pay::scan(&text)
}

/// `/pay <token>`: the bare Cashu token to send, or None if it isn't a
/// cleanly decoding token with a positive amount.
#[uniffi::export]
pub fn pay_token(input: String) -> Option<String> {
    crate::pay::pay_token(&input).ok().map(|(token, _)| token)
}

#[uniffi::export]
pub fn cashu_bearer_warning() -> String {
    crate::pay::BEARER_WARNING.into()
}

#[derive(uniffi::Record)]
pub struct FfiTorStatus {
    pub ready: bool,
    pub message: String,
}

/// Embedded Tor progress for the UI; None while Tor isn't in use.
#[uniffi::export]
pub fn tor_status() -> Option<FfiTorStatus> {
    #[cfg(feature = "tor")]
    let s = crate::tor::status();
    #[cfg(not(feature = "tor"))]
    let s = None;
    s.map(|(ready, message)| FfiTorStatus { ready, message })
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
                        chats.push(c.into());
                    }
                    FfiEvent::Snapshot { chats }
                }
                Event::ContactAdded(c) => FfiEvent::ContactAdded { contact: c.into() },
                Event::ContactRemoved { contact_id } => FfiEvent::ContactRemoved { contact_id },
                Event::ContactUpdated(c) => FfiEvent::ContactUpdated { contact: c.into() },
                Event::PeerJoined { contact_id } => FfiEvent::PeerJoined { contact_id },
                Event::Received { contact_id, msg } => {
                    FfiEvent::Received { contact_id, msg: msg.into() }
                }
                Event::Delivered { contact_id, wire_id } => {
                    FfiEvent::Delivered { contact_id, wire_id }
                }
                Event::Sent { contact_id, wire_id } => FfiEvent::Sent { contact_id, wire_id },
                Event::Read { contact_id, wire_ids, ts_ms } => {
                    FfiEvent::Read { contact_id, wire_ids, ts_ms }
                }
                Event::SendFailed { contact_id, wire_id, error } => {
                    FfiEvent::SendFailed { contact_id, wire_id, message: error }
                }
                Event::Reaction { contact_id, ref_id, emoji } => {
                    FfiEvent::Reaction { contact_id, ref_id, emoji }
                }
                Event::TimerChanged { .. } => continue_marker(),
                Event::Unsent { contact_id, wire_id } => FfiEvent::Removed { contact_id, wire_id },
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

    /// Send a file from a local path; returns the wire id.
    pub fn send_file(&self, contact_id: i64, path: String) -> String {
        let wire_id = ulid::Ulid::new().to_string();
        let _ = self.outbound.lock().expect("outbound lock").send(Cmd::SendFile {
            contact_id,
            wire_id: wire_id.clone(),
            path,
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

    /// Mint a group (2-6 members): one invite link per other seat arrives in
    /// the ContactAdded event's invite_link, newline-separated.
    pub fn new_group(&self, name: String, size: i64) {
        let _ = self.outbound.lock().expect("outbound lock").send(Cmd::NewGroup { name, size });
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

    pub fn get_settings(&self) -> FfiSettings {
        let s = crate::demo::Settings::load();
        FfiSettings {
            theme: s.theme,
            timestamps: s.timestamps,
            proxy_enabled: s.proxy_enabled,
            proxy_url: s.proxy_url,
            disappear_secs: s.disappear_secs,
            read_receipts: s.read_receipts,
        }
    }

    pub fn save_settings(&self, new: FfiSettings) {
        let mut s = crate::demo::Settings::load();
        s.theme = new.theme;
        s.timestamps = new.timestamps;
        s.proxy_enabled = new.proxy_enabled;
        s.proxy_url = new.proxy_url;
        s.disappear_secs = new.disappear_secs;
        s.read_receipts = new.read_receipts;
        s.save();
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

    /// The user is looking at this chat (call on open and on new messages
    /// while it is on screen).
    pub fn mark_read(&self, contact_id: i64) {
        let _ = self.outbound.lock().expect("outbound lock").send(Cmd::MarkRead { contact_id });
    }

    /// Delete a message here; `everyone` also deletes it on the peer's side.
    pub fn delete_msg(&self, contact_id: i64, wire_id: String, everyone: bool) {
        let _ = self
            .outbound
            .lock()
            .expect("outbound lock")
            .send(Cmd::DeleteMsg { contact_id, wire_id, everyone });
    }

    pub fn set_verified(&self, contact_id: i64, on: bool) {
        self.mark(contact_id, Mark::Verified, on);
    }

    pub fn set_blocked(&self, contact_id: i64, on: bool) {
        self.mark(contact_id, Mark::Blocked, on);
    }

    pub fn set_favorite(&self, contact_id: i64, on: bool) {
        self.mark(contact_id, Mark::Favorite, on);
    }

    /// Emergency wipe: delete all local data and start over as on first
    /// launch (a fresh worker, a fresh identity). Instant and offline.
    /// Returns what couldn't be deleted, if anything.
    pub fn wipe(&self) -> Option<String> {
        let result = crate::contact::wipe();
        let (out_tx, out_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        spawn_worker(out_rx, ev_tx);
        *self.outbound.lock().expect("outbound lock") = out_tx;
        *self.events.lock().expect("events lock") = ev_rx;
        self.history.lock().expect("history lock").clear();
        result.err()
    }

    pub fn unsend(&self, contact_id: i64, wire_id: String) {
        let _ = self
            .outbound
            .lock()
            .expect("outbound lock")
            .send(Cmd::Unsend { contact_id, wire_id });
    }
}

impl Client {
    fn mark(&self, contact_id: i64, mark: Mark, on: bool) {
        let _ = self.outbound.lock().expect("outbound lock").send(Cmd::Mark { contact_id, mark, on });
    }
}

/// Placeholder for events the mobile MVP ignores (filtered out above).
fn continue_marker() -> FfiEvent {
    FfiEvent::ErrorEvent { message: "__skip__".into() }
}
