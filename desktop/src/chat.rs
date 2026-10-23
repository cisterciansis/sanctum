//! Demo chat session over one shared dead-drop bucket.
//!
//! Pairing: each side publishes its public key at `pk/<role>` and polls for
//! the peer's. Messages go to `to-<peer>/<ulid>`; each side polls its own
//! `to-<role>/` prefix and deletes after read. Demo ceiling: one shared
//! bucket, static keys — the real app mints one bucket per contact
//! (SPEC-V2 §4b) and upgrades crypto to libsignal.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use sanctum_core::crypto_box::PublicKey;
use sanctum_core::{open, seal, transport::Mailbox, Identity};

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
            sanctum_core::transport::DEFAULT_ENDPOINT,
            &bucket,
            &env(ak)?,
            &env(sk)?,
        )
        .map_err(|e| e.to_string())?;

        // Persist identity across runs so re-pairing isn't needed.
        let dir = std::env::var("HOME").map(|h| format!("{h}/.sanctum"))
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

    pub fn send(&self, text: &str) -> Result<(), String> {
        let pk = self.peer_pk.as_ref().ok_or("not paired yet")?;
        let sealed = seal(&self.me, pk, text.as_bytes()).map_err(|e| e.to_string())?;
        self.mbox
            .put(&format!("to-{}/{}", self.peer, ulid::Ulid::new()), &sealed)
            .map_err(|e| e.to_string())
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
