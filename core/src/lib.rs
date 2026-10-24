//! sanctum-core — identity, capability invites, E2E envelopes, Hippius dead-drop transport.
//!
//! Protocol (SPEC-V2-DEADDROP.md): a contact invite carries the capability to
//! write into one inbox bucket (a Hippius sub-token scoped to it) plus the
//! recipient's public key. Senders need no account of any kind.

#[cfg(feature = "ffi")]
pub mod api;

#[cfg(feature = "ffi")]
uniffi::setup_scaffolding!();
pub mod contact;
pub mod demo;
pub mod invite;
pub mod provision;
pub mod store;
pub mod transport;

pub use crypto_box;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use crypto_box::{
    aead::{Aead, AeadCore, OsRng},
    PublicKey, SalsaBox, SecretKey,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("transport: {0}")]
    Transport(String),
    #[error("crypto: {0}")]
    Crypto(String),
    #[error("encoding: {0}")]
    Encoding(String),
}

/// A user identity: one X25519 keypair. v1 envelopes use it directly as a
/// static box key; v2 (`Ratchet`) uses the same key as the Signal identity.
pub struct Identity {
    pub secret: SecretKey,
}

impl Identity {
    pub fn generate() -> Self {
        Self { secret: SecretKey::generate(&mut OsRng) }
    }

    pub fn public(&self) -> PublicKey {
        self.secret.public_key()
    }

    pub fn to_b64(&self) -> String {
        B64.encode(self.secret.to_bytes())
    }

    pub fn from_b64(s: &str) -> Result<Self, Error> {
        let bytes: [u8; 32] = B64
            .decode(s)
            .map_err(|e| Error::Encoding(e.to_string()))?
            .try_into()
            .map_err(|_| Error::Encoding("bad key length".into()))?;
        Ok(Self { secret: SecretKey::from_bytes(bytes) })
    }
}

/// Wire envelope, serialized as JSON inside the dead-drop object.
#[derive(Serialize, Deserialize)]
pub struct Envelope {
    pub v: u8,
    /// sender's public key, b64url
    pub from: String,
    /// nonce, b64url
    pub n: String,
    /// ciphertext, b64url
    pub c: String,
}

pub fn seal(sender: &Identity, recipient_pk: &PublicKey, plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    let bx = SalsaBox::new(recipient_pk, &sender.secret);
    let nonce = SalsaBox::generate_nonce(&mut OsRng);
    let ct = bx.encrypt(&nonce, plaintext).map_err(|e| Error::Crypto(e.to_string()))?;
    let env = Envelope {
        v: 1,
        from: B64.encode(sender.public().as_bytes()),
        n: B64.encode(nonce),
        c: B64.encode(ct),
    };
    serde_json::to_vec(&env).map_err(|e| Error::Encoding(e.to_string()))
}

/// Opens an envelope; returns (sender public key, plaintext).
pub fn open(recipient: &Identity, raw: &[u8]) -> Result<(PublicKey, Vec<u8>), Error> {
    let env: Envelope = serde_json::from_slice(raw).map_err(|e| Error::Encoding(e.to_string()))?;
    if env.v != 1 {
        return Err(Error::Encoding(format!("unsupported envelope version {}", env.v)));
    }
    let from_bytes: [u8; 32] = B64
        .decode(&env.from)
        .map_err(|e| Error::Encoding(e.to_string()))?
        .try_into()
        .map_err(|_| Error::Encoding("bad sender key".into()))?;
    let from = PublicKey::from_bytes(from_bytes);
    let nonce_bytes = B64.decode(&env.n).map_err(|e| Error::Encoding(e.to_string()))?;
    let ct = B64.decode(&env.c).map_err(|e| Error::Encoding(e.to_string()))?;
    let bx = SalsaBox::new(&from, &recipient.secret);
    let pt = bx
        .decrypt(nonce_bytes.as_slice().into(), ct.as_slice())
        .map_err(|_| Error::Crypto("decryption failed".into()))?;
    Ok((from, pt))
}

/// Envelope version of a raw dead-drop object (0 if it isn't one).
pub fn version(raw: &[u8]) -> u8 {
    #[derive(Deserialize)]
    struct V {
        v: u8,
    }
    serde_json::from_slice::<V>(raw).map(|e| e.v).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// v2 envelopes: libsignal PQXDH + double ratchet (forward secrecy, post-
// compromise security, post-quantum handshake). One `Ratchet` per contact,
// each with its own libsignal store, persisted as one opaque blob. Mirrors
// libsignal's rust/protocol/tests/session.rs flow.
// ---------------------------------------------------------------------------

use libsignal_protocol as sig;
use sig::{
    GenericSignedPreKey, IdentityKeyStore, KyberPreKeyStore, PreKeyStore, SessionStore,
    SignedPreKeyStore,
};

/// Each per-contact store holds exactly one device and one prekey of each
/// kind, so ids are constants rather than allocated.
const ONE: u32 = 1;

/// The bundle an inviter publishes for the acceptor (libsignal's
/// `PreKeyBundle` has no wire format of its own). All fields b64url.
#[derive(Serialize, Deserialize)]
struct Bundle {
    ik: String,
    pre: String,
    spk: String,
    spk_sig: String,
    kpk: String,
    kpk_sig: String,
}

/// Persisted ratchet state. Prekeys live only until a session exists.
#[derive(Serialize, Deserialize)]
struct Saved {
    session: Option<String>,
    peer: Option<String>,
    prekeys: Option<[String; 3]>,
}

#[derive(Serialize, Deserialize)]
struct Envelope2 {
    v: u8,
    /// libsignal message type: 3 = PreKey (handshake), 2 = Whisper.
    t: u8,
    c: String,
}

pub struct Ratchet {
    store: sig::InMemSignalProtocolStore,
}

fn sig_err(e: impl std::fmt::Display) -> Error {
    Error::Crypto(e.to_string())
}

fn b64d(s: &str) -> Result<Vec<u8>, Error> {
    B64.decode(s).map_err(|e| Error::Encoding(e.to_string()))
}

fn rng() -> impl rand::CryptoRng {
    use rand::TryRngCore;
    rand::rngs::OsRng.unwrap_err()
}

fn peer_addr() -> sig::ProtocolAddress {
    sig::ProtocolAddress::new("peer".into(), sig::DeviceId::new(ONE as u8).expect("valid id"))
}

fn local_addr() -> sig::ProtocolAddress {
    sig::ProtocolAddress::new("me".into(), sig::DeviceId::new(ONE as u8).expect("valid id"))
}

impl Ratchet {
    fn empty(me: &Identity) -> Result<Self, Error> {
        let private = sig::PrivateKey::deserialize(&me.secret.to_bytes()).map_err(sig_err)?;
        let public = private.public_key().map_err(sig_err)?;
        let ik = sig::IdentityKeyPair::new(sig::IdentityKey::new(public), private);
        Ok(Self { store: sig::InMemSignalProtocolStore::new(ik, ONE).map_err(sig_err)? })
    }

    /// Inviter: mint one-time prekeys (EC + Kyber1024, both signed by the
    /// identity). Returns the ratchet and the bundle bytes to publish.
    pub fn offer(me: &Identity) -> Result<(Self, Vec<u8>), Error> {
        use futures_executor::block_on;
        let mut r = Self::empty(me)?;
        let mut rng = rng();
        let ik = block_on(r.store.get_identity_key_pair()).map_err(sig_err)?;
        let pre = sig::KeyPair::generate(&mut rng);
        let spk = sig::KeyPair::generate(&mut rng);
        let kpk = sig::kem::KeyPair::generate(sig::kem::KeyType::Kyber1024, &mut rng);
        let mut sign = |public: &[u8]| ik.private_key().calculate_signature(public, &mut rng).map_err(sig_err);
        let spk_sig = sign(&spk.public_key.serialize())?;
        let kpk_sig = sign(&kpk.public_key.serialize())?;
        let now = sig::Timestamp::from_epoch_millis(store::chrono_now_ms() as u64);
        block_on(async {
            r.store.save_pre_key(ONE.into(), &sig::PreKeyRecord::new(ONE.into(), &pre)).await?;
            let spk_rec = sig::SignedPreKeyRecord::new(ONE.into(), now, &spk, &spk_sig);
            r.store.save_signed_pre_key(ONE.into(), &spk_rec).await?;
            let kpk_rec = sig::KyberPreKeyRecord::new(ONE.into(), now, &kpk, &kpk_sig);
            r.store.save_kyber_pre_key(ONE.into(), &kpk_rec).await
        })
        .map_err(sig_err)?;
        let bundle = Bundle {
            ik: B64.encode(ik.identity_key().serialize()),
            pre: B64.encode(pre.public_key.serialize()),
            spk: B64.encode(spk.public_key.serialize()),
            spk_sig: B64.encode(spk_sig),
            kpk: B64.encode(kpk.public_key.serialize()),
            kpk_sig: B64.encode(kpk_sig),
        };
        let bytes = serde_json::to_vec(&bundle).map_err(|e| Error::Encoding(e.to_string()))?;
        Ok((r, bytes))
    }

    /// Acceptor: run PQXDH against the inviter's bundle. The bundle must
    /// belong to `inviter` — the key the invite link carried out of band —
    /// so whoever can write the bucket can't substitute their own.
    pub fn accept(me: &Identity, bundle: &[u8], inviter: &PublicKey) -> Result<Self, Error> {
        let b: Bundle = serde_json::from_slice(bundle).map_err(|e| Error::Encoding(e.to_string()))?;
        let ik = sig::IdentityKey::decode(&b64d(&b.ik)?).map_err(sig_err)?;
        if ik.public_key().public_key_bytes() != inviter.as_bytes() {
            return Err(Error::Crypto("key bundle is not from the inviter".into()));
        }
        let bundle = sig::PreKeyBundle::new(
            ONE,
            sig::DeviceId::new(ONE as u8).expect("valid id"),
            Some((ONE.into(), sig::PublicKey::deserialize(&b64d(&b.pre)?).map_err(sig_err)?)),
            ONE.into(),
            sig::PublicKey::deserialize(&b64d(&b.spk)?).map_err(sig_err)?,
            b64d(&b.spk_sig)?,
            ONE.into(),
            sig::kem::PublicKey::deserialize(&b64d(&b.kpk)?).map_err(sig_err)?,
            b64d(&b.kpk_sig)?,
            ik,
        )
        .map_err(sig_err)?;
        let mut r = Self::empty(me)?;
        let s = &mut r.store;
        futures_executor::block_on(sig::process_prekey_bundle(
            &peer_addr(),
            &local_addr(),
            &mut s.session_store,
            &mut s.identity_store,
            &bundle,
            std::time::SystemTime::now(),
            &mut rng(),
        ))
        .map_err(sig_err)?;
        Ok(r)
    }

    pub fn load(me: &Identity, blob: &str) -> Result<Self, Error> {
        let saved: Saved = serde_json::from_str(blob).map_err(|e| Error::Encoding(e.to_string()))?;
        let mut r = Self::empty(me)?;
        let s = &mut r.store;
        let addr = peer_addr();
        futures_executor::block_on(async {
            if let Some(x) = &saved.session {
                s.store_session(&addr, &sig::SessionRecord::deserialize(&b64d(x)?).map_err(sig_err)?)
                    .await
                    .map_err(sig_err)?;
            }
            if let Some(x) = &saved.peer {
                s.save_identity(&addr, &sig::IdentityKey::decode(&b64d(x)?).map_err(sig_err)?)
                    .await
                    .map_err(sig_err)?;
            }
            if let Some([pre, spk, kpk]) = &saved.prekeys {
                let pre = sig::PreKeyRecord::deserialize(&b64d(pre)?).map_err(sig_err)?;
                let spk = sig::SignedPreKeyRecord::deserialize(&b64d(spk)?).map_err(sig_err)?;
                let kpk = sig::KyberPreKeyRecord::deserialize(&b64d(kpk)?).map_err(sig_err)?;
                s.save_pre_key(ONE.into(), &pre).await.map_err(sig_err)?;
                s.save_signed_pre_key(ONE.into(), &spk).await.map_err(sig_err)?;
                s.save_kyber_pre_key(ONE.into(), &kpk).await.map_err(sig_err)?;
            }
            Ok::<_, Error>(())
        })?;
        Ok(r)
    }

    pub fn save(&self) -> Result<String, Error> {
        let s = &self.store;
        let addr = peer_addr();
        let saved = futures_executor::block_on(async {
            let session = s.load_session(&addr).await?;
            // Once a session exists the prekeys are spent: dropping them
            // makes a replayed handshake undecryptable.
            let prekeys = match session {
                Some(_) => None,
                None => Some([
                    B64.encode(s.get_pre_key(ONE.into()).await?.serialize()?),
                    B64.encode(s.get_signed_pre_key(ONE.into()).await?.serialize()?),
                    B64.encode(s.get_kyber_pre_key(ONE.into()).await?.serialize()?),
                ]),
            };
            Ok::<_, sig::SignalProtocolError>(Saved {
                session: session.map(|x| x.serialize().map(|b| B64.encode(b))).transpose()?,
                peer: s.get_identity(&addr).await?.map(|k| B64.encode(k.serialize())),
                prekeys,
            })
        })
        .map_err(sig_err)?;
        serde_json::to_string(&saved).map_err(|e| Error::Encoding(e.to_string()))
    }

    /// A session exists (acceptor: always; inviter: once the peer's first
    /// message has been opened).
    pub fn ready(&self) -> bool {
        matches!(futures_executor::block_on(self.store.load_session(&peer_addr())), Ok(Some(_)))
    }

    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, Error> {
        let s = &mut self.store;
        let msg = futures_executor::block_on(sig::message_encrypt(
            plaintext,
            &peer_addr(),
            &local_addr(),
            &mut s.session_store,
            &mut s.identity_store,
            std::time::SystemTime::now(),
            &mut rng(),
        ))
        .map_err(sig_err)?;
        let env = Envelope2 { v: 2, t: msg.message_type() as u8, c: B64.encode(msg.serialize()) };
        serde_json::to_vec(&env).map_err(|e| Error::Encoding(e.to_string()))
    }

    /// Opens a v2 envelope; returns (authenticated sender key, plaintext).
    pub fn open(&mut self, raw: &[u8]) -> Result<(PublicKey, Vec<u8>), Error> {
        let env: Envelope2 = serde_json::from_slice(raw).map_err(|e| Error::Encoding(e.to_string()))?;
        if env.v != 2 {
            return Err(Error::Encoding(format!("not a v2 envelope (v{})", env.v)));
        }
        let c = b64d(&env.c)?;
        let msg = match sig::CiphertextMessageType::try_from(env.t) {
            Ok(sig::CiphertextMessageType::PreKey) => {
                sig::CiphertextMessage::PreKeySignalMessage(c.as_slice().try_into().map_err(sig_err)?)
            }
            Ok(sig::CiphertextMessageType::Whisper) => {
                sig::CiphertextMessage::SignalMessage(c.as_slice().try_into().map_err(sig_err)?)
            }
            _ => return Err(Error::Encoding(format!("unsupported message type {}", env.t))),
        };
        let s = &mut self.store;
        let addr = peer_addr();
        let pt = futures_executor::block_on(sig::message_decrypt(
            &msg,
            &addr,
            &local_addr(),
            &mut s.session_store,
            &mut s.identity_store,
            &mut s.pre_key_store,
            &s.signed_pre_key_store,
            &mut s.kyber_pre_key_store,
            &mut rng(),
        ))
        .map_err(|_| Error::Crypto("decryption failed".into()))?;
        let peer = futures_executor::block_on(s.get_identity(&addr))
            .map_err(sig_err)?
            .ok_or_else(|| Error::Crypto("no peer identity".into()))?;
        let from: [u8; 32] = peer
            .public_key()
            .public_key_bytes()
            .try_into()
            .map_err(|_| Error::Crypto("bad peer identity".into()))?;
        Ok((PublicKey::from_bytes(from), pt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_roundtrip() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let raw = seal(&bob, &alice.public(), b"hi alice").unwrap();
        let (from, pt) = open(&alice, &raw).unwrap();
        assert_eq!(from.as_bytes(), bob.public().as_bytes());
        assert_eq!(pt, b"hi alice");
        // tampering fails
        let mut bad = raw.clone();
        let len = bad.len();
        bad[len - 10] ^= 1;
        assert!(open(&alice, &bad).is_err());
    }

    #[test]
    fn ratchet_handshake_roundtrip() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let (a, bundle) = Ratchet::offer(&alice).unwrap();
        // A bundle must match the key the invite carried.
        assert!(Ratchet::accept(&bob, &bundle, &bob.public()).is_err());
        let mut b = Ratchet::accept(&bob, &bundle, &alice.public()).unwrap();
        assert!(b.ready() && !a.ready());

        let hello = b.seal(b"hello").unwrap();
        assert_eq!(version(&hello), 2);
        // Inviter state survives a restart before the handshake lands.
        let mut a = Ratchet::load(&alice, &a.save().unwrap()).unwrap();
        let (from, pt) = a.open(&hello).unwrap();
        assert_eq!((from.as_bytes(), pt.as_slice()), (bob.public().as_bytes(), &b"hello"[..]));
        assert!(a.ready());
        // Prekeys are spent: a replayed handshake no longer opens.
        let mut a = Ratchet::load(&alice, &a.save().unwrap()).unwrap();
        assert!(a.open(&hello).is_err());

        let reply = a.seal(b"hi bob").unwrap();
        let mut b = Ratchet::load(&bob, &b.save().unwrap()).unwrap();
        assert_eq!(b.open(&reply).unwrap().1, b"hi bob");
        assert!(b.open(&reply).is_err(), "duplicate rejected");
        // Out-of-order delivery within the ratchet still opens.
        let (m1, m2) = (b.seal(b"one").unwrap(), b.seal(b"two").unwrap());
        assert_eq!(a.open(&m2).unwrap().1, b"two");
        assert_eq!(a.open(&m1).unwrap().1, b"one");
        // v1 and v2 never cross.
        assert!(open(&alice, &m1).is_err());
        assert!(a.open(&seal(&bob, &alice.public(), b"x").unwrap()).is_err());
    }
}
