//! sanctum-core — identity, capability invites, E2E envelopes, Hippius dead-drop transport.
//!
//! Protocol (SPEC-V2-DEADDROP.md): a contact invite carries the capability to
//! write into one inbox bucket (a Hippius sub-token scoped to it) plus the
//! recipient's public key. Senders need no account of any kind.

pub mod demo;
pub mod invite;
pub mod provision;
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

/// A user identity: one X25519 keypair. MVP ceiling: static keys, no
/// ratchet/PQ — upgrade path is libsignal PQXDH (envelope has a version byte).
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
}
