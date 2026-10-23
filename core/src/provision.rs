//! Optional Hippius provisioning: mint per-contact inbox buckets and
//! bucket-scoped sub-tokens through the management API.
//!
//! Strictly opt-in (SPEC-V2 §4b): the app runs fine without a token; with
//! one (Settings.hippius_api_token, from console → Settings) it unlocks
//! per-contact buckets and queue rotation. The token never leaves the
//! device. Endpoints per api.hippius.com's published OpenAPI spec.

use crate::Error;
use std::time::Duration;

const API: &str = "https://api.hippius.com/api";

pub struct Provisioner {
    token: String,
    agent: ureq::Agent,
}

impl Provisioner {
    /// None when no token is configured — callers fall back to static creds.
    pub fn from_settings() -> Option<Self> {
        let token = crate::demo::Settings::load().hippius_api_token?;
        Some(Self {
            token,
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(60))
                .user_agent(concat!("sanctum/", env!("CARGO_PKG_VERSION")))
                .build(),
        })
    }

    fn post(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value, Error> {
        let resp = self
            .agent
            .post(&format!("{API}{path}"))
            .set("Authorization", &format!("Token {}", self.token))
            .send_json(body)
            .map_err(|e| Error::Transport(format!("POST {path}: {e}")))?;
        resp.into_json().map_err(|e| Error::Encoding(e.to_string()))
    }

    /// Create a bucket; names are globally unique, so callers pass something
    /// random like `inbox-<ulid>`.
    pub fn create_bucket(&self, name: &str) -> Result<(), Error> {
        self.post("/objectstore/buckets/", serde_json::json!({ "name": name, "region": "auto" }))?;
        Ok(())
    }

    /// Mint a sub-token scoped to exactly one bucket (the invite capability).
    /// Returns (access_key_id, secret).
    pub fn create_sub_token(&self, bucket: &str, label: &str) -> Result<(String, String), Error> {
        let v = self.post(
            "/objectstore/sub-tokens/",
            serde_json::json!({
                "name": label,
                "scope_type": "single_bucket",
                "bucket_names": [bucket],
            }),
        )?;
        // Field spelling per spec is accessKeyId/secret; accept snake_case too.
        let pick = |keys: &[&str]| {
            keys.iter()
                .find_map(|k| v.get(*k).and_then(|x| x.as_str()))
                .map(str::to_string)
        };
        let key = pick(&["accessKeyId", "access_key_id"])
            .ok_or_else(|| Error::Encoding(format!("no access key in response: {v}")))?;
        let secret = pick(&["secret", "secretKey", "secret_key"])
            .ok_or_else(|| Error::Encoding("no secret in response".into()))?;
        Ok((key, secret))
    }

    /// Delete a bucket (rotation retires the old inbox).
    pub fn delete_bucket(&self, name: &str) -> Result<(), Error> {
        self.agent
            .delete(&format!("{API}/objectstore/buckets/{name}/"))
            .set("Authorization", &format!("Token {}", self.token))
            .call()
            .map_err(|e| Error::Transport(format!("DELETE bucket {name}: {e}")))?;
        Ok(())
    }
}
