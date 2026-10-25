//! Offline carrier: a Bluetooth LE flood mesh, ported from bitchat
//! (TTL-limited flooding, per-packet dedup, per-link rate limits). The radio
//! lives in the app (CoreBluetooth on iOS); this is the radio-agnostic part.
//!
//! The mesh carries the same sealed v2 envelopes Hippius does, never a new
//! trust path. A relay sees a version, kind, TTL, a 16-byte tag and
//! ciphertext. Tags are HMACs under a per-pair key (X25519 of the two
//! identity keys) that rotate every 10 minutes, so relays can't link a chat
//! across epochs; ack tokens are HMACs over the envelope, which relays can't
//! forge. Every device relays everything, recipients included, so a relay
//! can't tell who a packet stopped at.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::crypto_box::PublicKey;
use crate::Identity;

pub const ENVELOPE: u8 = 1;
pub const ACK: u8 = 2;
const VERSION: u8 = 1;
/// bitchat's flood depth; dense neighborhoods clamp to `TTL_DENSE`.
pub const TTL: u8 = 7;
const TTL_DENSE: u8 = 5;
const DENSE: usize = 6;
/// version, kind, ttl, tag.
pub const HEADER: usize = 3 + 16;
/// Largest packet carried; bigger envelopes go over Hippius only.
pub const MAX_PACKET: usize = 8 * 1024;
const FIRST: u8 = 1;
const LAST: u8 = 2;
/// Dedup window: far longer than a flood takes to die out.
const SEEN_TTL: Duration = Duration::from_secs(300);
const SEEN_CAP: usize = 4096;
const LINKS_CAP: usize = 32;
const OUTBOX_CAP: usize = 64;
/// Per-link budgets (token buckets): burst, then sustained per second.
const LINK_BYTES: (f64, f64) = (32.0 * 1024.0, 8.0 * 1024.0);
const LINK_PACKETS: (f64, f64) = (20.0, 4.0);
const EPOCH_SECS: u64 = 600;

pub struct Packet {
    pub kind: u8,
    pub ttl: u8,
    pub tag: [u8; 16],
    pub body: Vec<u8>,
}

impl Packet {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(HEADER + self.body.len());
        b.extend([VERSION, self.kind, self.ttl]);
        b.extend(self.tag);
        b.extend(&self.body);
        b
    }

    /// Untrusted input: known version and kind, envelopes non-empty, acks
    /// bodyless, nothing over `MAX_PACKET`.
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < HEADER || b.len() > MAX_PACKET || b[0] != VERSION {
            return None;
        }
        let body = b[HEADER..].to_vec();
        match (b[1], body.is_empty()) {
            (ENVELOPE, false) | (ACK, true) => {}
            _ => return None,
        }
        Some(Self { kind: b[1], ttl: b[2], tag: b[3..HEADER].try_into().ok()?, body })
    }

    /// Dedup key. TTL is left out: relays rewrite it, and it isn't signed.
    fn id(&self) -> [u8; 16] {
        let h = Sha256::new().chain_update([self.kind]).chain_update(self.tag).chain_update(&self.body);
        h.finalize()[..16].try_into().expect("16 bytes")
    }
}

/// Envelope digest the receiver uses to drop the Hippius copy of something
/// the mesh already delivered.
pub fn digest(envelope: &[u8]) -> [u8; 16] {
    Sha256::digest(envelope)[..16].try_into().expect("16 bytes")
}

/// Split a packet into link frames: one flags byte (first/last) + chunk.
/// Links deliver frames in order, so one reassembly buffer per link does.
pub fn frames(packet: &[u8], mtu: usize) -> Vec<Vec<u8>> {
    let chunk = mtu.saturating_sub(1).max(19);
    let n = packet.len().div_ceil(chunk).max(1);
    (0..n)
        .map(|i| {
            let part = &packet[i * chunk..((i + 1) * chunk).min(packet.len())];
            let flags = if i == 0 { FIRST } else { 0 } | if i == n - 1 { LAST } else { 0 };
            [&[flags][..], part].concat()
        })
        .collect()
}

struct Bucket {
    tokens: f64,
    at: Instant,
}

impl Bucket {
    fn new((burst, _): (f64, f64), now: Instant) -> Self {
        Self { tokens: burst, at: now }
    }

    fn take(&mut self, n: f64, (burst, rate): (f64, f64), now: Instant) -> bool {
        self.tokens = (self.tokens + now.duration_since(self.at).as_secs_f64() * rate).min(burst);
        self.at = now;
        let ok = self.tokens >= n;
        if ok {
            self.tokens -= n;
        }
        ok
    }
}

struct Link {
    buf: Vec<u8>,
    bytes: Bucket,
    packets: Bucket,
    last: Instant,
}

#[derive(Default)]
struct Inner {
    links: HashMap<String, Link>,
    seen: HashMap<[u8; 16], Instant>,
    order: VecDeque<[u8; 16]>,
    outbox: VecDeque<Vec<u8>>,
    neighbors: usize,
}

impl Inner {
    /// True the first time a packet id is seen within the window.
    fn fresh(&mut self, id: [u8; 16], now: Instant) -> bool {
        while let Some(old) = self.order.front() {
            if self.order.len() < SEEN_CAP && now.duration_since(self.seen[old]) < SEEN_TTL {
                break;
            }
            self.seen.remove(old);
            self.order.pop_front();
        }
        if self.seen.contains_key(&id) {
            return false;
        }
        self.seen.insert(id, now);
        self.order.push_back(id);
        true
    }
}

/// One radio per process, shared by the app's BLE layer and the worker.
pub static MESH: LazyLock<Mesh> = LazyLock::new(Mesh::default);

#[derive(Default)]
pub struct Mesh(Mutex<Inner>);

impl Mesh {
    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// One frame from a link. Returns a packet when it completes one that is
    /// new, with the bytes to relay to every other link (None at TTL end).
    pub fn receive(&self, link: &str, frame: &[u8]) -> Option<(Packet, Option<Vec<u8>>)> {
        let now = Instant::now();
        let mut m = self.inner();
        if !m.links.contains_key(link) && m.links.len() >= LINKS_CAP {
            let idle = m.links.iter().min_by_key(|(_, l)| l.last).map(|(k, _)| k.clone())?;
            m.links.remove(&idle);
        }
        let l = m.links.entry(link.to_string()).or_insert_with(|| Link {
            buf: Vec::new(),
            bytes: Bucket::new(LINK_BYTES, now),
            packets: Bucket::new(LINK_PACKETS, now),
            last: now,
        });
        l.last = now;
        let (&flags, chunk) = frame.split_first()?;
        if !l.bytes.take(frame.len() as f64, LINK_BYTES, now) {
            l.buf.clear();
            return None;
        }
        if flags & FIRST != 0 {
            l.buf.clear();
        }
        if l.buf.len() + chunk.len() > MAX_PACKET {
            l.buf.clear();
            return None;
        }
        l.buf.extend_from_slice(chunk);
        if flags & LAST == 0 {
            return None;
        }
        let raw = std::mem::take(&mut l.buf);
        if !l.packets.take(1.0, LINK_PACKETS, now) {
            return None;
        }
        let mut p = Packet::decode(&raw)?;
        if !m.fresh(p.id(), now) {
            return None;
        }
        let cap = if m.neighbors >= DENSE { TTL_DENSE } else { TTL };
        let ttl = p.ttl.min(cap);
        let relay = (ttl > 1).then(|| {
            p.ttl = ttl - 1;
            p.encode()
        });
        Some((p, relay))
    }

    /// Originate a packet; the app drains it with `drain`.
    pub fn send(&self, p: Packet) {
        let mut m = self.inner();
        m.fresh(p.id(), Instant::now());
        if m.outbox.len() >= OUTBOX_CAP {
            m.outbox.pop_front();
        }
        m.outbox.push_back(p.encode());
    }

    pub fn drain(&self) -> Vec<Vec<u8>> {
        self.inner().outbox.drain(..).collect()
    }

    /// Links the radio has up right now; 0 means don't try the mesh.
    pub fn set_neighbors(&self, n: usize) {
        self.inner().neighbors = n;
    }

    pub fn neighbors(&self) -> usize {
        self.inner().neighbors
    }

    pub fn link_closed(&self, link: &str) {
        self.inner().links.remove(link);
    }
}

fn hmac16(key: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("any key length");
    parts.iter().for_each(|p| mac.update(p));
    mac.finalize().into_bytes()[..16].try_into().expect("16 bytes")
}

/// Key both ends of a chat share for mesh tags and acks.
pub fn pair_key(me: &Identity, peer: &PublicKey) -> Option<[u8; 32]> {
    use libsignal_protocol as sig;
    let sk = sig::PrivateKey::deserialize(&me.secret.to_bytes()).ok()?;
    let pk = sig::PublicKey::from_djb_public_key_bytes(peer.as_bytes()).ok()?;
    let dh = sk.calculate_agreement(&pk).ok()?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&dh).expect("any key length");
    mac.update(b"sanctum mesh v1");
    Some(mac.finalize().into_bytes().into())
}

pub fn epoch_now() -> u64 {
    crate::store::chrono_now_ms() as u64 / 1000 / EPOCH_SECS
}

/// Tag addressing `to` in this epoch (direction-bound, so the two sides of
/// a chat don't share one).
pub fn tag(key: &[u8; 32], to: &PublicKey, epoch: u64) -> [u8; 16] {
    hmac16(key, &[b"tag", to.as_bytes(), &epoch.to_be_bytes()])
}

/// Proof of delivery only the two ends can make.
pub fn ack_token(key: &[u8; 32], envelope: &[u8]) -> [u8; 16] {
    hmac16(key, &[b"ack", &digest(envelope)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(body: &[u8], ttl: u8) -> Vec<u8> {
        Packet { kind: ENVELOPE, ttl, tag: [7; 16], body: body.to_vec() }.encode()
    }

    #[test]
    fn mesh_relays_once_and_reassembles() {
        let m = Mesh::default();
        let big = vec![9u8; 3000];
        let fs = frames(&env(&big, TTL), 185);
        assert!(fs.len() > 1);
        let mut got = None;
        for f in &fs {
            got = m.receive("a", f).or(got);
        }
        let (p, relay) = got.expect("reassembled");
        assert_eq!(p.body, big);
        assert_eq!(Packet::decode(&relay.unwrap()).unwrap().ttl, TTL - 1);
        // Same packet via another link, with a rewritten TTL: still a dup.
        assert!(frames(&env(&big, 3), 512).iter().all(|f| m.receive("b", f).is_none()));
        // TTL ends the flood; forged high TTLs are clamped.
        let (_, relay) = m.receive("a", &frames(&env(b"x", 1), 512)[0]).unwrap();
        assert!(relay.is_none());
        let (_, relay) = m.receive("a", &frames(&env(b"y", 200), 512)[0]).unwrap();
        assert_eq!(Packet::decode(&relay.unwrap()).unwrap().ttl, TTL - 1);
        // Own packets echoing back aren't taken again.
        m.send(Packet { kind: ACK, ttl: TTL, tag: [1; 16], body: vec![] });
        let mine = m.drain().pop().unwrap();
        assert!(m.receive("a", &frames(&mine, 512)[0]).is_none());
    }

    #[test]
    fn mesh_rejects_junk_and_floods() {
        let m = Mesh::default();
        assert!(Packet::decode(&[VERSION, ACK, 7]).is_none());
        assert!(Packet::decode(&env(b"", 7)).is_none(), "empty envelope");
        let mut bad = env(b"x", 7);
        bad[1] = ACK;
        assert!(Packet::decode(&bad).is_none(), "ack with a body");
        // A packet over the cap never completes.
        let huge = env(&vec![0; MAX_PACKET], 7);
        assert!(frames(&huge, 512).iter().all(|f| m.receive("a", f).is_none()));
        // Distinct packets from one link: the burst passes, then it's throttled.
        let n = (0..100u8)
            .filter(|i| m.receive("b", &frames(&env(&[*i], 7), 512)[0]).is_some())
            .count();
        assert_eq!(n, LINK_PACKETS.0 as usize);
    }

    #[test]
    fn mesh_tags_and_acks_match_across_the_pair() {
        let (a, b) = (Identity::generate(), Identity::generate());
        let ka = pair_key(&a, &b.public()).unwrap();
        let kb = pair_key(&b, &a.public()).unwrap();
        assert_eq!(ka, kb);
        let e = epoch_now();
        assert_eq!(tag(&ka, &b.public(), e), tag(&kb, &b.public(), e));
        assert_ne!(tag(&ka, &b.public(), e), tag(&ka, &a.public(), e), "direction-bound");
        assert_ne!(tag(&ka, &b.public(), e), tag(&ka, &b.public(), e + 1), "rotates");
        let other = pair_key(&a, &Identity::generate().public()).unwrap();
        assert_ne!(ack_token(&ka, b"env"), ack_token(&other, b"env"));
    }
}
