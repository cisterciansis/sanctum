//! TAO in chats: the wallet the worker keeps, payment cards, and the
//! `taoaddr` / `pay` control messages. Chain work (seconds to a minute) runs
//! on helper threads and reports back through `Jobs`, so chats never stall.
//!
//! Trust: a peer's card is a claim. "Paid" is shown only once the transfer
//! is verified on chain; a cheque's amount and payee come from its signed
//! bytes; addresses, amounts and sizes are validated before anything else.

use std::sync::mpsc::{channel, Receiver, Sender};

use serde::{Deserialize, Serialize};

use crate::contact::{Event, Live};
use crate::store::{Store, StoredMsg};
use crate::tao::{self, Account, Net, Receipt, Snapshot};

// ---------------------------------------------------------------------------
// Persistent wallet (inside the encrypted DB)
// ---------------------------------------------------------------------------

#[derive(Default, Serialize, Deserialize)]
struct Wallet {
    accounts: Vec<Account>,
    /// Address used for chat payments.
    active: Option<String>,
    /// Cheques we signed that may not have landed yet, oldest nonce first.
    #[serde(default)]
    issued: Vec<Issued>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Issued {
    net: Net,
    from: String,
    nonce: u64,
    bytes: String,
    contact_id: i64,
    wire_id: String,
}

const WALLET_KEY: &str = "tao/wallet";

impl Wallet {
    fn load(store: &Store) -> Self {
        store.kv_get(WALLET_KEY).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    fn save(&self, store: &Store) {
        if let Ok(s) = serde_json::to_string(self) {
            let _ = store.kv_set(WALLET_KEY, &s);
        }
    }

    fn active(&self) -> Option<&Account> {
        let a = self.active.as_deref()?;
        self.accounts.iter().find(|x| x.address == a)
    }
}

fn net() -> Net {
    Net::from_id(&crate::demo::Settings::load().tao_net).unwrap_or(Net::Main)
}

fn snap_key(net: Net, address: &str) -> String {
    format!("tao/snap/{}/{address}", net.id())
}

fn peer_key(l: &Live) -> String {
    format!("tao/peer/{}", l.contact.bucket_in)
}

// ---------------------------------------------------------------------------
// Payment cards
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PayKind {
    Paid,
    Request,
    Cheque,
    /// A shared TAO address (from a `taoaddr` message; `to` holds it).
    Address,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PayStatus {
    /// Ours, on its way to the chain.
    Sending,
    /// Ours, finalized on chain.
    Sent,
    /// Theirs, being checked on chain.
    Verifying,
    /// Theirs, confirmed on chain.
    Verified,
    /// Theirs, not found on chain as claimed.
    Unverified,
    Failed,
    /// A request not yet paid.
    Open,
    /// A request that has been paid.
    Settled,
    /// A cheque not yet cashed.
    Pending,
    Cashing,
    Cashed,
    Bounced,
}

/// One payment as shown in a chat. Stored as JSON on the message; `summary`
/// is the plain text for clients without cards.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PayCard {
    pub kind: PayKind,
    pub status: PayStatus,
    pub net: Net,
    /// Amount in rao, as a decimal string (u128 doesn't fit JSON numbers).
    pub rao: String,
    /// Payer ("" on a request).
    pub from: String,
    /// Payee.
    pub to: String,
    #[serde(default)]
    pub tx: Option<String>,
    #[serde(default)]
    pub block: Option<String>,
    /// Signed cheque bytes, 0x-hex.
    #[serde(default)]
    pub cheque: Option<String>,
    #[serde(default)]
    pub memo: Option<String>,
    /// For a payment: the request it settles.
    #[serde(default)]
    pub re: Option<String>,
    /// Why it failed / bounced.
    #[serde(default)]
    pub note: Option<String>,
}

impl PayCard {
    /// A stored card (`StoredMsg::pay`), for clients.
    pub fn parse(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }

    pub fn amount(&self) -> u128 {
        self.rao.parse().unwrap_or(0)
    }

    pub fn summary(&self) -> String {
        let amt = tao::format_tao(self.amount());
        let net = if self.net == Net::Test { " (testnet)" } else { "" };
        let memo = self.memo.as_deref().map(|m| format!(" · {m}")).unwrap_or_default();
        let why = self.note.as_deref().map(|n| format!(": {n}")).unwrap_or_default();
        let s = match (self.kind, self.status) {
            (PayKind::Address, _) => format!("TAO address: {}", self.to),
            (PayKind::Paid, PayStatus::Sending) => format!("Sending {amt}…"),
            (PayKind::Paid, PayStatus::Sent) => format!("Sent {amt}"),
            (PayKind::Paid, PayStatus::Verifying) => format!("{amt} received · verifying on chain…"),
            (PayKind::Paid, PayStatus::Verified) => format!("Received {amt} · verified on chain"),
            (PayKind::Paid, PayStatus::Unverified) => format!("Claimed {amt} · NOT found on chain"),
            (PayKind::Request, PayStatus::Open) => format!("Request: {amt}{memo}"),
            (PayKind::Request, PayStatus::Settled) => format!("Request paid: {amt}{memo}"),
            (PayKind::Cheque, PayStatus::Pending) => format!("Cheque: {amt} · cash when online"),
            (PayKind::Cheque, PayStatus::Cashing) => format!("Cashing cheque {amt}…"),
            (PayKind::Cheque, PayStatus::Cashed) => format!("Cheque cashed: {amt}"),
            (PayKind::Cheque, PayStatus::Bounced) => format!("Cheque bounced: {amt}{why}"),
            (_, PayStatus::Failed) => format!("Payment failed: {amt}{why}"),
            (_, s) => format!("{amt} · {s:?}"),
        };
        format!("τ {s}{net}")
    }

    /// Everything a peer's card claims, checked before we act on it.
    fn validate(&self) -> Result<(), String> {
        let rao: u128 = self.rao.parse().map_err(|_| "bad amount")?;
        if rao == 0 || rao > 21_000_000 * tao::RAO_PER_TAO {
            return Err("amount out of range".into());
        }
        tao::parse_address(&self.to).map_err(|e| e.to_string())?;
        if !self.from.is_empty() {
            tao::parse_address(&self.from).map_err(|e| e.to_string())?;
        }
        if self.memo.as_ref().is_some_and(|m| m.chars().count() > 140)
            || self.cheque.as_ref().is_some_and(|c| c.len() > 8192)
            || [&self.tx, &self.block, &self.re].iter().any(|x| x.as_ref().is_some_and(|x| x.len() > 80))
        {
            return Err("oversized field".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Worker side
// ---------------------------------------------------------------------------

pub enum TaoCmd {
    /// Refresh balances (and the offline snapshot for the active account).
    Refresh,
    New { name: String },
    Import { name: String, phrase: String },
    Watch { name: String, address: String },
    Use { address: String },
    Forget { address: String },
    /// Show an account's recovery phrase (explicit user action only).
    Reveal { address: String },
    SetNet { net: Net },
    /// Send our active address to this contact.
    Share { contact_id: i64 },
    /// Pay `rao` to the contact (their shared address) or to `to`; `re`
    /// settles one of their requests.
    Pay { contact_id: i64, wire_id: String, rao: u128, to: Option<String>, re: Option<String> },
    Request { contact_id: i64, wire_id: String, rao: u128, memo: Option<String> },
    /// Sign a cheque offline and hand it to the contact.
    Cheque { contact_id: i64, wire_id: String, rao: u128 },
    /// Cash a cheque card in this chat.
    Cash { contact_id: i64, wire_id: String },
}

#[derive(Clone, Debug)]
pub struct AccountView {
    pub name: String,
    pub address: String,
    pub watch_only: bool,
    /// None while unknown (offline or not fetched yet).
    pub balance: Option<u128>,
}

#[derive(Clone, Debug)]
pub struct WalletView {
    pub net: Net,
    pub active: Option<String>,
    pub accounts: Vec<AccountView>,
    /// Address the selected contact shared, if any (per contact, filled by UI asks).
    pub note: Option<String>,
}

/// Results coming back from helper threads.
pub enum Done {
    Balances(Vec<(String, Option<u128>)>, Option<(Net, String, Snapshot)>),
    Card { contact_id: i64, wire_id: String, card: PayCard, tell_peer: bool },
    Error(String),
}

pub struct Jobs {
    tx: Sender<Done>,
    rx: Receiver<Done>,
    /// Last known balances, by address.
    balances: Vec<(String, Option<u128>)>,
}

impl Jobs {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, balances: Vec::new() }
    }

    fn spawn(&self, f: impl FnOnce() -> Done + Send + 'static) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
    }
}

fn view(store: &Store, jobs: &Jobs) -> WalletView {
    let w = Wallet::load(store);
    WalletView {
        net: net(),
        active: w.active.clone(),
        accounts: w
            .accounts
            .iter()
            .map(|a| AccountView {
                name: a.name.clone(),
                address: a.address.clone(),
                watch_only: !a.can_sign(),
                balance: jobs.balances.iter().find(|(x, _)| *x == a.address).and_then(|(_, b)| *b),
            })
            .collect(),
        note: None,
    }
}

fn post_card(
    store: &Store,
    events: &Sender<Event>,
    contact_id: i64,
    wire_id: &str,
    mine: bool,
    card: &PayCard,
) {
    let msg = StoredMsg {
        wire_id: wire_id.into(),
        mine,
        text: card.summary(),
        file_path: None,
        ts_ms: crate::store::chrono_now_ms(),
        delivered: false,
        reaction: None,
        sender_seat: None,
        read_ms: None,
        pay: serde_json::to_string(card).ok(),
    };
    let _ = store.add_msg(contact_id, &msg);
    let _ = events.send(Event::Received { contact_id, msg });
}

/// The card for a shared TAO address (no amount; `to` is the address).
fn address_card(net: Net, address: &str) -> PayCard {
    PayCard {
        kind: PayKind::Address,
        status: PayStatus::Open,
        net,
        rao: "0".into(),
        from: String::new(),
        to: address.into(),
        tx: None,
        block: None,
        cheque: None,
        memo: None,
        re: None,
        note: None,
    }
}

fn update_card(store: &Store, events: &Sender<Event>, contact_id: i64, wire_id: &str, card: &PayCard) {
    let text = card.summary();
    let pay = serde_json::to_string(card).ok();
    let _ = store.set_msg(contact_id, wire_id, &text, pay.as_deref());
    let _ = events.send(Event::MsgUpdated { contact_id, wire_id: wire_id.into(), text, pay });
}

fn card_of(store: &Store, contact_id: i64, wire_id: &str) -> Option<PayCard> {
    store
        .messages(contact_id)
        .ok()?
        .into_iter()
        .find(|m| m.wire_id == wire_id)?
        .pay
        .and_then(|p| serde_json::from_str(&p).ok())
}

pub(crate) fn handle_cmd(cmd: TaoCmd, store: &Store, live: &mut [Live], events: &Sender<Event>, jobs: &Jobs) {
    let fail = |e: String| {
        let _ = events.send(Event::Error(format!("tao: {e}")));
    };
    let mut w = Wallet::load(store);
    match cmd {
        TaoCmd::Refresh => {
            let accounts: Vec<String> = w.accounts.iter().map(|a| a.address.clone()).collect();
            let signer = w.active().filter(|a| a.can_sign()).map(|a| a.address.clone());
            let issued = w.issued.clone();
            let net = net();
            let _ = events.send(Event::TaoWallet(view(store, jobs)));
            jobs.spawn(move || {
                // Our own outstanding cheques land first (either side may cash
                // them), so later sends don't collide with their nonces.
                for c in issued.iter().filter(|c| c.net == net) {
                    if let Ok(bytes) = tao::unhex(&c.bytes) {
                        let _ = tao::cash_cheque(net, &bytes);
                    }
                }
                let balances = accounts.iter().map(|a| (a.clone(), tao::balance(net, a).ok())).collect();
                let snap = signer.and_then(|a| tao::snapshot(net, &a).ok().map(|s| (net, a, s)));
                Done::Balances(balances, snap)
            });
        }
        TaoCmd::New { name } => match Account::generate(&name) {
            Ok(a) => {
                w.active.get_or_insert(a.address.clone());
                w.accounts.push(a);
                w.save(store);
                let _ = events.send(Event::TaoWallet(view(store, jobs)));
            }
            Err(e) => fail(e.to_string()),
        },
        TaoCmd::Import { name, phrase } => match Account::import(&name, &phrase) {
            Ok(a) if w.accounts.iter().any(|x| x.address == a.address) => fail("already in the wallet".into()),
            Ok(a) => {
                w.active.get_or_insert(a.address.clone());
                w.accounts.push(a);
                w.save(store);
                let _ = events.send(Event::TaoWallet(view(store, jobs)));
            }
            Err(e) => fail(e.to_string()),
        },
        TaoCmd::Watch { name, address } => match Account::watch(&name, &address) {
            Ok(a) if w.accounts.iter().any(|x| x.address == a.address) => fail("already in the wallet".into()),
            Ok(a) => {
                w.active.get_or_insert(a.address.clone());
                w.accounts.push(a);
                w.save(store);
                let _ = events.send(Event::TaoWallet(view(store, jobs)));
            }
            Err(e) => fail(e.to_string()),
        },
        TaoCmd::Use { address } => {
            if w.accounts.iter().any(|a| a.address == address) {
                w.active = Some(address);
                w.save(store);
                let _ = events.send(Event::TaoWallet(view(store, jobs)));
            }
        }
        TaoCmd::Forget { address } => {
            w.accounts.retain(|a| a.address != address);
            w.issued.retain(|c| c.from != address);
            if w.active.as_deref() == Some(address.as_str()) {
                w.active = w.accounts.first().map(|a| a.address.clone());
            }
            w.save(store);
            let _ = events.send(Event::TaoWallet(view(store, jobs)));
        }
        TaoCmd::Reveal { address } => {
            if let Some(phrase) = w.accounts.iter().find(|a| a.address == address).and_then(|a| a.phrase.clone()) {
                let _ = events.send(Event::TaoPhrase { address, phrase });
            }
        }
        TaoCmd::SetNet { net } => {
            let mut s = crate::demo::Settings::load();
            s.tao_net = net.id().into();
            s.save();
            handle_cmd(TaoCmd::Refresh, store, live, events, jobs);
        }
        TaoCmd::Share { contact_id } => {
            let Some(addr) = w.active.clone() else { return fail("no wallet yet".into()) };
            let Some(l) = live.iter_mut().find(|l| l.contact.id == contact_id) else { return };
            match l.session.send_control(&serde_json::json!({ "t": "taoaddr", "a": addr })) {
                // Both sides of the chat should show it, not just theirs.
                Ok(key) => {
                    let wire_id = ulid::Ulid::new().to_string();
                    post_card(store, events, contact_id, &wire_id, true, &address_card(net(), &addr));
                    l.track(&wire_id, key);
                }
                Err(e) => fail(e),
            }
        }
        TaoCmd::Pay { contact_id, wire_id, rao, to, re } => {
            let Some(from) = w.active().filter(|a| a.can_sign()).cloned() else {
                return fail("no signing account (create or import one; watch-only can't pay)".into());
            };
            let Some(l) = live.iter().find(|l| l.contact.id == contact_id) else { return };
            let Some(to) = to.or_else(|| store.kv_get(&peer_key(l))) else {
                return fail("they haven't shared a TAO address; ask them to, or give one".into());
            };
            if let Err(e) = tao::parse_address(&to) {
                return fail(e.to_string());
            }
            let net = net();
            let mut card = PayCard {
                kind: PayKind::Paid,
                status: PayStatus::Sending,
                net,
                rao: rao.to_string(),
                from: from.address.clone(),
                to: to.clone(),
                tx: None,
                block: None,
                cheque: None,
                memo: None,
                re,
                note: None,
            };
            post_card(store, events, contact_id, &wire_id, true, &card);
            jobs.spawn(move || {
                match tao::send(net, &from, &to, rao) {
                    Ok(Receipt { tx, block }) => {
                        card.status = PayStatus::Sent;
                        card.tx = Some(tx);
                        card.block = Some(block);
                    }
                    Err(e) => {
                        card.status = PayStatus::Failed;
                        card.note = Some(tao::reason(&e));
                    }
                }
                let tell_peer = card.status == PayStatus::Sent;
                Done::Card { contact_id, wire_id, card, tell_peer }
            });
        }
        TaoCmd::Request { contact_id, wire_id, rao, memo } => {
            let Some(to) = w.active.clone() else { return fail("no wallet yet".into()) };
            let card = PayCard {
                kind: PayKind::Request,
                status: PayStatus::Open,
                net: net(),
                rao: rao.to_string(),
                from: String::new(),
                to,
                tx: None,
                block: None,
                cheque: None,
                memo: memo.map(|m| m.chars().take(140).collect()),
                re: None,
                note: None,
            };
            send_card(store, live, events, contact_id, &wire_id, &card);
        }
        TaoCmd::Cheque { contact_id, wire_id, rao } => {
            let Some(from) = w.active().filter(|a| a.can_sign()).cloned() else {
                return fail("no signing account".into());
            };
            let Some(l) = live.iter().find(|l| l.contact.id == contact_id) else { return };
            let Some(to) = store.kv_get(&peer_key(l)) else {
                return fail("they haven't shared a TAO address yet".into());
            };
            let net = net();
            // Signed from the last online snapshot: works with no network.
            let Some(snap) = store
                .kv_get(&snap_key(net, &from.address))
                .and_then(|s| serde_json::from_str::<Snapshot>(&s).ok())
            else {
                return fail("go online once (/tao refresh) so cheques can be signed offline".into());
            };
            let nonce = w
                .issued
                .iter()
                .filter(|c| c.net == net && c.from == from.address)
                .map(|c| c.nonce + 1)
                .max()
                .unwrap_or(0)
                .max(snap.nonce);
            let bytes = match tao::sign_cheque(&snap, &from, &to, rao, nonce) {
                Ok(b) => b,
                Err(e) => return fail(e.to_string()),
            };
            w.issued.push(Issued {
                net,
                from: from.address.clone(),
                nonce,
                bytes: tao::hex(&bytes),
                contact_id,
                wire_id: wire_id.clone(),
            });
            w.save(store);
            let card = PayCard {
                kind: PayKind::Cheque,
                status: PayStatus::Pending,
                net,
                rao: rao.to_string(),
                from: from.address,
                to,
                tx: None,
                block: None,
                cheque: Some(tao::hex(&bytes)),
                memo: None,
                re: None,
                note: None,
            };
            send_card(store, live, events, contact_id, &wire_id, &card);
        }
        TaoCmd::Cash { contact_id, wire_id } => {
            let Some(mut card) = card_of(store, contact_id, &wire_id) else { return };
            if card.kind != PayKind::Cheque || !matches!(card.status, PayStatus::Pending | PayStatus::Bounced) {
                return;
            }
            let Some(bytes) = card.cheque.as_deref().and_then(|c| tao::unhex(c).ok()) else { return };
            card.status = PayStatus::Cashing;
            update_card(store, events, contact_id, &wire_id, &card);
            jobs.spawn(move || {
                match tao::cash_cheque(card.net, &bytes) {
                    Ok(Receipt { tx, block }) => {
                        card.status = PayStatus::Cashed;
                        card.tx = Some(tx);
                        card.block = Some(block);
                    }
                    Err(e) => {
                        card.status = PayStatus::Bounced;
                        card.note = Some(tao::reason(&e));
                    }
                }
                let tell_peer = card.status == PayStatus::Cashed;
                Done::Card { contact_id, wire_id, card, tell_peer }
            });
        }
    }
}

/// Post our card locally and send it to the contact.
fn send_card(store: &Store, live: &mut [Live], events: &Sender<Event>, contact_id: i64, wire_id: &str, card: &PayCard) {
    let Some(l) = live.iter_mut().find(|l| l.contact.id == contact_id) else { return };
    match l.session.send_control(&serde_json::json!({ "t": "pay", "id": wire_id, "card": card })) {
        Ok(key) => {
            post_card(store, events, contact_id, wire_id, true, card);
            l.track(wire_id, key);
        }
        Err(e) => {
            let _ = events.send(Event::SendFailed { contact_id, wire_id: wire_id.into(), error: e });
        }
    }
}

/// A `taoaddr` or `pay` message from the peer.
pub(crate) fn handle_item(v: serde_json::Value, l: &mut Live, store: &Store, events: &Sender<Event>, jobs: &Jobs) {
    let cid = l.contact.id;
    match v.get("t").and_then(|t| t.as_str()) {
        Some("taoaddr") => {
            let Some(a) = v.get("a").and_then(|a| a.as_str()) else { return };
            if let Ok(acc) = tao::parse_address(a) {
                let a = tao::address_of(&acc);
                let _ = store.kv_set(&peer_key(l), &a);
                let wire_id = ulid::Ulid::new().to_string();
                post_card(store, events, cid, &wire_id, false, &address_card(net(), &a));
            }
        }
        Some("pay") => {
            let Some(id) = v.get("id").and_then(|x| x.as_str()).filter(|x| x.len() <= 64) else { return };
            let Ok(mut card) = serde_json::from_value::<PayCard>(v["card"].clone()) else { return };
            // Address cards only come from `taoaddr`, never as a peer's card.
            if card.kind == PayKind::Address || card.validate().is_err() {
                return;
            }
            let id = id.to_string();
            // An update to a card we already have (cheque cashed by them,
            // request paid): settle our copy from the chain, not their word.
            if card_of(store, cid, &id).is_some() {
                return settle_update(store, jobs, cid, id, card);
            }
            match card.kind {
                PayKind::Request => {
                    card.status = PayStatus::Open;
                    post_card(store, events, cid, &id, false, &card);
                }
                PayKind::Paid => {
                    card.status = PayStatus::Verifying;
                    post_card(store, events, cid, &id, false, &card);
                    verify_later(jobs, cid, id, card);
                }
                PayKind::Cheque => {
                    card.status = PayStatus::Pending;
                    post_card(store, events, cid, &id, false, &card);
                    // Re-read amount and payee from the signed bytes.
                    let my = Wallet::load(store).accounts.into_iter().map(|a| a.address).collect::<Vec<_>>();
                    jobs.spawn(move || check_cheque(cid, id, card, my));
                }
                PayKind::Address => {} // refused above
            }
        }
        _ => {}
    }
}

fn verify_later(jobs: &Jobs, contact_id: i64, wire_id: String, mut card: PayCard) {
    jobs.spawn(move || {
        let ok = match (&card.tx, &card.block) {
            (Some(tx), Some(block)) => {
                let r = Receipt { tx: tx.clone(), block: block.clone() };
                tao::verify(card.net, &r, &card.from, &card.to, card.amount()).unwrap_or(false)
            }
            _ => false,
        };
        card.status = if ok { PayStatus::Verified } else { PayStatus::Unverified };
        Done::Card { contact_id, wire_id, card, tell_peer: false }
    });
}

/// Decode a received cheque; the card shown is what the bytes say.
fn check_cheque(contact_id: i64, wire_id: String, mut card: PayCard, mine: Vec<String>) -> Done {
    let bytes = card.cheque.as_deref().and_then(|c| tao::unhex(c).ok());
    // Offline (no snapshot to decode with): stays pending, and the chain
    // checks it anyway when cashed.
    if let (Some(bytes), Ok(snap)) = (bytes, tao::snapshot(card.net, &card.from)) {
        match tao::inspect_cheque(&snap, &bytes) {
            Ok(c) => {
                card.from = c.from;
                card.to = c.to;
                card.rao = c.rao.to_string();
                if !mine.contains(&card.to) {
                    card.status = PayStatus::Bounced;
                    card.note = Some("not payable to any account in your wallet".into());
                }
            }
            Err(e) => {
                card.status = PayStatus::Bounced;
                card.note = Some(tao::reason(&e));
            }
        }
    }
    Done::Card { contact_id, wire_id, card, tell_peer: false }
}

/// The peer reports progress on a card we hold (they cashed our cheque, or
/// paid our request). Check it on chain before showing it.
fn settle_update(store: &Store, jobs: &Jobs, cid: i64, id: String, theirs: PayCard) {
    let Some(mut ours) = card_of(store, cid, &id) else { return };
    let (Some(tx), Some(block)) = (theirs.tx.clone(), theirs.block.clone()) else { return };
    let (net, from, to, rao) = (ours.net, ours.from.clone(), ours.to.clone(), ours.amount());
    jobs.spawn(move || {
        let r = Receipt { tx: tx.clone(), block: block.clone() };
        if tao::verify(net, &r, &from, &to, rao).unwrap_or(false) {
            ours.status = match ours.kind {
                PayKind::Cheque => PayStatus::Cashed,
                PayKind::Request => PayStatus::Settled,
                PayKind::Paid | PayKind::Address => ours.status,
            };
            ours.tx = Some(tx);
            ours.block = Some(block);
        }
        Done::Card { contact_id: cid, wire_id: id, card: ours, tell_peer: false }
    });
}

/// Apply finished chain work. Called every worker pass.
pub(crate) fn drain(store: &Store, live: &mut [Live], events: &Sender<Event>, jobs: &mut Jobs) {
    while let Ok(done) = jobs.rx.try_recv() {
        match done {
            Done::Balances(b, snap) => {
                jobs.balances = b;
                if let Some((net, addr, s)) = snap {
                    if let Ok(j) = serde_json::to_string(&s) {
                        let _ = store.kv_set(&snap_key(net, &addr), &j);
                    }
                    // Cheques at nonces the chain has passed are settled.
                    let mut w = Wallet::load(store);
                    w.issued.retain(|c| !(c.net == net && c.from == addr && c.nonce < s.nonce));
                    w.save(store);
                }
                let _ = events.send(Event::TaoWallet(view(store, jobs)));
            }
            Done::Card { contact_id, wire_id, card, tell_peer } => {
                update_card(store, events, contact_id, &wire_id, &card);
                if tell_peer {
                    if let Some(l) = live.iter_mut().find(|l| l.contact.id == contact_id) {
                        let sent = l.session.send_control(&serde_json::json!({
                            "t": "pay", "id": wire_id, "card": card,
                        }));
                        if let Ok(key) = sent {
                            l.track(&wire_id, key);
                        }
                    }
                }
                // A landed payment settles the request it answers: ours once
                // we sent it, theirs once we verified it on chain.
                if let (Some(re), PayStatus::Sent | PayStatus::Verified) = (card.re.as_deref(), card.status) {
                    if let Some(mut req) = card_of(store, contact_id, re).filter(|r| r.kind == PayKind::Request) {
                        req.status = PayStatus::Settled;
                        req.tx = card.tx.clone();
                        req.block = card.block.clone();
                        update_card(store, events, contact_id, re, &req);
                    }
                }
            }
            Done::Error(e) => {
                let _ = events.send(Event::Error(format!("tao: {e}")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> PayCard {
        let a = Account::generate("a").unwrap().address;
        PayCard {
            kind: PayKind::Paid,
            status: PayStatus::Sent,
            net: Net::Main,
            rao: "1500000000".into(),
            from: a.clone(),
            to: a,
            tx: None,
            block: None,
            cheque: None,
            memo: None,
            re: None,
            note: None,
        }
    }

    #[test]
    fn peer_cards_are_validated() {
        assert!(card().validate().is_ok());
        let mut c = card();
        c.rao = "0".into();
        assert!(c.validate().is_err());
        let mut c = card();
        c.rao = "99999999999999999999999999".into();
        assert!(c.validate().is_err(), "more TAO than will ever exist");
        let mut c = card();
        c.to = "15oF4uVJwmo4TdGW7VfQxNLavjCXviqxT9S1MgbjMNHr6Sp5".into();
        assert!(c.validate().is_err(), "other-chain address");
        let mut c = card();
        c.memo = Some("x".repeat(141));
        assert!(c.validate().is_err());
        assert_eq!(card().summary(), "τ Sent 1.5 τ");
    }
}
