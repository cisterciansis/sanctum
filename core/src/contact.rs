//! Real per-contact sessions (SPEC-V2 §4b): every contact is a pair of
//! freshly-minted queue buckets plus a capability token. The inviter mints
//! and pays (~cents); the invitee needs no account, ever — their capability
//! arrives inside the invite link.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use serde::{Deserialize, Serialize};

use crate::crypto_box::PublicKey;
use crate::provision::Provisioner;
use crate::store::{Contact, Store, StoredMsg};
use crate::transport::Mailbox;
use crate::{open, seal, Identity};

pub const SCHEME_V2: &str = "sanctum://v2/";

/// Everything the invitee needs: their two queues, a capability over both,
/// and the inviter's public key.
#[derive(Serialize, Deserialize, Clone)]
pub struct Invite2 {
    /// Bucket the acceptor writes into (inviter polls).
    pub to_inviter: String,
    /// Bucket the acceptor polls (inviter writes into).
    pub to_acceptor: String,
    pub k: String,
    pub s: String,
    pub pk: String,
    pub e: String,
}

impl Invite2 {
    pub fn to_link(&self) -> String {
        format!("{SCHEME_V2}{}", B64.encode(serde_json::to_vec(self).expect("invite serializes")))
    }

    pub fn from_link(link: &str) -> Result<Self, String> {
        let payload = link
            .trim()
            .strip_prefix(SCHEME_V2)
            .ok_or("not a sanctum v2 invite")?;
        let json = B64.decode(payload).map_err(|e| e.to_string())?;
        serde_json::from_slice(&json).map_err(|e| e.to_string())
    }
}

/// Inviter side: mint two queue buckets and two capabilities (one stays
/// local, one ships in the invite). Returns the contact row + the link.
pub fn create_invite(p: &Provisioner, me: &Identity, name: &str) -> Result<(Contact, String), String> {
    let pair = ulid::Ulid::new().to_string().to_lowercase();
    let to_inviter = format!("q-{pair}-a");
    let to_acceptor = format!("q-{pair}-b");
    p.create_bucket(&to_inviter).map_err(|e| e.to_string())?;
    p.create_bucket(&to_acceptor).map_err(|e| e.to_string())?;
    let (my_key, my_secret) = p
        .create_sub_token_multi(&[&to_inviter, &to_acceptor], "sanctum-self")
        .map_err(|e| e.to_string())?;
    let (peer_key, peer_secret) = p
        .create_sub_token_multi(&[&to_inviter, &to_acceptor], "sanctum-invite")
        .map_err(|e| e.to_string())?;
    let invite = Invite2 {
        to_inviter: to_inviter.clone(),
        to_acceptor: to_acceptor.clone(),
        k: peer_key,
        s: peer_secret,
        pk: B64.encode(me.public().as_bytes()),
        e: crate::transport::DEFAULT_ENDPOINT.into(),
    };
    let link = invite.to_link();
    let contact = Contact {
        id: 0,
        name: name.into(),
        bucket_in: to_inviter,
        bucket_out: to_acceptor,
        access_key: my_key,
        secret: my_secret,
        peer_pk: String::new(), // until their hello lands
        endpoint: crate::transport::DEFAULT_ENDPOINT.into(),
        invite_link: link.clone(),
    };
    Ok((contact, link))
}

/// Acceptor side: adopt the capabilities and announce our public key.
pub fn accept_invite(link: &str, me: &Identity, name: &str) -> Result<Contact, String> {
    let inv = Invite2::from_link(link)?;
    let contact = Contact {
        id: 0,
        name: name.into(),
        bucket_in: inv.to_acceptor.clone(),
        bucket_out: inv.to_inviter.clone(),
        access_key: inv.k.clone(),
        secret: inv.s.clone(),
        peer_pk: inv.pk.clone(),
        endpoint: inv.e.clone(),
        invite_link: String::new(),
    };
    let session = ContactSession::new(&contact, me)?;
    session.deposit(&serde_json::json!({
        "t": "hello",
        "pk": B64.encode(me.public().as_bytes()),
    }))?;
    Ok(contact)
}

/// One live conversation: poll bucket_in, write bucket_out.
pub struct ContactSession {
    me_secret: String,
    peer_pk: Option<PublicKey>,
    mbox_in: Mailbox,
    mbox_out: Mailbox,
}

pub enum ChatItem {
    Hello { pk: String },
    Message { id: String, text: String },
    Attachment { id: String, name: String, att_key: String },
    Reaction { ref_id: String, emoji: String },
    Timer { secs: u64 },
}

impl ContactSession {
    pub fn new(c: &Contact, me: &Identity) -> Result<Self, String> {
        let peer_pk = if c.peer_pk.is_empty() {
            None
        } else {
            let bytes: [u8; 32] = B64
                .decode(&c.peer_pk)
                .map_err(|e| e.to_string())?
                .try_into()
                .map_err(|_| "bad peer pk")?;
            Some(PublicKey::from_bytes(bytes))
        };
        Ok(Self {
            me_secret: me.to_b64(),
            peer_pk,
            mbox_in: Mailbox::new(&c.endpoint, &c.bucket_in, &c.access_key, &c.secret)
                .map_err(|e| e.to_string())?,
            mbox_out: Mailbox::new(&c.endpoint, &c.bucket_out, &c.access_key, &c.secret)
                .map_err(|e| e.to_string())?,
        })
    }

    fn me(&self) -> Identity {
        Identity::from_b64(&self.me_secret).expect("identity round-trips")
    }

    pub fn set_peer_pk(&mut self, pk_b64: &str) -> Result<(), String> {
        let bytes: [u8; 32] = B64
            .decode(pk_b64)
            .map_err(|e| e.to_string())?
            .try_into()
            .map_err(|_| "bad peer pk")?;
        self.peer_pk = Some(PublicKey::from_bytes(bytes));
        Ok(())
    }

    pub fn ready(&self) -> bool {
        self.peer_pk.is_some()
    }

    fn deposit(&self, body: &serde_json::Value) -> Result<String, String> {
        let pk = self.peer_pk.as_ref().ok_or("contact hasn't joined yet")?;
        let sealed = seal(&self.me(), pk, body.to_string().as_bytes()).map_err(|e| e.to_string())?;
        let key = format!("m/{}", ulid::Ulid::new());
        self.mbox_out.put(&key, &sealed).map_err(|e| e.to_string())?;
        Ok(key)
    }

    pub fn send(&self, wire_id: &str, text: &str) -> Result<String, String> {
        self.deposit(&serde_json::json!({ "t": "m", "id": wire_id, "x": text }))
    }

    pub fn react(&self, ref_id: &str, emoji: &str) -> Result<(), String> {
        self.deposit(&serde_json::json!({ "t": "r", "ref": ref_id, "e": emoji }))?;
        Ok(())
    }

    pub fn set_timer(&self, secs: u64) -> Result<(), String> {
        self.deposit(&serde_json::json!({ "t": "tmr", "s": secs }))?;
        Ok(())
    }

    pub fn send_file(&self, wire_id: &str, path: &str) -> Result<(String, String), String> {
        let bytes = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
        if bytes.len() > 25 * 1024 * 1024 {
            return Err("file larger than 25 MB".into());
        }
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let pk = self.peer_pk.as_ref().ok_or("contact hasn't joined yet")?;
        let sealed = seal(&self.me(), pk, &bytes).map_err(|e| e.to_string())?;
        let att_key = format!("att/{}", ulid::Ulid::new());
        self.mbox_out.put(&att_key, &sealed).map_err(|e| e.to_string())?;
        let msg_key =
            self.deposit(&serde_json::json!({ "t": "a", "id": wire_id, "n": name, "k": att_key }))?;
        Ok((msg_key, name))
    }

    pub fn fetch_attachment(&self, att_key: &str, name: &str) -> Result<String, String> {
        let raw = self.mbox_in.get(att_key).map_err(|e| e.to_string())?;
        let (_, bytes) = open(&self.me(), &raw).map_err(|e| e.to_string())?;
        let dir = std::env::var("HOME")
            .map(|h| format!("{h}/.sanctum/files"))
            .map_err(|_| "no HOME".to_string())?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let safe = name.replace(['/', '\\'], "_");
        let path = format!("{dir}/{}-{safe}", ulid::Ulid::new());
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        let _ = self.mbox_in.delete(att_key);
        Ok(path)
    }

    pub fn is_delivered(&self, key: &str) -> Result<bool, String> {
        Ok(!self.mbox_out.exists(key).map_err(|e| e.to_string())?)
    }

    pub fn unsend(&self, key: &str) -> Result<(), String> {
        self.mbox_out.delete(key).map_err(|e| e.to_string())
    }

    /// Drain the inbox. The hello control message needs no established peer
    /// key to open (sender pk rides in the envelope).
    pub fn poll(&self) -> Vec<ChatItem> {
        let mut out = Vec::new();
        let Ok(keys) = self.mbox_in.list("m/") else { return out };
        for key in keys {
            let Ok(raw) = self.mbox_in.get(&key) else { continue };
            let Ok((_, pt)) = open(&self.me(), &raw) else { continue };
            let txt = String::from_utf8_lossy(&pt);
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
                let item = match v.get("t").and_then(|t| t.as_str()) {
                    Some("hello") => Some(ChatItem::Hello { pk: s("pk") }),
                    Some("m") => Some(ChatItem::Message { id: s("id"), text: s("x") }),
                    Some("a") => {
                        Some(ChatItem::Attachment { id: s("id"), name: s("n"), att_key: s("k") })
                    }
                    Some("r") => Some(ChatItem::Reaction { ref_id: s("ref"), emoji: s("e") }),
                    Some("tmr") => Some(ChatItem::Timer {
                        secs: v.get("s").and_then(|x| x.as_u64()).unwrap_or(0),
                    }),
                    _ => None,
                };
                if let Some(item) = item {
                    out.push(item);
                    let _ = self.mbox_in.delete(&key);
                    continue;
                }
            }
            // Unknown payloads are left in place for a newer client.
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Multi-contact worker: owns the Store and every live session. UIs speak
// Cmd/Event over channels and never touch the database directly.
// ---------------------------------------------------------------------------

pub enum Cmd {
    NewInvite { name: String },
    AcceptInvite { link: String, name: String },
    Rename { contact_id: i64, name: String },
    Send { contact_id: i64, wire_id: String, text: String },
    SendFile { contact_id: i64, wire_id: String, path: String },
    React { contact_id: i64, ref_id: String, emoji: String },
    SetTimer { contact_id: i64, secs: u64 },
    Unsend { contact_id: i64, wire_id: String },
    /// Remove a chat: retire its buckets (when a provisioning token exists)
    /// and delete it with its history from the store.
    DeleteContact { contact_id: i64 },
    /// Clear local history: all of it, or only messages older than
    /// `older_than_secs`.
    ClearHistory { contact_id: i64, older_than_secs: Option<u64> },
}

pub enum Event {
    /// Full state on startup: every contact with its history.
    Snapshot(Vec<(Contact, Vec<StoredMsg>)>),
    ContactAdded(Contact),
    PeerJoined { contact_id: i64 },
    Received { contact_id: i64, msg: StoredMsg },
    Delivered { contact_id: i64, wire_id: String },
    Reaction { contact_id: i64, ref_id: String, emoji: String },
    TimerChanged { contact_id: i64, secs: u64 },
    Unsent { contact_id: i64, wire_id: String },
    ContactRemoved { contact_id: i64 },
    /// Authoritative reload of one chat's history (after a clear).
    History { contact_id: i64, msgs: Vec<StoredMsg> },
    Error(String),
}

struct Live {
    contact: Contact,
    session: ContactSession,
    /// (wire_id, object key, seen-once) for delivery receipts.
    pending: Vec<(String, String, bool)>,
}

pub fn spawn_worker(
    outbound: std::sync::mpsc::Receiver<Cmd>,
    events: std::sync::mpsc::Sender<Event>,
) {
    std::thread::spawn(move || {
        let err = |e: String| Event::Error(e);
        let store = match Store::open() {
            Ok(s) => s,
            Err(e) => {
                let _ = events.send(err(e.to_string()));
                return;
            }
        };
        let me = match store.identity() {
            Ok(i) => i,
            Err(e) => {
                let _ = events.send(err(e.to_string()));
                return;
            }
        };

        let mut live: Vec<Live> = Vec::new();
        let mut snapshot = Vec::new();
        for c in store.contacts().unwrap_or_default() {
            let msgs = store.messages(c.id).unwrap_or_default();
            snapshot.push((c.clone(), msgs));
            if let Ok(session) = ContactSession::new(&c, &me) {
                live.push(Live { contact: c, session, pending: Vec::new() });
            }
        }
        let _ = events.send(Event::Snapshot(snapshot));

        loop {
            while let Ok(cmd) = outbound.try_recv() {
                handle_cmd(cmd, &store, &me, &mut live, &events);
            }

            for l in &mut live {
                for item in l.session.poll() {
                    handle_item(item, l, &store, &events);
                }
                let cid = l.contact.id;
                l.pending.retain_mut(|(wire_id, key, seen)| match l.session.is_delivered(key) {
                    Ok(false) => {
                        *seen = true;
                        true
                    }
                    Ok(true) if *seen => {
                        let _ = store.set_delivered(cid, wire_id);
                        let _ = events
                            .send(Event::Delivered { contact_id: cid, wire_id: wire_id.clone() });
                        false
                    }
                    _ => true,
                });
            }

            let settings = crate::demo::Settings::load();
            if settings.disappear_secs > 0 {
                let _ = store.purge_older_than(settings.disappear_secs);
            }
            std::thread::sleep(std::time::Duration::from_millis(1500));
        }
    });
}

fn handle_cmd(
    cmd: Cmd,
    store: &Store,
    me: &Identity,
    live: &mut Vec<Live>,
    events: &std::sync::mpsc::Sender<Event>,
) {
    let send_err = |e: String| {
        let _ = events.send(Event::Error(e));
    };
    match cmd {
        Cmd::NewInvite { name } => {
            let Some(p) = Provisioner::from_settings() else {
                return send_err(
                    "inviting needs a hippius_api_token in ~/.sanctum/settings.json".into(),
                );
            };
            match create_invite(&p, me, &name) {
                Ok((mut contact, _link)) => match store.add_contact(&contact) {
                    Ok(id) => {
                        contact.id = id;
                        if let Ok(session) = ContactSession::new(&contact, me) {
                            live.push(Live { contact: contact.clone(), session, pending: Vec::new() });
                        }
                        let _ = events.send(Event::ContactAdded(contact));
                    }
                    Err(e) => send_err(e.to_string()),
                },
                Err(e) => send_err(e),
            }
        }
        Cmd::AcceptInvite { link, name } => match accept_invite(&link, me, &name) {
            Ok(mut contact) => match store.add_contact(&contact) {
                Ok(id) => {
                    contact.id = id;
                    if let Ok(session) = ContactSession::new(&contact, me) {
                        live.push(Live { contact: contact.clone(), session, pending: Vec::new() });
                    }
                    let _ = events.send(Event::ContactAdded(contact));
                }
                Err(e) => send_err(e.to_string()),
            },
            Err(e) => send_err(e),
        },
        Cmd::Rename { contact_id, name } => {
            let _ = store.rename_contact(contact_id, &name);
        }
        Cmd::Send { contact_id, wire_id, text } => {
            let Some(l) = live.iter_mut().find(|l| l.contact.id == contact_id) else { return };
            match l.session.send(&wire_id, &text) {
                Ok(key) => {
                    let _ = store.add_msg(
                        contact_id,
                        &StoredMsg {
                            wire_id: wire_id.clone(),
                            mine: true,
                            text,
                            file_path: None,
                            ts_ms: crate::store::chrono_now_ms(),
                            delivered: false,
                            reaction: None,
                        },
                    );
                    l.pending.push((wire_id, key, false));
                }
                Err(e) => send_err(e),
            }
        }
        Cmd::SendFile { contact_id, wire_id, path } => {
            let Some(l) = live.iter_mut().find(|l| l.contact.id == contact_id) else { return };
            match l.session.send_file(&wire_id, &path) {
                Ok((key, name)) => {
                    let _ = store.add_msg(
                        contact_id,
                        &StoredMsg {
                            wire_id: wire_id.clone(),
                            mine: true,
                            text: name,
                            file_path: Some(path),
                            ts_ms: crate::store::chrono_now_ms(),
                            delivered: false,
                            reaction: None,
                        },
                    );
                    l.pending.push((wire_id, key, false));
                }
                Err(e) => send_err(e),
            }
        }
        Cmd::React { contact_id, ref_id, emoji } => {
            let Some(l) = live.iter().find(|l| l.contact.id == contact_id) else { return };
            match l.session.react(&ref_id, &emoji) {
                Ok(()) => {
                    let _ = store.set_reaction(contact_id, &ref_id, &emoji);
                }
                Err(e) => send_err(e),
            }
        }
        Cmd::SetTimer { contact_id, secs } => {
            let Some(l) = live.iter().find(|l| l.contact.id == contact_id) else { return };
            if let Err(e) = l.session.set_timer(secs) {
                send_err(e);
            }
        }
        Cmd::DeleteContact { contact_id } => {
            if let Some(pos) = live.iter().position(|l| l.contact.id == contact_id) {
                let l = live.remove(pos);
                if let Some(p) = Provisioner::from_settings() {
                    let _ = p.delete_bucket(&l.contact.bucket_in);
                    let _ = p.delete_bucket(&l.contact.bucket_out);
                }
            }
            let _ = store.delete_contact(contact_id);
            let _ = events.send(Event::ContactRemoved { contact_id });
        }
        Cmd::ClearHistory { contact_id, older_than_secs } => {
            let before = older_than_secs.map(|s| crate::store::chrono_now_ms() - (s as i64) * 1000);
            let _ = store.clear_history(contact_id, before);
            let msgs = store.messages(contact_id).unwrap_or_default();
            let _ = events.send(Event::History { contact_id, msgs });
        }
        Cmd::Unsend { contact_id, wire_id } => {
            let Some(l) = live.iter_mut().find(|l| l.contact.id == contact_id) else { return };
            if let Some(pos) = l.pending.iter().position(|(w, ..)| *w == wire_id) {
                let (_, key, _) = l.pending.remove(pos);
                match l.session.unsend(&key) {
                    Ok(()) => {
                        let _ = store.delete_msg(contact_id, &wire_id);
                        let _ = events.send(Event::Unsent { contact_id, wire_id });
                    }
                    Err(_) => {
                        let _ = store.set_delivered(contact_id, &wire_id);
                        let _ = events.send(Event::Delivered { contact_id, wire_id });
                    }
                }
            }
        }
    }
}

fn handle_item(item: ChatItem, l: &mut Live, store: &Store, events: &std::sync::mpsc::Sender<Event>) {
    let cid = l.contact.id;
    match item {
        ChatItem::Hello { pk } => {
            if l.session.set_peer_pk(&pk).is_ok() {
                let _ = store.set_peer_pk(cid, &pk);
                l.contact.peer_pk = pk;
                let _ = events.send(Event::PeerJoined { contact_id: cid });
            }
        }
        ChatItem::Message { id, text } => {
            let msg = StoredMsg {
                wire_id: id,
                mine: false,
                text,
                file_path: None,
                ts_ms: crate::store::chrono_now_ms(),
                delivered: false,
                reaction: None,
            };
            let _ = store.add_msg(cid, &msg);
            let _ = events.send(Event::Received { contact_id: cid, msg });
        }
        ChatItem::Attachment { id, name, att_key } => match l.session.fetch_attachment(&att_key, &name) {
            Ok(path) => {
                let msg = StoredMsg {
                    wire_id: id,
                    mine: false,
                    text: name,
                    file_path: Some(path),
                    ts_ms: crate::store::chrono_now_ms(),
                    delivered: false,
                    reaction: None,
                };
                let _ = store.add_msg(cid, &msg);
                let _ = events.send(Event::Received { contact_id: cid, msg });
            }
            Err(e) => {
                let _ = events.send(Event::Error(format!("attachment {name}: {e}")));
            }
        },
        ChatItem::Reaction { ref_id, emoji } => {
            let _ = store.set_reaction(cid, &ref_id, &emoji);
            let _ = events.send(Event::Reaction { contact_id: cid, ref_id, emoji });
        }
        ChatItem::Timer { secs } => {
            let mut s = crate::demo::Settings::load();
            s.disappear_secs = secs;
            s.save();
            let _ = events.send(Event::TimerChanged { contact_id: cid, secs });
        }
    }
}
