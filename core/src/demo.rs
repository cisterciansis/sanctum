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
        let default_peer = if role == "alice" { "bob" } else { "alice" };
        let peer = std::env::var("SANCTUM_PEER").unwrap_or_else(|_| default_peer.into());
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

    fn deposit(&self, body: &serde_json::Value) -> Result<String, String> {
        let pk = self.peer_pk.as_ref().ok_or("not paired yet")?;
        let sealed =
            seal(&self.me, pk, body.to_string().as_bytes()).map_err(|e| e.to_string())?;
        let key = format!("to-{}/{}", self.peer, ulid::Ulid::new());
        self.mbox.put(&key, &sealed).map_err(|e| e.to_string())?;
        Ok(key)
    }

    /// Send a text message. `wire_id` names it across devices (reactions,
    /// edits reference it). Returns the deposited object key (receipts).
    pub fn send(&self, wire_id: &str, text: &str) -> Result<String, String> {
        self.deposit(&serde_json::json!({ "t": "m", "id": wire_id, "x": text }))
    }

    /// React to a message by its wire id. Fire-and-forget control message.
    pub fn react(&self, ref_id: &str, emoji: &str) -> Result<(), String> {
        self.deposit(&serde_json::json!({ "t": "r", "ref": ref_id, "e": emoji }))?;
        Ok(())
    }

    /// Tell the peer the disappearing-messages timer changed (0 = off).
    pub fn set_timer(&self, secs: u64) -> Result<(), String> {
        self.deposit(&serde_json::json!({ "t": "tmr", "s": secs }))?;
        Ok(())
    }

    /// Send a file: the blob is sealed exactly like a message (same E2E
    /// envelope) and parked under att/; the chat message carries its pointer.
    /// Returns the message object key (receipts track the message).
    /// // ceiling: whole file in memory, 25 MB cap; chunking when needed.
    pub fn send_file(&self, wire_id: &str, path: &str) -> Result<(String, String), String> {
        let bytes = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
        if bytes.len() > 25 * 1024 * 1024 {
            return Err("file larger than 25 MB".into());
        }
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let pk = self.peer_pk.as_ref().ok_or("not paired yet")?;
        let sealed = seal(&self.me, pk, &bytes).map_err(|e| e.to_string())?;
        let att_key = format!("att/{}", ulid::Ulid::new());
        self.mbox.put(&att_key, &sealed).map_err(|e| e.to_string())?;
        let msg_key = self
            .deposit(&serde_json::json!({ "t": "a", "id": wire_id, "n": name, "k": att_key }))?;
        Ok((msg_key, name))
    }

    /// Fetch + decrypt an attachment into ~/.sanctum/files/, then remove it
    /// from the drop. Returns the local path.
    pub fn fetch_attachment(&self, att_key: &str, name: &str) -> Result<String, String> {
        let raw = self.mbox.get(att_key).map_err(|e| e.to_string())?;
        let (_, bytes) = open(&self.me, &raw).map_err(|e| e.to_string())?;
        let dir = std::env::var("HOME")
            .map(|h| format!("{h}/.sanctum/files"))
            .map_err(|_| "no HOME".to_string())?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // Prefix with a ulid so a hostile filename can't overwrite anything.
        let safe = name.replace(['/', '\\'], "_");
        let path = format!("{dir}/{}-{safe}", ulid::Ulid::new());
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        let _ = self.mbox.delete(att_key);
        Ok(path)
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
    pub fn poll(&self) -> Vec<Incoming> {
        let mut out = Vec::new();
        let Ok(keys) = self.mbox.list(&format!("to-{}/", self.role)) else {
            return out;
        };
        for key in keys {
            let Ok(raw) = self.mbox.get(&key) else { continue };
            if let Ok((_, pt)) = open(&self.me, &raw) {
                out.push(Incoming::parse(&pt));
                let _ = self.mbox.delete(&key);
            }
        }
        out
    }
}

/// A decrypted inbox item. Non-JSON payloads (pre-wire-format builds) fall
/// back to a plain message with an empty id.
pub enum Incoming {
    Message { id: String, text: String },
    Attachment { id: String, name: String, att_key: String },
    Reaction { ref_id: String, emoji: String },
    Timer { secs: u64 },
}

impl Incoming {
    fn parse(pt: &[u8]) -> Self {
        let txt = String::from_utf8_lossy(pt);
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
            let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
            match v.get("t").and_then(|t| t.as_str()) {
                Some("m") => return Incoming::Message { id: s("id"), text: s("x") },
                Some("a") => {
                    return Incoming::Attachment { id: s("id"), name: s("n"), att_key: s("k") }
                }
                Some("r") => return Incoming::Reaction { ref_id: s("ref"), emoji: s("e") },
                Some("tmr") => {
                    return Incoming::Timer {
                        secs: v.get("s").and_then(|x| x.as_u64()).unwrap_or(0),
                    }
                }
                _ => {}
            }
        }
        Incoming::Message { id: String::new(), text: txt.into_owned() }
    }
}

/// Client settings shared by GUI and TUI, persisted as JSON. The container
/// default means a hand-written partial file (e.g. just hippius_api_token)
/// still parses.
#[derive(serde::Serialize, serde::Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub timestamps: bool,
    /// Route all gateway traffic through `proxy_url` (Tor, UR, any SOCKS5/HTTP
    /// proxy). Applied when a session starts; SANCTUM_PROXY env overrides.
    #[serde(default)]
    pub proxy_enabled: bool,
    #[serde(default = "default_proxy_url")]
    pub proxy_url: String,
    /// Not implemented yet; reserved so the toggle can ship in the UI.
    #[serde(default)]
    pub typing_indicators: bool,
    /// Disappearing messages: delete from local view this many seconds after
    /// they appear (0 = off). Synced to the peer as a control message.
    #[serde(default)]
    pub disappear_secs: u64,
    /// Send read receipts, and show the peer's. Reciprocal; off by default.
    #[serde(default)]
    pub read_receipts: bool,
    /// TAO network for the wallet: "main" or "test".
    #[serde(default = "default_tao_net")]
    pub tao_net: String,
    /// Optional Hippius management-API token (console → Settings). Purely
    /// opt-in: without it the app works as-is; with it the client can mint
    /// per-contact buckets/sub-tokens and rotate queues.
    #[serde(default)]
    pub hippius_api_token: Option<String>,
    /// Dev convenience: keep the DB key in a 0600 file instead of the OS
    /// keychain, so unsigned dev rebuilds don't re-prompt on every launch.
    /// Release builds are code-signed and should leave this false.
    #[serde(default)]
    pub dev_keyfile: bool,
    /// "light" or "dark".
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Accent color for own bubbles / send button, 0xRRGGBB.
    #[serde(default = "default_accent")]
    pub accent: u32,
}

fn default_tao_net() -> String {
    "main".into()
}

fn default_theme() -> String {
    "light".into()
}

fn default_accent() -> u32 {
    0x0A84FF
}

fn default_proxy_url() -> String {
    "socks5://127.0.0.1:9050".into()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            timestamps: true,
            proxy_enabled: false,
            proxy_url: default_proxy_url(),
            typing_indicators: false,
            disappear_secs: 0,
            read_receipts: false,
            tao_net: default_tao_net(),
            hippius_api_token: None,
            dev_keyfile: false,
            theme: default_theme(),
            accent: default_accent(),
        }
    }
}

impl Settings {
    fn path() -> Option<String> {
        crate::data_dir().map(|d| format!("{d}/settings.json"))
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

/// Disappearing-message presets every client offers (seconds; 0 = off).
pub const DISAPPEAR_PRESETS: [u64; 5] = [0, 60, 3600, DAY, 30 * DAY];
/// Bounds for a custom timer: one minute to one year.
pub const DISAPPEAR_MIN: u64 = 60;
pub const DISAPPEAR_MAX: u64 = 365 * DAY;
const DAY: u64 = 86_400;

/// Off, or within the custom bounds. Checked on every timer a peer sends:
/// an unchecked huge value overflowed the purge cutoff and wiped history.
pub fn disappear_ok(secs: u64) -> bool {
    secs == 0 || (DISAPPEAR_MIN..=DISAPPEAR_MAX).contains(&secs)
}

/// Parses "off", "90m", "36 hours", "2w", "6mo", "1y"; None if malformed or
/// out of bounds. A month is 30 days, a year 365.
pub fn parse_disappear(input: &str) -> Option<u64> {
    let s = input.trim().to_ascii_lowercase();
    if s == "off" {
        return Some(0);
    }
    let split = s.find(|c: char| !c.is_ascii_digit())?;
    let n: u64 = s[..split].parse().ok()?;
    let unit = match s[split..].trim() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
        "d" | "day" | "days" => DAY,
        "w" | "wk" | "wks" | "week" | "weeks" => 7 * DAY,
        "mo" | "month" | "months" => 30 * DAY,
        "y" | "yr" | "yrs" | "year" | "years" => 365 * DAY,
        _ => return None,
    };
    n.checked_mul(unit).filter(|&secs| secs > 0 && disappear_ok(secs))
}

/// "Off", "1 month", "90 minutes": the largest unit that divides exactly.
pub fn disappear_label(secs: u64) -> String {
    if secs == 0 {
        return "Off".into();
    }
    let units = [
        (365 * DAY, "year"),
        (30 * DAY, "month"),
        (7 * DAY, "week"),
        (DAY, "day"),
        (3600, "hour"),
        (60, "minute"),
        (1, "second"),
    ];
    let (unit, name) = units.into_iter().find(|(u, _)| secs % u == 0).expect("1 divides");
    let n = secs / unit;
    format!("{n} {name}{}", if n == 1 { "" } else { "s" })
}

/// The preset after `secs` (custom values restart the cycle at off).
pub fn next_disappear_preset(secs: u64) -> u64 {
    let i = DISAPPEAR_PRESETS.iter().position(|&p| p == secs).map_or(0, |i| i + 1);
    DISAPPEAR_PRESETS[i % DISAPPEAR_PRESETS.len()]
}

/// Commands a UI sends to the worker. `id` is the UI's local handle;
/// `wire_id` names the message across devices (reaction/edit references).
pub enum Cmd {
    Send { id: u64, wire_id: String, text: String },
    SendFile { id: u64, wire_id: String, path: String },
    React { ref_id: String, emoji: String },
    /// Sync the disappearing-messages timer to the peer (0 = off).
    SetTimer(u64),
    /// Delete the message from the drop if the peer hasn't fetched it yet.
    Unsend(u64),
}

/// Events the background worker reports to a UI. Delivered/Unsent echo the
/// UI's message id; Delivered fires once the peer's device has fetched and
/// decrypted that message (its drop object is gone).
pub enum Event {
    Paired,
    Received { wire_id: String, text: String },
    /// A file arrived and was decrypted to `path`.
    ReceivedFile { wire_id: String, name: String, path: String },
    Reaction { ref_id: String, emoji: String },
    /// Peer changed the disappearing-messages timer (0 = off).
    TimerChanged(u64),
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
                    Cmd::Send { id, wire_id, text } => match session.send(&wire_id, &text) {
                        Ok(key) => pending.push((id, key, false)),
                        Err(e) => {
                            let _ = events.send(Event::Error(e));
                        }
                    },
                    Cmd::SendFile { id, wire_id, path } => match session.send_file(&wire_id, &path) {
                        Ok((key, _)) => pending.push((id, key, false)),
                        Err(e) => {
                            let _ = events.send(Event::Error(e));
                        }
                    },
                    Cmd::React { ref_id, emoji } => {
                        if let Err(e) = session.react(&ref_id, &emoji) {
                            let _ = events.send(Event::Error(e));
                        }
                    }
                    Cmd::SetTimer(secs) => {
                        if let Err(e) = session.set_timer(secs) {
                            let _ = events.send(Event::Error(e));
                        }
                    }
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
                for item in session.poll() {
                    let ev = match item {
                        Incoming::Message { id, text } => Event::Received { wire_id: id, text },
                        Incoming::Attachment { id, name, att_key } => {
                            match session.fetch_attachment(&att_key, &name) {
                                Ok(path) => Event::ReceivedFile { wire_id: id, name, path },
                                Err(e) => Event::Error(format!("attachment {name}: {e}")),
                            }
                        }
                        Incoming::Reaction { ref_id, emoji } => Event::Reaction { ref_id, emoji },
                        Incoming::Timer { secs } => Event::TimerChanged(secs),
                    };
                    let _ = events.send(ev);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disappear_parsing_and_bounds() {
        assert_eq!(parse_disappear("off"), Some(0));
        assert_eq!(parse_disappear(" 90m "), Some(5400));
        assert_eq!(parse_disappear("2 Weeks"), Some(14 * DAY));
        assert_eq!(parse_disappear("1y"), Some(DISAPPEAR_MAX));
        assert_eq!(parse_disappear("60s"), Some(DISAPPEAR_MIN));
        for bad in ["59s", "0m", "366d", "2y", "90", "m", "5 parsecs", "-1h", "99999999999999999999y"] {
            assert_eq!(parse_disappear(bad), None, "{bad}");
        }
        assert!(!disappear_ok(u64::MAX) && !disappear_ok(30) && disappear_ok(0));
        assert_eq!(disappear_label(30 * DAY), "1 month");
        assert_eq!(disappear_label(5400), "90 minutes");
        assert_eq!(disappear_label(DISAPPEAR_MAX), "1 year");
        assert_eq!(next_disappear_preset(DAY), 30 * DAY);
        assert_eq!(next_disappear_preset(30 * DAY), 0);
        assert_eq!(next_disappear_preset(5400), 0);
    }
}
