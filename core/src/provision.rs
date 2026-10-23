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
        self.create_sub_token_multi(&[bucket], label)
    }

    /// Mint a sub-token scoped to a set of buckets (a contact's queue pair).
    pub fn create_sub_token_multi(&self, buckets: &[&str], label: &str) -> Result<(String, String), Error> {
        let v = self.post(
            "/objectstore/sub-tokens/",
            serde_json::json!({
                "name": label,
                "scope_type": if buckets.len() == 1 { "single_bucket" } else { "multi_bucket" },
                "bucket_names": buckets,
                // Omitting actions yields an empty (deny-all) scope.
                "actions": ["read", "write"],
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

    /// Revoke every active sub-token with the given label (test cleanup and
    /// queue rotation both retire by label).
    pub fn revoke_sub_tokens_by_label(&self, label: &str) -> Result<usize, Error> {
        let resp = self
            .agent
            .get(&format!("{API}/objectstore/sub-tokens/?page_size=500"))
            .set("Authorization", &format!("Token {}", self.token))
            .call()
            .map_err(|e| Error::Transport(format!("list sub-tokens: {e}")))?;
        let v: serde_json::Value =
            resp.into_json().map_err(|e| Error::Encoding(e.to_string()))?;
        let items = v
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .or_else(|| v.as_array().cloned())
            .unwrap_or_default();
        let mut n = 0;
        for it in items {
            let name = it.get("name").and_then(|x| x.as_str()).unwrap_or_default();
            let status = it.get("status").and_then(|x| x.as_str()).unwrap_or_default();
            let id = it.get("id").and_then(|x| x.as_str()).unwrap_or_default();
            if name == label && status == "active" && !id.is_empty() {
                self.post(&format!("/objectstore/sub-tokens/{id}/revoke/"), serde_json::json!({}))?;
                n += 1;
            }
        }
        Ok(n)
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
