//! Demo chat session over one shared dead-drop bucket, used by the desktop
//! and TUI clients.
//!
//! Pairing: each side publishes its public key at `pk/<role>` and polls for
//! the peer's. Messages go to `to-<peer>/<ulid>`; each side polls its own
//! `to-<role>/` prefix and deletes after read. Demo ceiling: one shared
//! bucket — the real app mints one bucket per contact (SPEC-V2 §4b).

use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use crypto_box::PublicKey;

use crate::{open, seal, transport::Mailbox, Identity};

pub struct Session {
    pub role: String,
    peer: String,
    me: Identity,
    peer_pk: Option<PublicKey>,
    mbox: Mailbox,
}

impl Session {
    /// Role is "alice" or "bob"; creds picked from env accordingly.
    pub fn from_env() -> Result<Self, String> {
        let role = std::env::var("SANCTUM_ROLE").unwrap_or_else(|_| "alice".into());
        let peer = if role == "alice" { "bob" } else { "alice" };
        let (ak, sk) = if role == "alice" {
            ("OWNER_KEY", "OWNER_SECRET")
        } else {
            ("SENDER_KEY", "SENDER_SECRET")
        };
        let env = |k: &str| std::env::var(k).map_err(|_| format!("missing env {k}"));
        let bucket = std::env::var("SANCTUM_BUCKET").unwrap_or_else(|_| "owner".into());
        let mbox = Mailbox::new(
            crate::transport::DEFAULT_ENDPOINT,
            &bucket,
            &env(ak)?,
            &env(sk)?,
        )
        .map_err(|e| e.to_string())?;

        // Persist identity across runs so re-pairing isn't needed.
        let dir = std::env::var("HOME")
            .map(|h| format!("{h}/.sanctum"))
            .map_err(|_| "no HOME".to_string())?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let key_path = format!("{dir}/demo-{role}.key");
        let me = match std::fs::read_to_string(&key_path) {
            Ok(s) => Identity::from_b64(s.trim()).map_err(|e| e.to_string())?,
            Err(_) => {
                let id = Identity::generate();
                std::fs::write(&key_path, id.to_b64()).map_err(|e| e.to_string())?;
                id
            }
        };

        Ok(Self { role, peer: peer.into(), me, peer_pk: None, mbox })
    }

    /// Publish own pk; fetch peer's. True once paired.
    pub fn pair(&mut self) -> bool {
        if self.peer_pk.is_some() {
            return true;
        }
        let _ = self.mbox.put(
            &format!("pk/{}", self.role),
            B64.encode(self.me.public().as_bytes()).as_bytes(),
        );
        if let Ok(raw) = self.mbox.get(&format!("pk/{}", self.peer)) {
            if let Ok(bytes) = B64.decode(String::from_utf8_lossy(&raw).trim()) {
                if let Ok(arr) = <[u8; 32]>::try_from(bytes.as_slice()) {
                    self.peer_pk = Some(PublicKey::from_bytes(arr));
                }
            }
        }
        self.peer_pk.is_some()
    }

    /// Returns the deposited object key (used for delivery receipts).
    pub fn send(&self, text: &str) -> Result<String, String> {
        let pk = self.peer_pk.as_ref().ok_or("not paired yet")?;
        let sealed = seal(&self.me, pk, text.as_bytes()).map_err(|e| e.to_string())?;
        let key = format!("to-{}/{}", self.peer, ulid::Ulid::new());
        self.mbox.put(&key, &sealed).map_err(|e| e.to_string())?;
        Ok(key)
    }

    /// Delivery check for a key this side deposited: the recipient deletes
    /// after decrypt, so absence = delivered to their device.
    pub fn is_delivered(&self, key: &str) -> Result<bool, String> {
        Ok(!self.mbox.exists(key).map_err(|e| e.to_string())?)
    }

    /// Unsend: delete our own object before pickup — the message never
    /// reaches the peer. True deletion, not a tombstone.
    pub fn unsend(&self, key: &str) -> Result<(), String> {
        self.mbox.delete(key).map_err(|e| e.to_string())
    }

    /// Drain this side's inbox prefix; delete after successful decrypt.
    pub fn poll(&self) -> Vec<String> {
        let mut out = Vec::new();
        let Ok(keys) = self.mbox.list(&format!("to-{}/", self.role)) else {
            return out;
        };
        for key in keys {
            let Ok(raw) = self.mbox.get(&key) else { continue };
            if let Ok((_, pt)) = open(&self.me, &raw) {
                out.push(String::from_utf8_lossy(&pt).into_owned());
                let _ = self.mbox.delete(&key);
            }
        }
        out
    }
}

/// Client settings shared by GUI and TUI, persisted as JSON.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy)]
pub struct Settings {
    pub timestamps: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { timestamps: true }
    }
}

impl Settings {
    fn path() -> Option<String> {
        std::env::var("HOME").ok().map(|h| format!("{h}/.sanctum/settings.json"))
    }

    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(p) = Self::path() {
            if let Some(dir) = std::path::Path::new(&p).parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default());
        }
    }
}

/// Commands a UI sends to the worker. Ids are UI-chosen per message.
pub enum Cmd {
    Send(u64, String),
    /// Delete the message from the drop if the peer hasn't fetched it yet.
    Unsend(u64),
}

/// Events the background worker reports to a UI. Delivered/Unsent echo the
/// UI's message id; Delivered fires once the peer's device has fetched and
/// decrypted that message (its drop object is gone).
pub enum Event {
    Paired,
    Received(String),
    Delivered(u64),
    Unsent(u64),
    Error(String),
}

/// Worker thread: owns the blocking Session; UIs talk to it via channels.
pub fn spawn_worker(outbound: Receiver<Cmd>, events: Sender<Event>) {
    std::thread::spawn(move || {
        let mut session = match Session::from_env() {
            Ok(s) => s,
            Err(e) => {
                let _ = events.send(Event::Error(e));
                return;
            }
        };
        let mut paired = false;
        // (id, key, seen): a receipt needs presence first, then absence —
        // otherwise list-lag would read as instant delivery.
        let mut pending: Vec<(u64, String, bool)> = Vec::new();
        loop {
            if !paired && session.pair() {
                paired = true;
                let _ = events.send(Event::Paired);
            }
            while let Ok(cmd) = outbound.try_recv() {
                match cmd {
                    Cmd::Send(id, text) => match session.send(&text) {
                        Ok(key) => pending.push((id, key, false)),
                        Err(e) => {
                            let _ = events.send(Event::Error(e));
                        }
                    },
                    Cmd::Unsend(id) => {
                        if let Some(pos) = pending.iter().position(|(pid, ..)| *pid == id) {
                            let (_, key, _) = pending.remove(pos);
                            match session.unsend(&key) {
                                Ok(()) => {
                                    let _ = events.send(Event::Unsent(id));
                                }
                                // Peer's poll may have deleted it first: delivered wins.
                                Err(_) => {
                                    let _ = events.send(Event::Delivered(id));
                                }
                            }
                        }
                        // Not pending = already delivered; the UI already knows.
                    }
                }
            }
            if paired {
                for text in session.poll() {
                    let _ = events.send(Event::Received(text));
                }
            }
            pending.retain_mut(|(id, key, seen)| match session.is_delivered(key) {
                Ok(false) => {
                    *seen = true;
                    true
                }
                Ok(true) if *seen => {
                    let _ = events.send(Event::Delivered(*id));
                    false
                }
                _ => true,
            });
            std::thread::sleep(Duration::from_millis(1500));
        }
    });
}
