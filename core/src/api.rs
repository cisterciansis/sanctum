//! Mobile FFI facade (UniFFI): the same contact worker the desktop and TUI
//! use, behind a thread-safe polled interface that Swift/Kotlin drive with a
//! timer. All state lives in the encrypted store; HOME on iOS/Android is the
//! app sandbox, so paths and the OS keychain resolve correctly as-is.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;

use crate::contact::{spawn_worker, Cmd, Event};
use crate::store::{Contact, Mark, StoredMsg};
use crate::tao::{self, Net};
use crate::wallet::{PayCard, TaoCmd, WalletView};



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
    /// TAO payment card, if this message is one.
    pub pay: Option<FfiPayCard>,
}

/// A payment card for display; amounts pre-formatted by the core.
#[derive(uniffi::Record, Clone)]
pub struct FfiPayCard {
    /// "paid" | "request" | "cheque"
    pub kind: String,
    /// "sending" | "sent" | "verifying" | "verified" | "unverified" | "failed"
    /// | "open" | "settled" | "pending" | "cashing" | "cashed" | "bounced"
    pub status: String,
    pub testnet: bool,
    /// "1.5 τ"
    pub amount: String,
    pub from: String,
    pub to: String,
    pub tx: Option<String>,
    pub memo: Option<String>,
    pub note: Option<String>,
}

fn ffi_card(json: Option<&str>) -> Option<FfiPayCard> {
    let c = PayCard::parse(json?)?;
    let word = |v: serde_json::Value| v.as_str().unwrap_or_default().to_string();
    Some(FfiPayCard {
        kind: word(serde_json::to_value(c.kind).ok()?),
        status: word(serde_json::to_value(c.status).ok()?),
        testnet: c.net == Net::Test,
        amount: tao::format_tao(c.amount()),
        from: c.from,
        to: c.to,
        tx: c.tx,
        memo: c.memo,
        note: c.note,
    })
}

#[derive(uniffi::Record, Clone)]
pub struct FfiTaoAccount {
    pub name: String,
    pub address: String,
    pub watch_only: bool,
    /// "12.5 τ", or None while unknown.
    pub balance: Option<String>,
}

/// The Hippius account that pays for the mailboxes this device creates.
#[derive(uniffi::Record, Clone)]
pub struct FfiMailbox {
    /// None when connected by a pasted API token (no phrase on device).
    pub address: Option<String>,
    /// "$12.34", or None while unknown.
    pub credits: Option<String>,
    /// Ready to start chats.
    pub funded: bool,
    pub deposit: Option<String>,
    /// "0.0832 τ": what the $20 top-up costs today.
    pub fund_tao: Option<String>,
    pub busy: bool,
    pub note: Option<String>,
}

#[derive(uniffi::Record, Clone)]
pub struct FfiWallet {
    pub testnet: bool,
    pub active: Option<String>,
    pub accounts: Vec<FfiTaoAccount>,
    pub mailbox: Option<FfiMailbox>,
}

impl From<WalletView> for FfiWallet {
    fn from(w: WalletView) -> Self {
        Self {
            testnet: w.net == Net::Test,
            active: w.active,
            accounts: w
                .accounts
                .into_iter()
                .map(|a| FfiTaoAccount {
                    name: a.name,
                    address: a.address,
                    watch_only: a.watch_only,
                    balance: a.balance.map(tao::format_tao),
                })
                .collect(),
            mailbox: w.mailbox.map(|m| FfiMailbox {
                address: m.address,
                credits: m.credits.map(|c| format!("${c:.2}")),
                funded: m.credits.is_some_and(|c| c > 0.0),
                deposit: m.deposit,
                fund_tao: m.fund_rao.map(tao::format_tao),
                busy: m.busy,
                note: m.note,
            }),
        }
    }
}

/// "1.5" → rao as a decimal string, or None if it isn't a valid amount.
#[uniffi::export]
pub fn tao_parse_amount(input: String) -> Option<String> {
    tao::parse_tao(&input).map(|r| r.to_string())
}

/// A Bittensor (SS58 prefix 42) address with a valid checksum.
#[uniffi::export]
pub fn tao_valid_address(input: String) -> bool {
    tao::parse_address(&input).is_ok()
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
            pay: ffi_card(m.pay.as_deref()),
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
    MsgUpdated { contact_id: i64, wire_id: String, text: String, pay: Option<FfiPayCard> },
    TaoWallet { wallet: FfiWallet },
    TaoPhrase { address: String, phrase: String },
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

impl Client {
    fn tao(&self, cmd: TaoCmd) {
        let _ = self.outbound.lock().expect("outbound lock").send(Cmd::Tao(cmd));
    }
}

/// Bluetooth mesh (see `mesh`): the app's radio layer moves frames, the core
/// does the rest. Split a packet into frames that fit one link write.
#[uniffi::export]
pub fn mesh_frames(packet: Vec<u8>, mtu: u32) -> Vec<Vec<u8>> {
    crate::mesh::frames(&packet, mtu as usize)
}

/// Packets this device originates (its sends and acks), for every link.
#[uniffi::export]
pub fn mesh_outbox() -> Vec<Vec<u8>> {
    crate::mesh::MESH.drain()
}

/// How many links are up; 0 sends everything straight to Hippius.
#[uniffi::export]
pub fn mesh_set_links(count: u32) {
    crate::mesh::MESH.set_neighbors(count as usize)
}

#[uniffi::export]
pub fn mesh_link_closed(link: String) {
    crate::mesh::MESH.link_closed(&link)
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
                Event::MsgUpdated { contact_id, wire_id, text, pay } => FfiEvent::MsgUpdated {
                    contact_id,
                    wire_id,
                    text,
                    pay: ffi_card(pay.as_deref()),
                },
                Event::TaoWallet(w) => FfiEvent::TaoWallet { wallet: w.into() },
                Event::TaoPhrase { address, phrase } => FfiEvent::TaoPhrase { address, phrase },
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

    /// Balances (and the offline snapshot); answers with a TaoWallet event.
    pub fn tao_refresh(&self) {
        self.tao(TaoCmd::Refresh);
    }

    pub fn tao_new(&self, name: String) {
        self.tao(TaoCmd::New { name });
        self.tao(TaoCmd::Refresh);
    }

    pub fn tao_import(&self, name: String, phrase: String) {
        self.tao(TaoCmd::Import { name, phrase });
        self.tao(TaoCmd::Refresh);
    }

    pub fn tao_watch(&self, name: String, address: String) {
        self.tao(TaoCmd::Watch { name, address });
        self.tao(TaoCmd::Refresh);
    }

    pub fn tao_use(&self, address: String) {
        self.tao(TaoCmd::Use { address });
    }

    pub fn tao_forget(&self, address: String) {
        self.tao(TaoCmd::Forget { address });
    }

    /// Answers with a TaoPhrase event. Call only on an explicit user action.
    pub fn tao_reveal(&self, address: String) {
        self.tao(TaoCmd::Reveal { address });
    }

    /// Open a mailbox account: a new one, or restore with a phrase.
    pub fn mailbox_open(&self, phrase: Option<String>) {
        self.tao(TaoCmd::MailboxOpen { phrase });
    }

    /// Answers with a TaoPhrase event. Call only on an explicit user action.
    pub fn mailbox_reveal(&self) {
        self.tao(TaoCmd::MailboxReveal);
    }

    /// Top up $20 of credits from the active TAO account.
    pub fn mailbox_fund(&self) {
        self.tao(TaoCmd::MailboxFund);
    }

    pub fn tao_set_testnet(&self, testnet: bool) {
        self.tao(TaoCmd::SetNet { net: if testnet { Net::Test } else { Net::Main } });
    }

    pub fn tao_share(&self, contact_id: i64) {
        self.tao(TaoCmd::Share { contact_id });
    }

    /// Pay `amount` ("1.5") to the contact's shared address or `to`. False if
    /// the amount is invalid. Confirm with the user before calling.
    pub fn tao_pay(&self, contact_id: i64, amount: String, to: Option<String>, re: Option<String>) -> bool {
        let Some(rao) = tao::parse_tao(&amount) else { return false };
        let wire_id = ulid::Ulid::new().to_string();
        self.tao(TaoCmd::Pay { contact_id, wire_id, rao, to, re });
        true
    }

    pub fn tao_request(&self, contact_id: i64, amount: String, memo: Option<String>) -> bool {
        let Some(rao) = tao::parse_tao(&amount) else { return false };
        let wire_id = ulid::Ulid::new().to_string();
        self.tao(TaoCmd::Request { contact_id, wire_id, rao, memo });
        true
    }

    pub fn tao_cheque(&self, contact_id: i64, amount: String) -> bool {
        let Some(rao) = tao::parse_tao(&amount) else { return false };
        let wire_id = ulid::Ulid::new().to_string();
        self.tao(TaoCmd::Cheque { contact_id, wire_id, rao });
        true
    }

    pub fn tao_cash(&self, contact_id: i64, wire_id: String) {
        self.tao(TaoCmd::Cash { contact_id, wire_id });
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
    /// `keep_wallets` keeps the TAO and mailbox accounts. Returns what went
    /// wrong, if anything.
    pub fn wipe(&self, keep_wallets: bool) -> Option<String> {
        let result = match crate::contact::wipe(keep_wallets) {
            Err(e) => return Some(e), // nothing erased; keep running as we were
            Ok(problem) => problem,
        };
        let (out_tx, out_rx) = std::sync::mpsc::channel();
        let (ev_tx, ev_rx) = std::sync::mpsc::channel();
        spawn_worker(out_rx, ev_tx);
        *self.outbound.lock().expect("outbound lock") = out_tx;
        *self.events.lock().expect("events lock") = ev_rx;
        self.history.lock().expect("history lock").clear();
        result
    }

    /// One frame from a mesh link (an opaque per-link id). Returns a packet
    /// to relay to every other link, if any.
    pub fn mesh_receive(&self, link: String, frame: Vec<u8>) -> Option<Vec<u8>> {
        let (packet, relay) = crate::mesh::MESH.receive(&link, &frame)?;
        let _ = self.outbound.lock().expect("outbound lock").send(Cmd::Mesh(packet));
        relay
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
