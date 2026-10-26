//! TAO in chats: the wallet the worker keeps, payment cards, and the
//! `taoaddr` / `pay` control messages. Chain work (seconds to a minute) runs
//! on helper threads and reports back through `Jobs`, so chats never stall.
//!
//! Trust: a peer's card is a claim. "Paid" is shown only once the transfer
//! is verified on chain; a cheque's amount and payee come from its signed
//! bytes; addresses, amounts and sizes are validated before anything else.

use std::sync::mpsc::{channel, Receiver, Sender};

use subxt::utils::AccountId32;

use serde::{Deserialize, Serialize};

use crate::contact::{Event, Live};
use crate::store::{Store, StoredMsg};
use crate::stealth::{self, StealthKeys};
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
    /// One-time addresses paid to us privately: spendable with the owner
    /// account's spend key and the tweak.
    #[serde(default)]
    notes: Vec<Note>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Note {
    /// The account whose stealth keys own it.
    owner: String,
    address: String,
    /// Chain code, 0x-hex.
    tweak: String,
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

/// Their private (stealth) address, if they shared one.
fn peer_meta_key(l: &Live) -> String {
    format!("tao/peermeta/{}", l.contact.bucket_in)
}

fn private_mode() -> bool {
    crate::demo::Settings::load().private_payments
}

/// A plain SS58 address or a private `st1…` one.
fn check_payee(to: &str) -> Result<(), String> {
    if to.starts_with(stealth::META_PREFIX) {
        stealth::Meta::parse(to).map(|_| ()).map_err(|e| e.to_string())
    } else {
        tao::parse_address(to).map(|_| ()).map_err(|e| e.to_string())
    }
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
    /// A private payment's parts, one transfer each (empty otherwise).
    #[serde(default)]
    pub legs: Vec<Leg>,
}

/// One transfer of a private payment: from one of the payer's addresses to a
/// one-time address of the payee (index `k` derives it).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Leg {
    pub from: String,
    pub to: String,
    pub k: u64,
    pub rao: String,
    pub tx: String,
    pub block: String,
}

/// Most parts a private payment is split into.
const MAX_LEGS: usize = 8;

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
        let private = if self.legs.len() > 1 {
            format!(" · private, {} parts", self.legs.len())
        } else if !self.legs.is_empty() {
            " · private".into()
        } else {
            String::new()
        };
        let s = match (self.kind, self.status) {
            (PayKind::Address, _) if self.to.starts_with(stealth::META_PREFIX) => {
                format!("Private TAO address: {}", self.to)
            }
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
        format!("τ {s}{private}{net}")
    }

    /// Everything a peer's card claims, checked before we act on it.
    fn validate(&self) -> Result<(), String> {
        let rao: u128 = self.rao.parse().map_err(|_| "bad amount")?;
        if rao == 0 || rao > 21_000_000 * tao::RAO_PER_TAO {
            return Err("amount out of range".into());
        }
        check_payee(&self.to)?;
        if !self.from.is_empty() {
            tao::parse_address(&self.from).map_err(|e| e.to_string())?;
        }
        if self.legs.len() > MAX_LEGS {
            return Err("too many parts".into());
        }
        let mut sum = 0u128;
        for leg in &self.legs {
            tao::parse_address(&leg.from).map_err(|e| e.to_string())?;
            tao::parse_address(&leg.to).map_err(|e| e.to_string())?;
            sum += leg.rao.parse::<u128>().map_err(|_| "bad amount")?;
            if leg.tx.len() > 80 || leg.block.len() > 80 {
                return Err("oversized field".into());
            }
        }
        if !self.legs.is_empty() && sum != rao {
            return Err("parts don't add up".into());
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
    /// Open the Hippius mailbox account: a new one from a fresh phrase, or
    /// sign in with `phrase` (restore on a new device).
    MailboxOpen { phrase: Option<String> },
    /// Show the mailbox account's recovery phrase (explicit user action only).
    MailboxReveal,
    /// Buy FUND_USD of credits with TAO from the active account (mainnet).
    MailboxFund,
}

#[derive(Clone, Debug)]
pub struct AccountView {
    pub name: String,
    pub address: String,
    pub watch_only: bool,
    /// None while unknown (offline or not fetched yet).
    pub balance: Option<u128>,
    /// Held in one-time addresses received privately.
    pub private_balance: Option<u128>,
    /// Its private address (`st1…`); None for watch-only.
    pub meta: Option<String>,
}

/// The Hippius account that pays for the mailboxes we create.
#[derive(Clone, Debug, Default)]
pub struct MailboxView {
    /// Account address when we hold its phrase; None if connected by a
    /// pasted API token instead.
    pub address: Option<String>,
    /// Credits in USD; None while unknown.
    pub credits: Option<f64>,
    /// Where TAO becomes credits.
    pub deposit: Option<String>,
    /// TAO for a FUND_USD top-up at today's price (with margin).
    pub fund_rao: Option<u128>,
    /// Opening the account or waiting on a top-up.
    pub busy: bool,
    pub note: Option<String>,
}

#[derive(Clone, Debug)]
pub struct WalletView {
    pub net: Net,
    pub active: Option<String>,
    pub accounts: Vec<AccountView>,
    /// None: no mailbox account yet (starting chats needs one; joining doesn't).
    pub mailbox: Option<MailboxView>,
    /// Address the selected contact shared, if any (per contact, filled by UI asks).
    pub note: Option<String>,
}

/// Results coming back from helper threads.
pub enum Done {
    Balances(Vec<(String, Option<u128>)>, Option<(Net, String, Snapshot)>),
    MailboxOpened { phrase: String, address: String, token: String },
    MailboxInfo { credits: Option<f64>, deposit: Option<String>, tao_usd: Option<f64>, last_check: bool },
    /// A top-up left the wallet; credits follow within minutes.
    Funded { tao: String },
    MailboxFailed(String),
    Card { contact_id: i64, wire_id: String, card: PayCard, tell_peer: bool },
    /// A private payment we received and checked: its one-time addresses.
    Received { contact_id: i64, wire_id: String, card: PayCard, notes: Vec<(String, String, String)> },
    Error(String),
}

pub struct Jobs {
    tx: Sender<Done>,
    rx: Receiver<Done>,
    /// Last known balances, by address.
    balances: Vec<(String, Option<u128>)>,
    mailbox: MailboxView,
    tao_usd: Option<f64>,
    /// Private addresses by account (deriving them takes a key stretch).
    metas: std::cell::RefCell<std::collections::HashMap<String, String>>,
}

impl Jobs {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, balances: Vec::new(), mailbox: MailboxView::default(), tao_usd: None, metas: Default::default() }
    }

    fn meta_of(&self, a: &Account) -> Option<String> {
        let phrase = a.phrase.as_deref()?;
        let mut metas = self.metas.borrow_mut();
        if let Some(m) = metas.get(&a.address) {
            return Some(m.clone());
        }
        let m = StealthKeys::of(phrase).ok()?.meta().encode();
        metas.insert(a.address.clone(), m.clone());
        Some(m)
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
            .map(|a| {
                let bal = |addr: &str| jobs.balances.iter().find(|(x, _)| x == addr).and_then(|(_, b)| *b);
                let notes: Vec<_> = w.notes.iter().filter(|n| n.owner == a.address).collect();
                AccountView {
                    name: a.name.clone(),
                    address: a.address.clone(),
                    watch_only: !a.can_sign(),
                    balance: bal(&a.address),
                    private_balance: (!notes.is_empty())
                        .then(|| notes.iter().map(|n| bal(&n.address)).sum::<Option<u128>>())
                        .flatten(),
                    meta: jobs.meta_of(a),
                }
            })
            .collect(),
        note: None,
        mailbox: (Mailbox::load(store).is_some() || has_token() || jobs.mailbox.busy).then(|| MailboxView {
            address: Mailbox::load(store).map(|m| m.address),
            fund_rao: jobs.tao_usd.map(fund_rao),
            ..jobs.mailbox.clone()
        }),
    }
}

// ---------------------------------------------------------------------------
// Mailbox account: the Hippius account a 12-word phrase controls. Its own
// phrase, not a TAO wallet one, so Hippius can't tie mailboxes to the
// addresses we pay contacts from.
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct Mailbox {
    phrase: String,
    address: String,
}

const MAILBOX_KEY: &str = "hippius/account";

/// Hippius's smallest TAO top-up.
pub const FUND_USD: f64 = 20.0;

impl Mailbox {
    fn load(store: &Store) -> Option<Self> {
        store.kv_get(MAILBOX_KEY).and_then(|s| serde_json::from_str(&s).ok())
    }
}

/// What survives a "keep wallets" wipe: the TAO accounts, cheques still in
/// flight (they hold nonces later sends must not reuse) minus which chat
/// they went to, and the mailbox account. Nothing that says who you talked to.
pub(crate) struct Money {
    wallet: Option<String>,
    mailbox: Option<String>,
}

pub(crate) fn take_money(store: &Store) -> Money {
    let mut w = Wallet::load(store);
    for c in &mut w.issued {
        c.contact_id = 0;
        c.wire_id.clear();
    }
    Money {
        wallet: (!w.accounts.is_empty()).then(|| serde_json::to_string(&w).ok()).flatten(),
        mailbox: store.kv_get(MAILBOX_KEY),
    }
}

pub(crate) fn put_money(store: &Store, m: &Money) -> Result<(), crate::Error> {
    if let Some(w) = &m.wallet {
        store.kv_set(WALLET_KEY, w)?;
    }
    if let Some(b) = &m.mailbox {
        store.kv_set(MAILBOX_KEY, b)?;
    }
    Ok(())
}

fn has_token() -> bool {
    crate::demo::Settings::load().hippius_api_token.is_some()
}

/// TAO for FUND_USD, 3% over so it still clears the minimum if the price
/// moves before the deposit is credited.
fn fund_rao(tao_usd: f64) -> u128 {
    (FUND_USD * 1.03 / tao_usd * tao::RAO_PER_TAO as f64).ceil() as u128
}

fn refresh_mailbox(store: &Store, jobs: &Jobs) {
    let account = Mailbox::load(store).map(|m| m.address);
    let Some(p) = crate::provision::Provisioner::from_settings() else { return };
    jobs.spawn(move || Done::MailboxInfo {
        credits: p.credits().ok(),
        deposit: p.deposit_address(account.as_deref()).ok(),
        tao_usd: crate::provision::tao_usd().ok(),
        last_check: false,
    });
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
        legs: Vec::new(),
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

pub(crate) fn handle_cmd(cmd: TaoCmd, store: &Store, live: &mut [Live], events: &Sender<Event>, jobs: &mut Jobs) {
    let fail = |e: String| {
        let _ = events.send(Event::Error(format!("tao: {e}")));
    };
    let mut w = Wallet::load(store);
    match cmd {
        TaoCmd::Refresh => {
            let accounts: Vec<String> =
                w.accounts.iter().map(|a| a.address.clone()).chain(w.notes.iter().map(|n| n.address.clone())).collect();
            let signer = w.active().filter(|a| a.can_sign()).map(|a| a.address.clone());
            let issued = w.issued.clone();
            let net = net();
            let _ = events.send(Event::TaoWallet(view(store, jobs)));
            refresh_mailbox(store, jobs);
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
            let Some(acc) = w.active().cloned() else { return fail("no wallet yet".into()) };
            let Some(l) = live.iter_mut().find(|l| l.contact.id == contact_id) else { return };
            // Private: the stealth address, so they never learn an address
            // they could look up.
            let (addr, msg) = match jobs.meta_of(&acc).filter(|_| private_mode()) {
                Some(m) => (m.clone(), serde_json::json!({ "t": "taoaddr", "m": m })),
                None => (acc.address.clone(), serde_json::json!({ "t": "taoaddr", "a": acc.address })),
            };
            match l.session.send_control(&msg) {
                // Both sides of the chat should show it, not just theirs.
                Ok(key) => {
                    let wire_id = ulid::Ulid::new().to_string();
                    post_card(store, events, contact_id, &wire_id, true, &address_card(net(), &addr));
                    l.track(&wire_id, key);
                }
                Err(e) => fail(e),
            }
        }
        TaoCmd::MailboxOpen { phrase } => {
            // A new account never replaces one we have (pasted token included).
            if jobs.mailbox.busy || (phrase.is_none() && (Mailbox::load(store).is_some() || has_token())) {
                return;
            }
            jobs.mailbox = MailboxView { busy: true, note: Some("Setting up your account…".into()), ..Default::default() };
            let _ = events.send(Event::TaoWallet(view(store, jobs)));
            let phrase = phrase.unwrap_or_else(crate::provision::new_phrase);
            jobs.spawn(move || {
                let opened = crate::provision::sign_in(&phrase).and_then(|(token, address)| {
                    let p = crate::provision::Provisioner::new(token.clone())
                        .ok_or_else(|| crate::Error::Transport("no network agent".into()))?;
                    p.ensure_master_token()?;
                    Ok((token, address))
                });
                match opened {
                    Ok((token, address)) => Done::MailboxOpened { phrase, address, token },
                    Err(e) => Done::MailboxFailed(e.to_string()),
                }
            });
        }
        TaoCmd::MailboxReveal => {
            if let Some(m) = Mailbox::load(store) {
                let _ = events.send(Event::TaoPhrase { address: m.address, phrase: m.phrase });
            }
        }
        TaoCmd::MailboxFund => {
            let Some(from) = w.active().filter(|a| a.can_sign()).cloned() else {
                return fail("no signing account in the TAO wallet (create or import one)".into());
            };
            let (Some(deposit), Some(price)) = (jobs.mailbox.deposit.clone(), jobs.tao_usd) else {
                return fail("couldn't reach Hippius for the deposit address; try again".into());
            };
            if jobs.mailbox.busy {
                return;
            }
            let rao = fund_rao(price);
            jobs.mailbox.busy = true;
            jobs.mailbox.note = Some(format!("Sending {}…", tao::format_tao(rao)));
            let _ = events.send(Event::TaoWallet(view(store, jobs)));
            // Hippius credits mainnet TAO, whatever network the wallet shows.
            jobs.spawn(move || match tao::send(Net::Main, &from, &deposit, rao) {
                Ok(_) => Done::Funded { tao: tao::format_tao(rao) },
                Err(e) => Done::MailboxFailed(tao::reason(&e)),
            });
        }
        TaoCmd::Pay { contact_id, wire_id, rao, to, re } => {
            let Some(from) = w.active().filter(|a| a.can_sign()).cloned() else {
                return fail("no signing account (create or import one; watch-only can't pay)".into());
            };
            let Some(l) = live.iter().find(|l| l.contact.id == contact_id) else { return };
            let private = private_mode();
            let shared = || private.then(|| store.kv_get(&peer_meta_key(l))).flatten().or_else(|| store.kv_get(&peer_key(l)));
            let Some(to) = to.or_else(shared) else {
                return fail("they haven't shared a TAO address; ask them to, or give one".into());
            };
            if let Err(e) = check_payee(&to) {
                return fail(e);
            }
            let net = net();
            // To a private address, or in private mode: notes, split parts,
            // MEV Shield and jitter.
            if private || to.starts_with(stealth::META_PREFIX) {
                let card = PayCard {
                    kind: PayKind::Paid,
                    status: PayStatus::Sending,
                    net,
                    rao: rao.to_string(),
                    from: String::new(),
                    to: to.clone(),
                    tx: None,
                    block: None,
                    cheque: None,
                    memo: None,
                    re,
                    note: None,
                    legs: Vec::new(),
                };
                post_card(store, events, contact_id, &wire_id, true, &card);
                let payer = Payer {
                    phrase: from.phrase.clone().unwrap_or_default(),
                    notes: w
                        .notes
                        .iter()
                        .filter(|n| n.owner == from.address)
                        .filter_map(|n| tao::unhex(&n.tweak).ok()?.try_into().ok())
                        .collect(),
                };
                return jobs.spawn(move || pay_privately(net, payer, contact_id, wire_id, card));
            }
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
                legs: Vec::new(),
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
            let Some(acc) = w.active().cloned() else { return fail("no wallet yet".into()) };
            let to = jobs.meta_of(&acc).filter(|_| private_mode()).unwrap_or(acc.address);
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
                legs: Vec::new(),
            };
            send_card(store, live, events, contact_id, &wire_id, &card);
        }
        TaoCmd::Cheque { contact_id, wire_id, rao } => {
            let Some(from) = w.active().filter(|a| a.can_sign()).cloned() else {
                return fail("no signing account".into());
            };
            let Some(l) = live.iter().find(|l| l.contact.id == contact_id) else { return };
            let Some(to) = store.kv_get(&peer_key(l)) else {
                return fail("cheques need their plain TAO address; with a private address, send a payment instead".into());
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
                legs: Vec::new(),
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
            if let Some(m) = v.get("m").and_then(|m| m.as_str()).filter(|m| stealth::Meta::parse(m).is_ok()) {
                let _ = store.kv_set(&peer_meta_key(l), m);
                let wire_id = ulid::Ulid::new().to_string();
                post_card(store, events, cid, &wire_id, false, &address_card(net(), m));
                return;
            }
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
                PayKind::Paid if !card.legs.is_empty() => {
                    card.status = PayStatus::Verifying;
                    post_card(store, events, cid, &id, false, &card);
                    let accounts = Wallet::load(store).accounts.into_iter().map(|a| (a.address, a.phrase)).collect();
                    jobs.spawn(move || check_private(cid, id, card, accounts));
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

/// What a private payment can spend from: the account's own key and the
/// tweaks of its notes.
struct Payer {
    phrase: String,
    notes: Vec<[u8; 32]>,
}

/// Smallest part worth sending: twice what an account must hold to exist.
const MIN_LEG: u128 = 2 * tao::EXISTENTIAL_RAO;

/// Which sources pay how much: the smallest that covers it all, else the
/// fewest largest. None if they can't cover it. Every part is at least
/// MIN_LEG (sources below that are left out by the caller).
fn plan_legs(spendable: &[u128], total: u128) -> Option<Vec<(usize, u128)>> {
    let mut idx: Vec<usize> = (0..spendable.len()).collect();
    if let Some(&i) = idx.iter().filter(|&&i| spendable[i] >= total).min_by_key(|&&i| spendable[i]) {
        return Some(vec![(i, total)]);
    }
    idx.sort_by_key(|&i| std::cmp::Reverse(spendable[i]));
    let (mut legs, mut left) = (Vec::new(), total);
    for i in idx.into_iter().take(MAX_LEGS) {
        let x = spendable[i].min(left);
        legs.push((i, x));
        left -= x;
        if left == 0 {
            break;
        }
    }
    if left > 0 {
        return None;
    }
    // A sliver for the last part: move some over from the first.
    let last = legs.len() - 1;
    if legs[last].1 < MIN_LEG {
        let d = MIN_LEG - legs[last].1;
        if legs[0].1 < d + MIN_LEG {
            return None;
        }
        legs[0].1 -= d;
        legs[last].1 += d;
    }
    Some(legs)
}

/// A random pause before each part, so parts and chat messages don't line
/// up in time: a few seconds before the first, longer between the rest.
fn jitter(part: usize) {
    let secs = if part == 0 { rand::random_range(3..=20) } else { rand::random_range(20..=90) };
    std::thread::sleep(std::time::Duration::from_secs(secs));
}

/// Pay privately: plan the parts, then send each from its own address to
/// its own one-time address of the payee, shielded, after a random pause.
/// Notes come before the account's own (often public) address.
fn pay_privately(net: Net, payer: Payer, contact_id: i64, wire_id: String, mut card: PayCard) -> Done {
    let total = card.amount();
    let mut legs = Vec::new();
    let outcome = (|| -> Result<(), String> {
        let why = |e: crate::Error| tao::reason(&e);
        let main = stealth::account_key(&payer.phrase).map_err(why)?;
        let keys = StealthKeys::of(&payer.phrase).map_err(why)?;
        let fee = tao::fee_reserve(net, &main).map_err(why)?;
        let mut sources = Vec::new();
        for kp in payer.notes.iter().map(|t| keys.note_key(*t)).chain([main]) {
            let left = tao::balance_of(net, &AccountId32(kp.public.to_bytes())).map_err(why)?;
            let spendable = left.saturating_sub(fee + tao::EXISTENTIAL_RAO);
            if spendable >= MIN_LEG {
                sources.push((kp, spendable));
            }
        }
        let amounts: Vec<u128> = sources.iter().map(|(_, x)| *x).collect();
        let plan = plan_legs(&amounts, total).ok_or("not enough TAO across your addresses for this plus fees")?;
        let meta = match card.to.starts_with(stealth::META_PREFIX) {
            true => Some(stealth::Meta::parse(&card.to).map_err(why)?),
            false => None,
        };
        for (part, (i, amount)) in plan.into_iter().enumerate() {
            jitter(part);
            let kp = &sources[i].0;
            let from = AccountId32(kp.public.to_bytes());
            // Its nonce: unique per payment from this address, so no
            // one-time address repeats.
            let k = tao::nonce(net, &from).map_err(why)?;
            let to = match &meta {
                Some(m) => m.one_time(kp, k),
                None => tao::parse_address(&card.to).map_err(why)?,
            };
            let (r, shielded) = tao::send_shielded(net, kp, &to, amount).map_err(why)?;
            if !shielded {
                card.note = Some("a part went without MEV Shield (its key rotated)".into());
            }
            legs.push(Leg {
                from: tao::address_of(&from),
                to: tao::address_of(&to),
                k,
                rao: amount.to_string(),
                tx: r.tx,
                block: r.block,
            });
        }
        Ok(())
    })();
    let sent: u128 = legs.iter().map(|l| l.rao.parse::<u128>().unwrap_or(0)).sum();
    if let Some(first) = legs.first() {
        card.tx = Some(first.tx.clone());
        card.block = Some(first.block.clone());
    }
    card.legs = legs;
    match outcome {
        Ok(()) => card.status = PayStatus::Sent,
        Err(why) if sent > 0 => {
            card.status = PayStatus::Failed;
            card.note = Some(format!("only {} of {} went through: {why}", tao::format_tao(sent), tao::format_tao(total)));
            card.rao = sent.to_string(); // what they can verify
        }
        Err(why) => {
            card.status = PayStatus::Failed;
            card.note = Some(why);
        }
    }
    let tell_peer = !card.legs.is_empty();
    Done::Card { contact_id, wire_id, card, tell_peer }
}

/// Check a private payment to us: every part must go to a one-time address
/// we derive ourselves (never trusting the card) and be on chain as
/// claimed. Its notes are kept only if all of it checks out.
fn check_private(contact_id: i64, wire_id: String, mut card: PayCard, accounts: Vec<(String, Option<String>)>) -> Done {
    let mut notes = Vec::new();
    let ok = (|| {
        let owner = match card.to.starts_with(stealth::META_PREFIX) {
            true => Some(accounts.iter().find_map(|(a, phrase)| {
                let keys = StealthKeys::of(phrase.as_deref()?).ok()?;
                (keys.meta().encode() == card.to).then(|| (a.clone(), keys))
            })?),
            false => None,
        };
        for leg in &card.legs {
            let (from, to) = (tao::parse_address(&leg.from).ok()?, tao::parse_address(&leg.to).ok()?);
            match &owner {
                Some((acc, keys)) => {
                    let tweak = keys.claim(&from, leg.k, &to)?;
                    notes.push((acc.clone(), leg.to.clone(), tao::hex(&tweak)));
                }
                None if accounts.iter().any(|(a, _)| *a == leg.to) => {}
                None => return None,
            }
            let r = Receipt { tx: leg.tx.clone(), block: leg.block.clone() };
            if !tao::verify(card.net, &r, &leg.from, &leg.to, leg.rao.parse().ok()?).unwrap_or(false) {
                return None;
            }
        }
        Some(())
    })()
    .is_some();
    card.status = if ok { PayStatus::Verified } else { PayStatus::Unverified };
    if !ok {
        notes.clear();
    }
    Done::Received { contact_id, wire_id, card, notes }
}

/// A payment that landed settles the request it answers.
fn settle_request(store: &Store, events: &Sender<Event>, contact_id: i64, card: &PayCard) {
    if let (Some(re), PayStatus::Sent | PayStatus::Verified) = (card.re.as_deref(), card.status) {
        if let Some(mut req) = card_of(store, contact_id, re).filter(|r| r.kind == PayKind::Request) {
            req.status = PayStatus::Settled;
            req.tx = card.tx.clone();
            req.block = card.block.clone();
            update_card(store, events, contact_id, re, &req);
        }
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
    // A private payment settling our request: its own card carries the
    // parts, and arrives (and is checked) as a payment.
    if !theirs.legs.is_empty() {
        return;
    }
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
                settle_request(store, events, contact_id, &card);
            }
            Done::Received { contact_id, wire_id, card, notes } => {
                update_card(store, events, contact_id, &wire_id, &card);
                settle_request(store, events, contact_id, &card);
                if !notes.is_empty() {
                    let mut w = Wallet::load(store);
                    for (owner, address, tweak) in notes {
                        if !w.notes.iter().any(|n| n.address == address) {
                            w.notes.push(Note { owner, address, tweak });
                        }
                    }
                    w.save(store);
                    handle_cmd(TaoCmd::Refresh, store, live, events, jobs);
                }
            }
            Done::MailboxOpened { phrase, address, token } => {
                let m = Mailbox { phrase, address };
                if let Ok(j) = serde_json::to_string(&m) {
                    let _ = store.kv_set(MAILBOX_KEY, &j);
                }
                let mut s = crate::demo::Settings::load();
                s.hippius_api_token = Some(token);
                s.save();
                jobs.mailbox = MailboxView::default();
                let _ = events.send(Event::TaoWallet(view(store, jobs)));
                refresh_mailbox(store, jobs);
            }
            Done::MailboxInfo { credits, deposit, tao_usd, last_check } => {
                // A top-up we're waiting on has landed once credits rise.
                let landed = matches!((jobs.mailbox.credits, credits), (Some(a), Some(b)) if b > a);
                if landed && jobs.mailbox.note.is_some() {
                    jobs.mailbox.busy = false;
                    jobs.mailbox.note = None;
                } else if last_check {
                    jobs.mailbox.busy = false;
                    jobs.mailbox.note = Some("Credits haven't shown up yet. Deposits can take a while; check back soon.".into());
                }
                jobs.mailbox.credits = credits.or(jobs.mailbox.credits);
                jobs.mailbox.deposit = deposit.or(jobs.mailbox.deposit.take());
                jobs.tao_usd = tao_usd.or(jobs.tao_usd);
                let _ = events.send(Event::TaoWallet(view(store, jobs)));
            }
            Done::Funded { tao } => {
                jobs.mailbox.note = Some(format!("Sent {tao}. Credits usually arrive within a few minutes."));
                let _ = events.send(Event::TaoWallet(view(store, jobs)));
                // Watch for the credits for up to ~10 minutes.
                let Some(p) = crate::provision::Provisioner::from_settings() else { continue };
                let before = jobs.mailbox.credits.unwrap_or(0.0);
                jobs.spawn(move || {
                    let mut credits = None;
                    for _ in 0..20 {
                        std::thread::sleep(std::time::Duration::from_secs(30));
                        credits = p.credits().ok();
                        if credits.is_some_and(|c| c > before) {
                            break;
                        }
                    }
                    Done::MailboxInfo { credits, deposit: None, tao_usd: None, last_check: true }
                });
            }
            Done::MailboxFailed(e) => {
                jobs.mailbox.busy = false;
                jobs.mailbox.note = None;
                let _ = events.send(Event::TaoWallet(view(store, jobs)));
                let _ = events.send(Event::Error(format!("mailbox: {e}")));
            }
            Done::Error(e) => {
                let _ = events.send(Event::Error(format!("tao: {e}")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn plan_prefers_one_part_then_fewest_largest() {
        let m = super::MIN_LEG;
        // The smallest single source that covers it.
        assert_eq!(super::plan_legs(&[50 * m, 9 * m, 10 * m], 8 * m), Some(vec![(1, 8 * m)]));
        // Else the largest first, fewest parts.
        assert_eq!(super::plan_legs(&[3 * m, 5 * m, 4 * m], 8 * m), Some(vec![(1, 5 * m), (2, 3 * m)]));
        assert_eq!(super::plan_legs(&[3 * m, 4 * m], 8 * m), None);
        // A sliver of a last part is topped up from the first.
        let plan = super::plan_legs(&[5 * m, 5 * m], 10 * m - m / 2).unwrap();
        assert!(plan.iter().all(|&(_, x)| x >= m));
        assert_eq!(plan.iter().map(|&(_, x)| x).sum::<u128>(), 10 * m - m / 2);
    }

    #[test]
    fn private_cards_must_add_up() {
        let mut c = card();
        c.kind = PayKind::Paid;
        c.to = crate::stealth::StealthKeys::of("bottom drive obey lake curtain smoke basket hold race lonely fit walk")
            .unwrap()
            .meta()
            .encode();
        let leg = |rao: &str| Leg { from: c.from.clone(), to: c.from.clone(), k: 0, rao: rao.into(), tx: "0x".into(), block: "0x".into() };
        c.legs = vec![leg("1000000000"), leg("500000000")];
        c.rao = "1500000000".into();
        assert!(c.validate().is_ok(), "{:?}", c.validate());
        c.rao = "2000000000".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn fund_amount_clears_the_minimum() {
        // $20 + 3% at $250/TAO = 0.0824 TAO.
        assert_eq!(super::fund_rao(250.0), 82_400_000);
        assert!(super::fund_rao(247.99) as f64 / 1e9 * 247.99 >= super::FUND_USD);
    }

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
            legs: Vec::new(),
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
