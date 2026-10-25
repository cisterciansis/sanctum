//! Real per-contact sessions (SPEC-V2 §4b): every contact is a pair of
//! freshly-minted queue buckets plus a capability token. The inviter mints
//! and pays (~cents); the invitee needs no account, ever — their capability
//! arrives inside the invite link.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use serde::{Deserialize, Serialize};

use crate::crypto_box::PublicKey;
use crate::mesh::{self, MESH};
use crate::provision::Provisioner;
use crate::store::{Contact, Mark, Store, StoredMsg};
use crate::transport::Mailbox;
use crate::{open, seal, Identity, Ratchet};

pub const SCHEME_V2: &str = "sanctum://v2/";

/// Object in `to_acceptor` holding the inviter's v2 key bundle.
const BUNDLE_KEY: &str = "bundle";

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
    /// Envelope version the inviter speaks; 2 means a key bundle waits at
    /// `BUNDLE_KEY` in `to_acceptor`. Absent (0) on pre-v2 links.
    #[serde(default)]
    pub v: u8,
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
/// local, one ships in the invite) and publish a v2 key bundle. Returns the
/// contact row, the link and the live session.
pub fn create_invite(
    p: &Provisioner,
    me: &Identity,
    name: &str,
) -> Result<(Contact, String, ContactSession), String> {
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
        v: 2,
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
        ..Default::default()
    };
    let (ratchet, bundle) = Ratchet::offer(me).map_err(|e| e.to_string())?;
    let session = ContactSession::new(&contact, me, Some(ratchet))?;
    session.mbox_out.put(BUNDLE_KEY, &bundle).map_err(|e| e.to_string())?;
    Ok((contact, link, session))
}

/// Acceptor side: adopt the capabilities and announce our public key — as
/// the first v2 (PQXDH) message when the inviter published a bundle.
pub fn accept_invite(
    link: &str,
    me: &Identity,
    name: &str,
) -> Result<(Contact, ContactSession), String> {
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
        ..Default::default()
    };
    let mut session = ContactSession::new(&contact, me, None)?;
    if inv.v >= 2 {
        let bundle = session.mbox_in.get(BUNDLE_KEY).map_err(|e| e.to_string())?;
        let inviter = session.peer_pk.as_ref().ok_or("invite has no key")?;
        session.ratchet = Some(Ratchet::accept(me, &bundle, inviter).map_err(|e| e.to_string())?);
    }
    // The inviter can't read our mesh tags before this hello: Hippius only.
    let mesh_key = session.mesh_key.take();
    session.deposit(&serde_json::json!({
        "t": "hello",
        "pk": B64.encode(me.public().as_bytes()),
    }))?;
    session.mesh_key = mesh_key;
    if session.ratchet.is_some() {
        let _ = session.mbox_in.delete(BUNDLE_KEY);
    }
    Ok((contact, session))
}

/// One live conversation: poll bucket_in, write bucket_out.
pub struct ContactSession {
    me_secret: String,
    peer_pk: Option<PublicKey>,
    /// v2 session; None for pre-v2 contacts, which stay on v1 envelopes.
    ratchet: Option<Ratchet>,
    /// Ratchet state changed since the last `ratchet_update`.
    dirty: bool,
    mbox_in: Mailbox,
    mbox_out: Mailbox,
    /// Pair key for mesh tags and acks; None until the peer's key is known.
    mesh_key: Option<[u8; 32]>,
    /// Sealed objects offered to the mesh and not yet on Hippius: each waits
    /// for an ack, then for the network. Persisted, so nothing is lost or
    /// misread as delivered across a restart.
    outbox: Vec<Outgoing>,
    /// Outbox changed since the last `outbox_update`.
    outbox_dirty: bool,
    /// Digests of envelopes the mesh delivered, to drop their Hippius copy.
    mesh_seen: std::collections::VecDeque<[u8; 16]>,
}

struct Outgoing {
    key: String,
    sealed: Vec<u8>,
    ack: [u8; 16],
    /// Last attempt (mesh send or upload).
    at: std::time::Instant,
}

/// How long the mesh gets to ack before Hippius takes over; also the
/// upload retry interval while offline.
const MESH_ACK_WAIT: std::time::Duration = std::time::Duration::from_secs(8);
const MESH_SEEN_CAP: usize = 256;

pub enum ChatItem {
    Hello { pk: String },
    Message { id: String, text: String },
    Attachment { id: String, name: String, att_key: String },
    Reaction { ref_id: String, emoji: String },
    Timer { secs: u64 },
    /// The peer deleted one of their messages for everyone.
    Delete { ref_id: String },
    /// The peer read these of our messages.
    Read { refs: Vec<String> },
    /// TAO address share or payment card (validated in `wallet`).
    Tao(serde_json::Value),
    /// Something arrived sealed under another identity key; it was refused.
    KeyChanged,
}

impl ContactSession {
    pub fn new(c: &Contact, me: &Identity, ratchet: Option<Ratchet>) -> Result<Self, String> {
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
            mesh_key: peer_pk.as_ref().and_then(|pk| mesh::pair_key(me, pk)),
            outbox: Vec::new(),
            outbox_dirty: false,
            mesh_seen: Default::default(),
            peer_pk,
            dirty: ratchet.is_some(),
            ratchet,
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
        let pk = PublicKey::from_bytes(bytes);
        self.mesh_key = mesh::pair_key(&self.me(), &pk);
        self.peer_pk = Some(pk);
        Ok(())
    }

    pub fn ready(&self) -> bool {
        self.peer_pk.is_some()
    }

    /// Ratchet state to persist, if a send or receive advanced it.
    pub fn ratchet_update(&mut self) -> Option<String> {
        if !std::mem::take(&mut self.dirty) {
            return None;
        }
        self.ratchet.as_ref()?.save().ok()
    }

    /// v2 once a ratchet session exists, else v1 to the peer's static key.
    fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        match &mut self.ratchet {
            Some(r) if r.ready() => {
                self.dirty = true;
                r.seal(plaintext).map_err(|e| e.to_string())
            }
            _ => {
                let pk = self.peer_pk.as_ref().ok_or("contact hasn't joined yet")?;
                seal(&self.me(), pk, plaintext).map_err(|e| e.to_string())
            }
        }
    }

    /// Opens either version; v1 is refused once a v2 session exists, so a
    /// bucket writer can't downgrade an established chat.
    fn unseal(&mut self, raw: &[u8]) -> Result<(PublicKey, Vec<u8>), String> {
        match (crate::version(raw), &mut self.ratchet) {
            (2, Some(r)) => {
                self.dirty = true;
                r.open(raw).map_err(|e| e.to_string())
            }
            (1, r) if !r.as_ref().is_some_and(Ratchet::ready) => {
                open(&self.me(), raw).map_err(|e| e.to_string())
            }
            (v, _) => Err(format!("unexpected envelope v{v}")),
        }
    }

    fn deposit(&mut self, body: &serde_json::Value) -> Result<String, String> {
        self.deposit_at(format!("m/{}", ulid::Ulid::new()), body)
    }

    /// Messages are stored at `m/<wire_id>`, so a restarted client can still
    /// tell from the bucket whether they were picked up.
    fn deposit_at(&mut self, key: String, body: &serde_json::Value) -> Result<String, String> {
        let sealed = self.seal(body.to_string().as_bytes())?;
        if !self.via_mesh(&key, &sealed) {
            self.mbox_out.put(&key, &sealed).map_err(|e| e.to_string())?;
        }
        Ok(key)
    }

    /// Bluetooth first when any Sanctum device is in range: the object waits
    /// in the outbox for the peer's ack, and goes to Hippius if none comes.
    /// Ratchet (v2) envelopes only, which a relay can't replay.
    fn via_mesh(&mut self, key: &str, sealed: &[u8]) -> bool {
        let (Some(k), Some(peer)) = (&self.mesh_key, &self.peer_pk) else { return false };
        if crate::version(sealed) != 2
            || mesh::HEADER + sealed.len() > mesh::MAX_PACKET
            || MESH.neighbors() == 0
        {
            return false;
        }
        let tag = mesh::tag(k, peer, mesh::epoch_now());
        MESH.send(mesh::Packet { kind: mesh::ENVELOPE, ttl: mesh::TTL, tag, body: sealed.to_vec() });
        self.outbox.push(Outgoing {
            key: key.into(),
            sealed: sealed.to_vec(),
            ack: mesh::ack_token(k, sealed),
            at: std::time::Instant::now(),
        });
        self.outbox_dirty = true;
        true
    }

    /// Upload what the mesh didn't deliver in time; retried while offline.
    pub fn flush_outbox(&mut self) {
        let mbox = &self.mbox_out;
        let before = self.outbox.len();
        self.outbox.retain_mut(|o| {
            if o.at.elapsed() < MESH_ACK_WAIT {
                return true;
            }
            o.at = std::time::Instant::now();
            mbox.put(&o.key, &o.sealed).is_err()
        });
        self.outbox_dirty |= self.outbox.len() != before;
    }

    /// The peer acked one of our objects over the mesh; returns its key.
    pub fn mesh_acked(&mut self, token: &[u8; 16]) -> Option<String> {
        let i = self.outbox.iter().position(|o| o.ack == *token)?;
        self.outbox_dirty = true;
        Some(self.outbox.remove(i).key)
    }

    /// Tags the peer addresses us by around now (one epoch of clock skew).
    pub fn mesh_tags(&self) -> Vec<[u8; 16]> {
        let Some(k) = &self.mesh_key else { return Vec::new() };
        let (me, e) = (self.me().public(), mesh::epoch_now());
        [e.saturating_sub(1), e, e + 1].iter().map(|&e| mesh::tag(k, &me, e)).collect()
    }

    /// An envelope that came over the mesh with one of our tags. Returns the
    /// item (None if unknown to this client) and the ack to send back, or
    /// None if it doesn't open.
    /// Unseal with the peer's identity pinned. Err(true): sealed under a
    /// different identity, refused and never adopted (a Sanctum key never
    /// legitimately changes; a reinstall is a new contact).
    fn open_pinned(&mut self, raw: &[u8]) -> Result<(PublicKey, Vec<u8>), bool> {
        let (from, pt) = self.unseal(raw).map_err(|e| e == crate::KEY_CHANGED)?;
        if self.peer_pk.as_ref().is_some_and(|pk| pk.as_bytes() != from.as_bytes()) {
            return Err(true);
        }
        Ok((from, pt))
    }

    pub fn open_mesh(&mut self, sealed: &[u8]) -> Option<(Option<ChatItem>, [u8; 16])> {
        let k = self.mesh_key?;
        if crate::version(sealed) != 2 {
            return None;
        }
        // Same pin as the Hippius path; a mismatch is refused (and not acked).
        let (from, pt) = self.open_pinned(sealed).ok()?;
        if self.mesh_seen.len() >= MESH_SEEN_CAP {
            self.mesh_seen.pop_front();
        }
        self.mesh_seen.push_back(mesh::digest(sealed));
        Some((parse_item(&from, &pt), mesh::ack_token(&k, sealed)))
    }

    /// Outbox to persist, if it changed: JSON [[key, b64 envelope], ...].
    pub fn outbox_update(&mut self) -> Option<String> {
        if !std::mem::take(&mut self.outbox_dirty) {
            return None;
        }
        let rows: Vec<(&str, String)> =
            self.outbox.iter().map(|o| (o.key.as_str(), B64.encode(&o.sealed))).collect();
        Some(serde_json::to_string(&rows).expect("outbox serializes"))
    }

    /// Reload a persisted outbox; it goes to Hippius on the next flush.
    pub fn restore_outbox(&mut self, blob: &str) {
        let Some(k) = self.mesh_key else { return };
        let rows: Vec<(String, String)> = serde_json::from_str(blob).unwrap_or_default();
        let due = std::time::Instant::now().checked_sub(MESH_ACK_WAIT);
        for (key, sealed) in rows {
            let (Ok(sealed), Some(at)) = (B64.decode(sealed), due) else { continue };
            self.outbox.push(Outgoing { ack: mesh::ack_token(&k, &sealed), key, sealed, at });
        }
    }

    pub fn send(&mut self, wire_id: &str, text: &str) -> Result<String, String> {
        self.deposit_at(format!("m/{wire_id}"), &serde_json::json!({ "t": "m", "id": wire_id, "x": text }))
    }

    pub fn react(&mut self, ref_id: &str, emoji: &str) -> Result<(), String> {
        self.deposit(&serde_json::json!({ "t": "r", "ref": ref_id, "e": emoji }))?;
        Ok(())
    }

    /// Send a control message (payments) over this chat's session; returns
    /// its object key, for delivery tracking.
    pub(crate) fn send_control(&mut self, body: &serde_json::Value) -> Result<String, String> {
        self.deposit(body)
    }

    /// Tell the peer we read these of their messages.
    pub fn send_read(&mut self, wire_ids: &[String]) -> Result<(), String> {
        self.deposit(&serde_json::json!({ "t": "read", "ref": wire_ids }))?;
        Ok(())
    }

    /// Ask the peer to delete one of our messages from their device.
    pub fn delete_for_everyone(&mut self, wire_id: &str) -> Result<(), String> {
        self.deposit(&serde_json::json!({ "t": "del", "ref": wire_id }))?;
        Ok(())
    }

    pub fn set_timer(&mut self, secs: u64) -> Result<(), String> {
        self.deposit(&serde_json::json!({ "t": "tmr", "s": secs }))?;
        Ok(())
    }

    pub fn send_file(&mut self, wire_id: &str, path: &str) -> Result<(String, String), String> {
        let bytes = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
        if bytes.len() > 25 * 1024 * 1024 {
            return Err("file larger than 25 MB".into());
        }
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| pretty_file_name(&n.to_string_lossy()))
            .unwrap_or_else(|| "file".into());
        let sealed = self.seal(&bytes)?;
        let att_key = format!("att/{}", ulid::Ulid::new());
        self.mbox_out.put(&att_key, &sealed).map_err(|e| e.to_string())?;
        let body = serde_json::json!({ "t": "a", "id": wire_id, "n": name, "k": att_key });
        let msg_key = self.deposit_at(format!("m/{wire_id}"), &body)?;
        Ok((msg_key, name))
    }

    pub fn fetch_attachment(&mut self, att_key: &str, name: &str) -> Result<String, String> {
        let raw = self.mbox_in.get(att_key).map_err(|e| e.to_string())?;
        let (_, bytes) = self.unseal(&raw)?;
        let dir = crate::data_dir().map(|d| format!("{d}/files")).ok_or("no HOME")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let safe = name.replace(['/', '\\'], "_");
        let path = format!("{dir}/{}-{safe}", ulid::Ulid::new());
        std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
        let _ = self.mbox_in.delete(att_key);
        Ok(path)
    }

    /// Drop an attachment blob unread (blocked sender).
    pub fn discard(&self, att_key: &str) {
        let _ = self.mbox_in.delete(att_key);
    }

    pub fn is_delivered(&self, key: &str) -> Result<bool, String> {
        if self.outbox.iter().any(|o| o.key == key) {
            return Ok(false);
        }
        Ok(!self.mbox_out.exists(key).map_err(|e| e.to_string())?)
    }

    /// Something still in the outbox never reached Hippius (the mesh may
    /// have carried it already; that can't be recalled).
    pub fn unsend(&mut self, key: &str) -> Result<(), String> {
        let before = self.outbox.len();
        self.outbox.retain(|o| o.key != key);
        if self.outbox.len() != before {
            self.outbox_dirty = true;
            return Ok(());
        }
        self.mbox_out.delete(key).map_err(|e| e.to_string())
    }

    /// Drain the inbox. The hello control message needs no established peer
    /// key to open (sender pk rides in the envelope, authenticated by it).
    pub fn poll(&mut self) -> Vec<ChatItem> {
        let mut out = Vec::new();
        let Ok(keys) = self.mbox_in.list("m/") else { return out };
        for key in keys {
            let Ok(raw) = self.mbox_in.get(&key) else { continue };
            if self.mesh_seen.contains(&mesh::digest(&raw)) {
                let _ = self.mbox_in.delete(&key);
                continue;
            }
            let (from, pt) = match self.open_pinned(&raw) {
                Ok(opened) => opened,
                Err(true) => {
                    out.push(ChatItem::KeyChanged);
                    let _ = self.mbox_in.delete(&key);
                    continue;
                }
                Err(false) => continue,
            };
            if let Some(item) = parse_item(&from, &pt) {
                out.push(item);
                let _ = self.mbox_in.delete(&key);
                continue;
            }
            // Unknown payloads are left in place for a newer client.
        }
        out
    }
}

/// A decrypted chat payload; None for types this client doesn't know.
fn parse_item(from: &PublicKey, pt: &[u8]) -> Option<ChatItem> {
    let v = serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(pt)).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string();
    match v.get("t").and_then(|t| t.as_str()) {
        Some("hello") => Some(ChatItem::Hello { pk: B64.encode(from.as_bytes()) }),
        Some("m") => Some(ChatItem::Message { id: s("id"), text: s("x") }),
        Some("a") => Some(ChatItem::Attachment { id: s("id"), name: s("n"), att_key: s("k") }),
        Some("r") => Some(ChatItem::Reaction { ref_id: s("ref"), emoji: s("e") }),
        Some("tmr") => Some(ChatItem::Timer { secs: v.get("s").and_then(|x| x.as_u64()).unwrap_or(0) }),
        Some("del") => Some(ChatItem::Delete { ref_id: s("ref") }),
        Some("taoaddr" | "pay") => Some(ChatItem::Tao(v.clone())),
        // Peer-controlled: bounded count and id length.
        Some("read") => Some(ChatItem::Read {
            refs: v
                .get("ref")
                .and_then(|r| r.as_array())
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str())
                .filter(|x| x.len() <= 64)
                .take(MAX_RECEIPT_IDS)
                .map(String::from)
                .collect(),
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Multi-contact worker: owns the Store and every live session. UIs speak
// Cmd/Event over channels and never touch the database directly.
// ---------------------------------------------------------------------------

pub enum Cmd {
    NewInvite { name: String },
    /// Mint a group: full pairwise mesh, one invite link per other seat.
    NewGroup { name: String, size: i64 },
    AcceptInvite { link: String, name: String },
    Rename { contact_id: i64, name: String },
    Send { contact_id: i64, wire_id: String, text: String },
    SendFile { contact_id: i64, wire_id: String, path: String },
    React { contact_id: i64, ref_id: String, emoji: String },
    SetTimer { contact_id: i64, secs: u64 },
    Unsend { contact_id: i64, wire_id: String },
    /// The user is looking at this chat: mark received messages read and,
    /// if read receipts are on, tell the peer.
    MarkRead { contact_id: i64 },
    /// TAO wallet and in-chat payments.
    Tao(crate::wallet::TaoCmd),
    /// Delete one message from this device; with `everyone`, also from the
    /// peer's (own messages in direct chats only).
    DeleteMsg { contact_id: i64, wire_id: String, everyone: bool },
    /// Remove a chat: retire its buckets (when a provisioning token exists)
    /// and delete it with its history from the store.
    DeleteContact { contact_id: i64 },
    /// Clear local history: all of it, or only messages older than
    /// `older_than_secs`.
    ClearHistory { contact_id: i64, older_than_secs: Option<u64> },
    /// Set or clear a local mark on a direct chat (verified, blocked, …).
    Mark { contact_id: i64, mark: Mark, on: bool },
    /// A packet the Bluetooth mesh delivered (possibly for someone else).
    Mesh(mesh::Packet),
}

pub enum Event {
    /// Full state on startup: every contact with its history.
    Snapshot(Vec<(Contact, Vec<StoredMsg>)>),
    ContactAdded(Contact),
    PeerJoined { contact_id: i64 },
    Received { contact_id: i64, msg: StoredMsg },
    Delivered { contact_id: i64, wire_id: String },
    /// The peer read these own messages (only with read receipts on).
    Read { contact_id: i64, wire_ids: Vec<String>, ts_ms: i64 },
    /// A message's text/payment card changed (payment status updates).
    MsgUpdated { contact_id: i64, wire_id: String, text: String, pay: Option<String> },
    /// Wallet accounts, balances and network.
    TaoWallet(crate::wallet::WalletView),
    /// A recovery phrase the user asked to see.
    TaoPhrase { address: String, phrase: String },
    /// An own message finished uploading (it is now waiting to be picked up).
    Sent { contact_id: i64, wire_id: String },
    /// An own message never left this device; `error` says why.
    SendFailed { contact_id: i64, wire_id: String, error: String },
    Reaction { contact_id: i64, ref_id: String, emoji: String },
    TimerChanged { contact_id: i64, secs: u64 },
    /// A message left the view: unsent, or deleted here or by the peer.
    Unsent { contact_id: i64, wire_id: String },
    ContactRemoved { contact_id: i64 },
    /// Contact/group state changed (e.g. a group seat was claimed).
    ContactUpdated(Contact),
    /// Authoritative reload of one chat's history (after a clear).
    History { contact_id: i64, msgs: Vec<StoredMsg> },
    Error(String),
}

impl Live {
    /// Mark `wire_id` delivered once the peer picks up the object at `key`
    /// (wallet messages ride the same receipts as chat messages).
    pub(crate) fn track(&mut self, wire_id: &str, key: String) {
        self.pending.push((wire_id.to_string(), key, false, std::time::Instant::now()));
    }
}

pub(crate) struct Live {
    pub(crate) contact: Contact,
    pub(crate) session: ContactSession,
    /// (wire_id, object key, seen-once, put at) for delivery receipts.
    pending: Vec<(String, String, bool, std::time::Instant)>,
}

/// One worker at a time: after a wipe the next one waits here until the
/// stale one has noticed and let go of the old database.
static ONE_WORKER: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Emergency wipe (bitchat's triple-tap): retire the running worker, then
/// delete every local trace. Never touches the network, so it is instant;
/// spawn a fresh worker afterwards to start over as on first launch.
pub fn wipe() -> Result<(), String> {
    crate::bump_epoch();
    crate::store::wipe().map_err(|e| e.to_string())
}

/// The contact with its safety number (empty until their key is known).
fn with_safety(mut c: Contact, me: &Identity) -> Contact {
    let peer = B64.decode(&c.peer_pk).ok().and_then(|b| <[u8; 32]>::try_from(b).ok());
    c.safety = peer
        .and_then(|b| crate::safety_number(&me.public(), &PublicKey::from_bytes(b)))
        .unwrap_or_default();
    c
}

pub fn spawn_worker(
    outbound: std::sync::mpsc::Receiver<Cmd>,
    events: std::sync::mpsc::Sender<Event>,
) {
    let epoch = crate::epoch();
    std::thread::spawn(move || {
        let _one = ONE_WORKER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        crate::set_born(epoch);
        if crate::stale() {
            return;
        }
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
        let mut groups: Vec<GroupLive> = Vec::new();
        let mut jobs = crate::wallet::Jobs::new();
        let mut snapshot = Vec::new();
        for c in store.contacts().unwrap_or_default() {
            let c = with_safety(c, &me);
            let msgs = store.messages(c.id).unwrap_or_default();
            // Own messages still in flight at the last exit.
            let in_flight: Vec<String> =
                msgs.iter().filter(|m| m.mine && !m.delivered).map(|m| m.wire_id.clone()).collect();
            snapshot.push((c.clone(), msgs));
            let ratchet = store.ratchet(&c.bucket_in).map(|b| Ratchet::load(&me, &b)).transpose();
            match ratchet.map_err(|e| e.to_string()).and_then(|r| ContactSession::new(&c, &me, r)) {
                Ok(mut session) => {
                    if let Some(blob) = store.outbox(&c.bucket_in) {
                        session.restore_outbox(&blob);
                    }
                    // Resume receipts for own messages still in flight at the
                    // last exit (pre-`m/<wire_id>` sends read as delivered).
                    let long_ago = std::time::Instant::now()
                        .checked_sub(PUT_VISIBLE)
                        .unwrap_or_else(std::time::Instant::now);
                    let pending = in_flight
                        .into_iter()
                        .map(|w| {
                            let key = format!("m/{w}");
                            (w, key, false, long_ago)
                        })
                        .collect();
                    live.push(Live { contact: c, session, pending })
                }
                Err(e) => {
                    let _ = events.send(err(format!("{}: {e}", c.name)));
                }
            }
        }
        for g in store.groups().unwrap_or_default() {
            let msgs = store.messages(-g.id).unwrap_or_default();
            snapshot.push((group_as_contact(&g), msgs));
            if let Ok(gl) = GroupLive::new(&g, &me) {
                groups.push(gl);
            }
        }
        let _ = events.send(Event::Snapshot(snapshot));

        loop {
            // A wipe retires this worker; stop before touching anything.
            if crate::stale() {
                return;
            }
            crate::wallet::drain(&store, &mut live, &events, &mut jobs);
            live.iter_mut().for_each(|l| persist_session(&store, l));

            for l in &mut live {
                if crate::stale() {
                    return;
                }
                for item in l.session.poll() {
                    handle_item(item, l, &store, &me, &events, &jobs);
                }
                l.session.flush_outbox();
                persist_session(&store, l);
                let cid = l.contact.id;
                l.pending.retain_mut(|(wire_id, key, seen, put_at)| match l.session.is_delivered(key) {
                    Ok(false) => {
                        *seen = true;
                        true
                    }
                    // Gone counts once we saw it, or once a fresh PUT has had
                    // time to become visible (a fast reader can take it first).
                    Ok(true) if *seen || put_at.elapsed() > PUT_VISIBLE => {
                        let _ = store.set_delivered(cid, wire_id);
                        let _ = events
                            .send(Event::Delivered { contact_id: cid, wire_id: wire_id.clone() });
                        false
                    }
                    _ => true,
                });
            }

            for gl in &mut groups {
                if crate::stale() {
                    return;
                }
                let gid = -gl.group.id;
                let items = gl.poll();
                for item in items {
                    match item {
                        GroupItem::NewPeer { seat, pk } => {
                            if gl.register_peer(seat, &pk) {
                                let _ = store.set_group_member(gl.group.id, seat, &pk);
                                gl.group.members.push((seat, pk));
                                // Mutual discovery: tell the newcomer who we are.
                                gl.announce_to(&[seat]);
                                let _ =
                                    events.send(Event::ContactUpdated(group_as_contact(&gl.group)));
                            }
                        }
                        GroupItem::Message { seat, id, text } => {
                            let msg = StoredMsg {
                                wire_id: id,
                                mine: false,
                                text,
                                file_path: None,
                                ts_ms: crate::store::chrono_now_ms(),
                                delivered: false,
                                reaction: None,
                                sender_seat: Some(seat),
                                read_ms: None,
                                pay: None,
                            };
                            let _ = store.add_msg(gid, &msg);
                            let _ = events.send(Event::Received { contact_id: gid, msg });
                        }
                        GroupItem::Reaction { ref_id, emoji } => {
                            let _ = store.set_reaction(gid, &ref_id, &emoji);
                            let _ = events
                                .send(Event::Reaction { contact_id: gid, ref_id, emoji });
                        }
                    }
                }
            }

            let settings = crate::demo::Settings::load();
            if settings.disappear_secs > 0 && crate::demo::disappear_ok(settings.disappear_secs) {
                // Tell the views too: rows vanishing silently stayed on screen.
                for (contact_id, wire_id, file) in
                    store.purge_older_than(settings.disappear_secs).unwrap_or_default()
                {
                    remove_saved_file(file);
                    let _ = events.send(Event::Unsent { contact_id, wire_id });
                }
            }
            // Wait out the poll interval, serving commands (and mesh packets)
            // as they arrive rather than at the next pass.
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
            while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
                match outbound.recv_timeout(left) {
                    Ok(cmd) => {
                        match cmd {
                            Cmd::Tao(t) => crate::wallet::handle_cmd(t, &store, &mut live, &events, &mut jobs),
                            cmd => handle_cmd(cmd, &store, &me, &mut live, &mut groups, &events, &jobs),
                        }
                        live.iter_mut().for_each(|l| persist_session(&store, l));
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        }
    });
}

/// Delete a received attachment's saved copy. Only files under our own
/// files dir: a sent message's path is the user's original, never ours to remove.
fn remove_saved_file(path: Option<String>) {
    if let (Some(path), Some(dir)) = (path, crate::data_dir()) {
        if std::path::Path::new(&path).starts_with(format!("{dir}/files")) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Most message ids one read receipt names (also the cap on what we accept).
const MAX_RECEIPT_IDS: usize = 200;

/// Hippius makes a PUT visible to HEAD within ~2.5 s; until then "missing"
/// may just mean "not visible yet", so absence only proves delivery after this.
const PUT_VISIBLE: std::time::Duration = std::time::Duration::from_secs(10);

fn persist_session(store: &Store, l: &mut Live) {
    if let Some(blob) = l.session.ratchet_update() {
        if let Err(e) = store.set_ratchet(&l.contact.bucket_in, &blob) {
            // Retry on the next pass rather than lose ratchet state.
            l.session.dirty = true;
            eprintln!("persist ratchet: {e}");
        }
    }
    if let Some(blob) = l.session.outbox_update() {
        if let Err(e) = store.set_outbox(&l.contact.bucket_in, &blob) {
            l.session.outbox_dirty = true;
            eprintln!("persist outbox: {e}");
        }
    }
}

/// Strip the collision-safety prefix (uuid or ulid + '-') from a stored
/// attachment filename for display.
fn pretty_file_name(name: &str) -> String {
    let is_uuid = |s: &str| {
        s.len() == 36
            && s.chars().enumerate().all(|(i, c)| match i {
                8 | 13 | 18 | 23 => c == '-',
                _ => c.is_ascii_hexdigit(),
            })
    };
    let is_ulid = |s: &str| s.len() == 26 && s.chars().all(|c| c.is_ascii_alphanumeric());
    if name.len() > 37 && is_uuid(&name[..36]) && name.as_bytes()[36] == b'-' {
        return name[37..].to_string();
    }
    if name.len() > 27 && is_ulid(&name[..26]) && name.as_bytes()[26] == b'-' {
        return name[27..].to_string();
    }
    name.to_string()
}

fn group_as_contact(g: &crate::store::Group) -> Contact {
    Contact {
        id: -g.id,
        name: g.name.clone(),
        bucket_in: String::new(),
        bucket_out: String::new(),
        access_key: String::new(),
        secret: String::new(),
        // "waiting" in the UI means no peer has joined yet.
        peer_pk: if g.members.len() > 1 { "joined".into() } else { String::new() },
        endpoint: g.endpoint.clone(),
        invite_link: open_seat_links(g),
        ..Default::default()
    }
}

/// Creator's seat links with claimed seats blanked (line n is seat n+1, so
/// labels stay stable); empty string once every seat is claimed.
fn open_seat_links(g: &crate::store::Group) -> String {
    let lines: Vec<&str> = g
        .invite_links
        .split('\n')
        .enumerate()
        .map(|(n, l)| {
            let seat = n as i64 + 1;
            if g.members.iter().any(|(s, _)| *s == seat) { "" } else { l }
        })
        .collect();
    if lines.iter().all(|l| l.is_empty()) {
        String::new()
    } else {
        lines.join("\n")
    }
}

fn handle_cmd(
    cmd: Cmd,
    store: &Store,
    me: &Identity,
    live: &mut Vec<Live>,
    groups: &mut Vec<GroupLive>,
    events: &std::sync::mpsc::Sender<Event>,
    jobs: &crate::wallet::Jobs,
) {
    // Negative ids are groups.
    if let Some(gid) = cmd_group_id(&cmd) {
        return handle_group_cmd(cmd, gid, store, groups, events);
    }
    let send_err = |e: String| {
        let _ = events.send(Event::Error(e));
    };
    match cmd {
        Cmd::NewInvite { name } => {
            let Some(p) = Provisioner::from_settings() else {
                return send_err("starting a chat needs a mailbox account: Settings → Mailbox".into());
            };
            match create_invite(&p, me, &name) {
                Ok((mut contact, _link, session)) => match store.add_contact(&contact) {
                    Ok(id) => {
                        contact.id = id;
                        live.push(Live { contact: contact.clone(), session, pending: Vec::new() });
                        let _ = events.send(Event::ContactAdded(contact));
                    }
                    Err(e) => send_err(e.to_string()),
                },
                Err(e) => send_err(e),
            }
        }
        Cmd::NewGroup { name, size } => {
            let Some(p) = Provisioner::from_settings() else {
                return send_err("creating a group needs a mailbox account: Settings → Mailbox".into());
            };
            match create_group(&p, me, &name, size) {
                Ok((mut group, _links)) => match store.add_group(&group) {
                    Ok(id) => {
                        group.id = id;
                        let _ = store.set_group_member(id, group.my_seat, {
                            &B64.encode(me.public().as_bytes())
                        });
                        if let Ok(gl) = GroupLive::new(&group, me) {
                            groups.push(gl);
                        }
                        let _ = events.send(Event::ContactAdded(group_as_contact(&group)));
                    }
                    Err(e) => send_err(e.to_string()),
                },
                Err(e) => send_err(e),
            }
        }
        Cmd::AcceptInvite { link, name } if link.trim().starts_with(SCHEME_G1) => {
            if let Ok(inv) = GroupInvite::from_link(&link) {
                if groups.iter().any(|g| g.group.gid == inv.gid && g.group.my_seat == inv.seat) {
                    return send_err("You've already joined with this invite".into());
                }
            }
            match accept_group(&link, me) {
                Ok(mut group) => match store.add_group(&group) {
                    Ok(id) => {
                        group.id = id;
                        let _ = store.set_group_member(id, 0, &group.members[0].1.clone());
                        let _ = store.set_group_member(id, group.my_seat, {
                            &B64.encode(me.public().as_bytes())
                        });
                        match GroupLive::new(&group, me) {
                            Ok(gl) => {
                                gl.announce_all();
                                groups.push(gl);
                                let _ =
                                    events.send(Event::ContactAdded(group_as_contact(&group)));
                            }
                            Err(e) => send_err(e),
                        }
                    }
                    Err(e) => send_err(e.to_string()),
                },
                Err(e) => send_err(e),
            }
        }
        Cmd::AcceptInvite { link, .. }
            if Invite2::from_link(&link)
                .map(|inv| live.iter().any(|l| l.contact.bucket_out == inv.to_inviter))
                .unwrap_or(false) =>
        {
            send_err("You've already joined with this invite".into())
        }
        Cmd::AcceptInvite { link, name } => match accept_invite(&link, me, &name) {
            Ok((contact, session)) => match store.add_contact(&contact) {
                Ok(id) => {
                    let contact = with_safety(Contact { id, ..contact }, me);
                    live.push(Live { contact: contact.clone(), session, pending: Vec::new() });
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
                            sender_seat: None,
                            read_ms: None,
                            pay: None,
                        },
                    );
                    let _ = events.send(Event::Sent { contact_id, wire_id: wire_id.clone() });
                    l.pending.push((wire_id, key, false, std::time::Instant::now()));
                }
                Err(error) => {
                    let _ = events.send(Event::SendFailed { contact_id, wire_id, error });
                }
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
                            text: pretty_file_name(&name),
                            file_path: Some(path),
                            ts_ms: crate::store::chrono_now_ms(),
                            delivered: false,
                            reaction: None,
                            sender_seat: None,
                            read_ms: None,
                            pay: None,
                        },
                    );
                    let _ = events.send(Event::Sent { contact_id, wire_id: wire_id.clone() });
                    l.pending.push((wire_id, key, false, std::time::Instant::now()));
                }
                Err(error) => {
                    let _ = events.send(Event::SendFailed { contact_id, wire_id, error });
                }
            }
        }
        Cmd::React { contact_id, ref_id, emoji } => {
            let Some(l) = live.iter_mut().find(|l| l.contact.id == contact_id) else { return };
            match l.session.react(&ref_id, &emoji) {
                Ok(()) => {
                    let _ = store.set_reaction(contact_id, &ref_id, &emoji);
                }
                Err(e) => send_err(e),
            }
        }
        Cmd::SetTimer { contact_id, secs } => {
            if !crate::demo::disappear_ok(secs) {
                return send_err("timer must be off or between 1 minute and 1 year".into());
            }
            let Some(l) = live.iter_mut().find(|l| l.contact.id == contact_id) else { return };
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
                let (_, key, ..) = l.pending.remove(pos);
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
        Cmd::Tao(_) => {} // the worker routes these to `wallet`
        Cmd::MarkRead { contact_id } => {
            let Ok(ids) = store.mark_incoming_read(contact_id) else { return };
            if ids.is_empty() || !crate::demo::Settings::load().read_receipts {
                return;
            }
            let Some(l) = live.iter_mut().find(|l| l.contact.id == contact_id) else { return };
            // One receipt per look, naming at most the newest MAX_RECEIPT_IDS.
            let newest = &ids[ids.len().saturating_sub(MAX_RECEIPT_IDS)..];
            if let Err(e) = l.session.send_read(newest) {
                send_err(format!("read receipt: {e}"));
            }
        }
        Cmd::Mark { contact_id, mark, on } => {
            let Some(l) = live.iter_mut().find(|l| l.contact.id == contact_id) else { return };
            if let Err(e) = store.set_mark(contact_id, mark, on) {
                return send_err(e.to_string());
            }
            let c = &mut l.contact;
            match mark {
                Mark::Verified => c.verified = on,
                Mark::Blocked => c.blocked = on,
                Mark::Favorite => c.favorite = on,
                Mark::KeyChanged => c.key_changed = on,
            }
            // Same pairing as Store::set_mark.
            match (mark, on) {
                (Mark::Verified, true) => c.key_changed = false,
                (Mark::KeyChanged, true) => c.verified = false,
                _ => {}
            }
            let _ = events.send(Event::ContactUpdated(c.clone()));
        }
        Cmd::Mesh(p) if p.kind == mesh::ACK => {
            for l in live.iter_mut() {
                let Some(key) = l.session.mesh_acked(&p.tag) else { continue };
                if let Some(pos) = l.pending.iter().position(|(_, k, ..)| *k == key) {
                    let (wire_id, ..) = l.pending.remove(pos);
                    let _ = store.set_delivered(l.contact.id, &wire_id);
                    let _ = events.send(Event::Delivered { contact_id: l.contact.id, wire_id });
                }
                return;
            }
        }
        Cmd::Mesh(p) => {
            let Some(l) = live.iter_mut().find(|l| l.session.mesh_tags().contains(&p.tag)) else {
                return;
            };
            let Some((item, ack)) = l.session.open_mesh(&p.body) else { return };
            MESH.send(mesh::Packet { kind: mesh::ACK, ttl: mesh::TTL, tag: ack, body: Vec::new() });
            if let Some(item) = item {
                handle_item(item, l, store, me, events, jobs);
            }
        }
        Cmd::DeleteMsg { contact_id, wire_id, everyone } => {
            let Some((mine, _)) = store.msg_meta(contact_id, &wire_id) else { return };
            if everyone {
                if !mine {
                    return send_err("you can only delete your own messages for everyone".into());
                }
                let Some(l) = live.iter_mut().find(|l| l.contact.id == contact_id) else { return };
                if let Err(e) = l.session.delete_for_everyone(&wire_id) {
                    return send_err(format!("delete for everyone: {e}"));
                }
                l.pending.retain(|(w, ..)| *w != wire_id);
            }
            let _ = store.delete_msg(contact_id, &wire_id);
            let _ = events.send(Event::Unsent { contact_id, wire_id });
        }
    }
}

fn cmd_group_id(cmd: &Cmd) -> Option<i64> {
    let id = match cmd {
        Cmd::Rename { contact_id, .. }
        | Cmd::Send { contact_id, .. }
        | Cmd::SendFile { contact_id, .. }
        | Cmd::React { contact_id, .. }
        | Cmd::SetTimer { contact_id, .. }
        | Cmd::Unsend { contact_id, .. }
        | Cmd::DeleteMsg { contact_id, .. }
        | Cmd::MarkRead { contact_id }
        | Cmd::DeleteContact { contact_id }
        | Cmd::ClearHistory { contact_id, .. } => *contact_id,
        _ => return None,
    };
    (id < 0).then_some(-id)
}

fn handle_group_cmd(
    cmd: Cmd,
    group_id: i64,
    store: &Store,
    groups: &mut Vec<GroupLive>,
    events: &std::sync::mpsc::Sender<Event>,
) {
    let send_err = |e: String| {
        let _ = events.send(Event::Error(e));
    };
    let cid = -group_id;
    match cmd {
        Cmd::Rename { name, .. } => {
            let _ = store.rename_group(group_id, &name);
        }
        Cmd::Send { wire_id, text, .. } => {
            let Some(gl) = groups.iter().find(|g| g.group.id == group_id) else { return };
            match gl.send(&wire_id, &text) {
                Ok(()) => {
                    let _ = events.send(Event::Sent { contact_id: cid, wire_id: wire_id.clone() });
                    let _ = store.add_msg(
                        cid,
                        &StoredMsg {
                            wire_id,
                            mine: true,
                            text,
                            file_path: None,
                            ts_ms: crate::store::chrono_now_ms(),
                            delivered: false,
                            reaction: None,
                            sender_seat: None,
                            read_ms: None,
                            pay: None,
                        },
                    );
                }
                Err(error) => {
                    let _ = events.send(Event::SendFailed { contact_id: cid, wire_id, error });
                }
            }
        }
        Cmd::React { ref_id, emoji, .. } => {
            let Some(gl) = groups.iter().find(|g| g.group.id == group_id) else { return };
            match gl.react(&ref_id, &emoji) {
                Ok(()) => {
                    let _ = store.set_reaction(cid, &ref_id, &emoji);
                }
                Err(e) => send_err(e),
            }
        }
        Cmd::ClearHistory { older_than_secs, .. } => {
            let before =
                older_than_secs.map(|s| crate::store::chrono_now_ms() - (s as i64) * 1000);
            let _ = store.clear_history(cid, before);
            let msgs = store.messages(cid).unwrap_or_default();
            let _ = events.send(Event::History { contact_id: cid, msgs });
        }
        Cmd::DeleteContact { .. } => {
            if let Some(pos) = groups.iter().position(|g| g.group.id == group_id) {
                let gl = groups.remove(pos);
                // Creator retires the mesh; members just leave locally.
                if gl.group.my_seat == 0 {
                    if let Some(p) = Provisioner::from_settings() {
                        for i in 0..gl.group.size {
                            for j in 0..gl.group.size {
                                if i != j {
                                    let _ = p.delete_bucket(&format!(
                                        "g-{}-{}-{}",
                                        gl.group.gid, i, j
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            let _ = store.delete_group(group_id);
            let _ = events.send(Event::ContactRemoved { contact_id: cid });
        }
        Cmd::SendFile { .. } => send_err("files in groups are coming soon".into()),
        Cmd::Unsend { .. } => send_err("unsend in groups is coming soon".into()),
        // Groups have no receipts yet; just keep local read state.
        Cmd::MarkRead { .. } => {
            let _ = store.mark_incoming_read(cid);
        }
        Cmd::DeleteMsg { everyone: true, .. } => {
            send_err("delete for everyone in groups is coming soon".into())
        }
        Cmd::DeleteMsg { wire_id, .. } => {
            let _ = store.delete_msg(cid, &wire_id);
            let _ = events.send(Event::Unsent { contact_id: cid, wire_id });
        }
        Cmd::SetTimer { .. } => send_err("disappearing timer sync in groups is coming soon".into()),
        _ => {}
    }
}

fn handle_item(
    item: ChatItem,
    l: &mut Live,
    store: &Store,
    me: &Identity,
    events: &std::sync::mpsc::Sender<Event>,
    jobs: &crate::wallet::Jobs,
) {
    let cid = l.contact.id;
    // Blocked: everything they send is drained unseen (so the block doesn't
    // show as "never delivered"), attachment blobs included.
    if l.contact.blocked {
        match &item {
            ChatItem::Hello { .. } | ChatItem::KeyChanged => {}
            ChatItem::Attachment { att_key, .. } => return l.session.discard(att_key),
            _ => return,
        }
    }
    match item {
        ChatItem::Hello { pk } => {
            if l.session.set_peer_pk(&pk).is_ok() {
                let _ = store.set_peer_pk(cid, &pk);
                l.contact.peer_pk = pk;
                l.contact.invite_link.clear();
                l.contact = with_safety(l.contact.clone(), me);
                let _ = events.send(Event::PeerJoined { contact_id: cid });
                let _ = events.send(Event::ContactUpdated(l.contact.clone()));
            }
        }
        ChatItem::KeyChanged => {
            if !l.contact.key_changed {
                let _ = store.set_mark(cid, Mark::KeyChanged, true);
                l.contact.key_changed = true;
                l.contact.verified = false;
                let _ = events.send(Event::ContactUpdated(l.contact.clone()));
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
                sender_seat: None,
                read_ms: None,
                pay: None,
            };
            let _ = store.add_msg(cid, &msg);
            let _ = events.send(Event::Received { contact_id: cid, msg });
        }
        ChatItem::Attachment { id, name, att_key } => match l.session.fetch_attachment(&att_key, &name) {
            Ok(path) => {
                let msg = StoredMsg {
                    wire_id: id,
                    mine: false,
                    text: pretty_file_name(&name),
                    file_path: Some(path),
                    ts_ms: crate::store::chrono_now_ms(),
                    delivered: false,
                    reaction: None,
                    sender_seat: None,
                    read_ms: None,
                    pay: None,
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
        // Peer-controlled: apply only a valid timer.
        ChatItem::Timer { secs } if crate::demo::disappear_ok(secs) => {
            let mut s = crate::demo::Settings::load();
            s.disappear_secs = secs;
            s.save();
            let _ = events.send(Event::TimerChanged { contact_id: cid, secs });
        }
        ChatItem::Timer { .. } => {}
        // Reciprocal: with receipts off we neither send nor record them.
        // Peer-controlled: set_read only matches our own messages here.
        ChatItem::Tao(v) => crate::wallet::handle_item(v, l, store, events, jobs),
        ChatItem::Read { refs } => {
            if !crate::demo::Settings::load().read_receipts {
                return;
            }
            let ts_ms = crate::store::chrono_now_ms();
            let wire_ids: Vec<String> =
                refs.into_iter().filter(|r| store.set_read(cid, r, ts_ms).unwrap_or(false)).collect();
            if !wire_ids.is_empty() {
                l.pending.retain(|(w, ..)| !wire_ids.contains(w)); // read implies delivered
                let _ = events.send(Event::Read { contact_id: cid, wire_ids, ts_ms });
            }
        }
        // Peer-controlled: only ever removes a message *they* sent in this
        // chat, plus the attachment copy we saved for it.
        ChatItem::Delete { ref_id } => {
            let Some((false, file)) = store.msg_meta(cid, &ref_id) else { return };
            remove_saved_file(file);
            let _ = store.delete_msg(cid, &ref_id);
            let _ = events.send(Event::Unsent { contact_id: cid, wire_id: ref_id });
        }
    }
}

// ---------------------------------------------------------------------------
// Groups (v1): N members, full pairwise mesh of queue buckets minted by the
// creator in one shot. Fan-out encryption per recipient; no group server; the
// network never learns a group exists (buckets look like any other queues).
// Seat discovery: joining members announce their public key as a plaintext
// `ghello` control object (a public key is public; messages stay sealed).
// v1 scope: text + reactions; files/receipts/unsend land later.
// ---------------------------------------------------------------------------

pub const SCHEME_G1: &str = "sanctum://g1/";

#[derive(Serialize, Deserialize, Clone)]
pub struct GroupInvite {
    pub gid: String,
    pub name: String,
    pub n: i64,
    pub seat: i64,
    pub k: String,
    pub s: String,
    /// Creator's public key (seat 0), so seat 0 never needs a hello.
    pub pk0: String,
    pub e: String,
}

impl GroupInvite {
    pub fn to_link(&self) -> String {
        format!("{SCHEME_G1}{}", B64.encode(serde_json::to_vec(self).expect("serializes")))
    }

    pub fn from_link(link: &str) -> Result<Self, String> {
        let payload = link.trim().strip_prefix(SCHEME_G1).ok_or("not a sanctum group invite")?;
        let json = B64.decode(payload).map_err(|e| e.to_string())?;
        serde_json::from_slice(&json).map_err(|e| e.to_string())
    }
}

fn g_bucket(gid: &str, from: i64, to: i64) -> String {
    format!("g-{gid}-{from}-{to}")
}

/// Creator: mint the whole mesh (N·(N-1) buckets, N seat tokens). Returns the
/// group row (seat 0) and one invite link per other seat.
pub fn create_group(
    p: &Provisioner,
    me: &Identity,
    name: &str,
    size: i64,
) -> Result<(crate::store::Group, Vec<String>), String> {
    let size = size.clamp(2, 6);
    let gid = ulid::Ulid::new().to_string().to_lowercase();
    for i in 0..size {
        for j in 0..size {
            if i != j {
                p.create_bucket(&g_bucket(&gid, i, j)).map_err(|e| e.to_string())?;
            }
        }
    }
    let seat_buckets = |seat: i64| -> Vec<String> {
        (0..size)
            .filter(|&j| j != seat)
            .flat_map(|j| [g_bucket(&gid, seat, j), g_bucket(&gid, j, seat)])
            .collect()
    };
    let mut tokens = Vec::new();
    for seat in 0..size {
        let buckets = seat_buckets(seat);
        let refs: Vec<&str> = buckets.iter().map(String::as_str).collect();
        tokens.push(
            p.create_sub_token_multi(&refs, "sanctum-group").map_err(|e| e.to_string())?,
        );
    }
    let pk0 = B64.encode(me.public().as_bytes());
    let mut links = Vec::new();
    for seat in 1..size {
        let (k, s) = tokens[seat as usize].clone();
        links.push(
            GroupInvite {
                gid: gid.clone(),
                name: name.into(),
                n: size,
                seat,
                k,
                s,
                pk0: pk0.clone(),
                e: crate::transport::DEFAULT_ENDPOINT.into(),
            }
            .to_link(),
        );
    }
    let (k0, s0) = tokens[0].clone();
    let group = crate::store::Group {
        id: 0,
        gid,
        name: name.into(),
        my_seat: 0,
        size,
        access_key: k0,
        secret: s0,
        endpoint: crate::transport::DEFAULT_ENDPOINT.into(),
        invite_links: links.join("\n"),
        members: vec![(0, pk0)],
    };
    Ok((group, links))
}

/// Acceptor side: adopt the seat. The worker announces our hello after the
/// store row exists.
pub fn accept_group(link: &str, _me: &Identity) -> Result<crate::store::Group, String> {
    let inv = GroupInvite::from_link(link)?;
    Ok(crate::store::Group {
        id: 0,
        gid: inv.gid,
        name: inv.name,
        my_seat: inv.seat,
        size: inv.n,
        access_key: inv.k,
        secret: inv.s,
        endpoint: inv.e,
        invite_links: String::new(),
        members: vec![(0, inv.pk0)],
    })
}

pub struct GroupLive {
    pub group: crate::store::Group,
    me_secret: String,
    /// seat -> peer public key.
    peers: std::collections::HashMap<i64, PublicKey>,
    /// (from-seat, inbox) and (to-seat, outbox) mailboxes.
    boxes_in: Vec<(i64, Mailbox)>,
    boxes_out: Vec<(i64, Mailbox)>,
}

pub enum GroupItem {
    NewPeer { seat: i64, pk: String },
    Message { seat: i64, id: String, text: String },
    Reaction { ref_id: String, emoji: String },
}

impl GroupLive {
    pub fn new(g: &crate::store::Group, me: &Identity) -> Result<Self, String> {
        let mut peers = std::collections::HashMap::new();
        for (seat, pk) in &g.members {
            if *seat != g.my_seat {
                let bytes: [u8; 32] = B64
                    .decode(pk)
                    .map_err(|e| e.to_string())?
                    .try_into()
                    .map_err(|_| "bad member pk")?;
                peers.insert(*seat, PublicKey::from_bytes(bytes));
            }
        }
        let mut boxes_in = Vec::new();
        let mut boxes_out = Vec::new();
        for j in 0..g.size {
            if j == g.my_seat {
                continue;
            }
            boxes_in.push((
                j,
                Mailbox::new(&g.endpoint, &g_bucket(&g.gid, j, g.my_seat), &g.access_key, &g.secret)
                    .map_err(|e| e.to_string())?,
            ));
            boxes_out.push((
                j,
                Mailbox::new(&g.endpoint, &g_bucket(&g.gid, g.my_seat, j), &g.access_key, &g.secret)
                    .map_err(|e| e.to_string())?,
            ));
        }
        Ok(Self { group: g.clone(), me_secret: me.to_b64(), peers, boxes_in, boxes_out })
    }

    fn me(&self) -> Identity {
        Identity::from_b64(&self.me_secret).expect("identity round-trips")
    }

    /// Announce our pk to specific seats (plaintext ghello control object).
    pub fn announce_to(&self, seats: &[i64]) {
        let body = serde_json::json!({
            "t": "ghello",
            "pk": B64.encode(self.me().public().as_bytes()),
            "s": self.group.my_seat,
        })
        .to_string();
        for (seat, out) in &self.boxes_out {
            if seats.contains(seat) {
                let _ = out.put(&format!("m/{}", ulid::Ulid::new()), body.as_bytes());
            }
        }
    }

    pub fn announce_all(&self) {
        let all: Vec<i64> = self.boxes_out.iter().map(|(s, _)| *s).collect();
        self.announce_to(&all);
    }

    pub fn register_peer(&mut self, seat: i64, pk_b64: &str) -> bool {
        if self.peers.contains_key(&seat) || seat == self.group.my_seat {
            return false;
        }
        let Ok(bytes) = B64.decode(pk_b64) else { return false };
        let Ok(arr) = <[u8; 32]>::try_from(bytes.as_slice()) else { return false };
        self.peers.insert(seat, PublicKey::from_bytes(arr));
        true
    }

    pub fn send(&self, wire_id: &str, text: &str) -> Result<(), String> {
        let body =
            serde_json::json!({ "t": "m", "id": wire_id, "x": text }).to_string();
        self.fan_out(&body)
    }

    pub fn react(&self, ref_id: &str, emoji: &str) -> Result<(), String> {
        let body = serde_json::json!({ "t": "r", "ref": ref_id, "e": emoji }).to_string();
        self.fan_out(&body)
    }

    fn fan_out(&self, body: &str) -> Result<(), String> {
        let me = self.me();
        for (seat, out) in &self.boxes_out {
            let Some(pk) = self.peers.get(seat) else { continue };
            let sealed = seal(&me, pk, body.as_bytes()).map_err(|e| e.to_string())?;
            out.put(&format!("m/{}", ulid::Ulid::new()), &sealed).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn poll(&self) -> Vec<GroupItem> {
        let me = self.me();
        let mut out = Vec::new();
        for (seat, mbox) in &self.boxes_in {
            let Ok(keys) = mbox.list("m/") else { continue };
            for key in keys {
                let Ok(raw) = mbox.get(&key) else { continue };
                // Plaintext ghello first; everything else is sealed.
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) {
                    if v.get("t").and_then(|t| t.as_str()) == Some("ghello") {
                        let pk = v.get("pk").and_then(|x| x.as_str()).unwrap_or_default();
                        let s = v.get("s").and_then(|x| x.as_i64()).unwrap_or(-1);
                        out.push(GroupItem::NewPeer { seat: s, pk: pk.into() });
                        let _ = mbox.delete(&key);
                        continue;
                    }
                }
                let Ok((_, pt)) = open(&me, &raw) else { continue };
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&pt) {
                    let s = |k: &str| {
                        v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string()
                    };
                    match v.get("t").and_then(|t| t.as_str()) {
                        Some("m") => out.push(GroupItem::Message {
                            seat: *seat,
                            id: s("id"),
                            text: s("x"),
                        }),
                        Some("r") => {
                            out.push(GroupItem::Reaction { ref_id: s("ref"), emoji: s("e") })
                        }
                        _ => {}
                    }
                }
                let _ = mbox.delete(&key);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wipe_retires_running_workers() {
        let epoch = crate::epoch();
        let worker = std::thread::spawn(move || {
            crate::set_born(epoch);
            let before = crate::data_dir().is_some();
            crate::bump_epoch(); // what wipe() does first
            (before, crate::stale(), crate::data_dir())
        });
        assert_eq!(worker.join().unwrap(), (true, true, None));
        assert!(!crate::stale() && crate::data_dir().is_some(), "UI threads are unaffected");
    }

    #[test]
    fn mesh_carries_a_message_and_its_ack() {
        // No network here: a proxy that is never dialed.
        std::env::set_var("SANCTUM_PROXY", "socks5://127.0.0.1:9");
        let (alice, bob) = (Identity::generate(), Identity::generate());
        let (ra, bundle) = Ratchet::offer(&alice).unwrap();
        let rb = Ratchet::accept(&bob, &bundle, &alice.public()).unwrap();
        let contact = |peer: &Identity| Contact {
            id: 0,
            name: "x".into(),
            bucket_in: "q-in".into(),
            bucket_out: "q-out".into(),
            access_key: "k".into(),
            secret: "s".into(),
            peer_pk: B64.encode(peer.public().as_bytes()),
            endpoint: crate::transport::DEFAULT_ENDPOINT.into(),
            invite_link: String::new(),
            verified: false,
            blocked: false,
            favorite: false,
            key_changed: false,
            safety: String::new(),
        };
        let mut a = ContactSession::new(&contact(&bob), &alice, Some(ra)).unwrap();
        let mut b = ContactSession::new(&contact(&alice), &bob, Some(rb)).unwrap();
        MESH.set_neighbors(1);

        let key = b.send("W1", "hi over bluetooth").unwrap();
        assert!(!b.is_delivered(&key).unwrap(), "waits for the ack");
        // The outbox survives a restart.
        let mut b2 = ContactSession::new(&contact(&alice), &bob, None).unwrap();
        b2.restore_outbox(&b.outbox_update().unwrap());
        assert!(!b2.is_delivered(&key).unwrap());

        let p = mesh::Packet::decode(&MESH.drain().pop().unwrap()).unwrap();
        assert!(a.mesh_tags().contains(&p.tag) && !b.mesh_tags().contains(&p.tag));
        let (item, ack) = a.open_mesh(&p.body).unwrap();
        assert!(matches!(item, Some(ChatItem::Message { text, .. }) if text == "hi over bluetooth"));
        assert!(a.open_mesh(&p.body).is_none(), "replay rejected");
        assert_eq!(b.mesh_acked(&[0; 16]), None);
        assert_eq!(b.mesh_acked(&ack), Some(key));
    }
}
