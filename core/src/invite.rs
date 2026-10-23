//! Capability invites: everything a stranger needs to message you, in one link.

use crate::Error;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use serde::{Deserialize, Serialize};

pub const SCHEME: &str = "sanctum://v1/";

/// The invite IS the sender's credentials: an inbox bucket, a sub-token
/// scoped to only that bucket, and the recipient's public key.
#[derive(Serialize, Deserialize, Clone)]
pub struct Invite {
    /// inbox bucket name (globally unique, unguessable)
    pub b: String,
    /// sub-token access key (hip_...)
    pub k: String,
    /// sub-token secret
    pub s: String,
    /// recipient identity public key, b64url
    pub pk: String,
    /// S3 endpoint (lets power users point at their own gateway)
    pub e: String,
}

impl Invite {
    pub fn to_link(&self) -> String {
        let json = serde_json::to_vec(self).expect("invite serializes");
        format!("{SCHEME}{}", B64.encode(json))
    }

    pub fn from_link(link: &str) -> Result<Self, Error> {
        let payload = link
            .strip_prefix(SCHEME)
            .ok_or_else(|| Error::Encoding("not a sanctum v1 invite".into()))?;
        let json = B64.decode(payload).map_err(|e| Error::Encoding(e.to_string()))?;
        serde_json::from_slice(&json).map_err(|e| Error::Encoding(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_roundtrip() {
        let inv = Invite {
            b: "inbox-abc".into(),
            k: "hip_x".into(),
            s: "sec".into(),
            pk: "pk".into(),
            e: "https://s3.hippius.com".into(),
        };
        let back = Invite::from_link(&inv.to_link()).unwrap();
        assert_eq!(back.b, "inbox-abc");
        assert_eq!(back.k, "hip_x");
    }
}
