//! Hippius S3 dead-drop transport: presigned SigV4 requests over plain HTTPS.
//!
//! Gateway facts (verified live, 2026-10-23): region "decentralized",
//! path-style only, PUT->visible ~2.5s. Blocking IO; callers thread it.

use crate::Error;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use std::time::Duration;

pub const DEFAULT_ENDPOINT: &str = "https://s3.hippius.com";
const SIGN_TTL: Duration = Duration::from_secs(300);

pub struct Mailbox {
    bucket: Bucket,
    creds: Credentials,
    agent: ureq::Agent,
}

/// Metadata-privacy toggle: set SANCTUM_PROXY to route every gateway request
/// through a SOCKS5/HTTP proxy, e.g. Tor (socks5://127.0.0.1:9050) or a UR
/// network proxy endpoint. Unset = direct.
fn agent_from_env() -> Result<ureq::Agent, Error> {
    let mut b = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(60))
        .user_agent(concat!("sanctum/", env!("CARGO_PKG_VERSION")));
    if let Ok(proxy) = std::env::var("SANCTUM_PROXY") {
        let proxy = ureq::Proxy::new(&proxy)
            .map_err(|e| Error::Transport(format!("bad SANCTUM_PROXY: {e}")))?;
        b = b.proxy(proxy);
    }
    Ok(b.build())
}


/// Strip query strings (presigned signatures) from error text before logging.
fn redact(e: impl std::fmt::Display) -> String {
    let s = e.to_string();
    match s.split_once('?') {
        Some((head, _)) => format!("{head}?<redacted>"),
        None => s,
    }
}

impl Mailbox {
    pub fn new(endpoint: &str, bucket: &str, access_key: &str, secret: &str) -> Result<Self, Error> {
        let bucket = Bucket::new(
            endpoint.parse().map_err(|_| Error::Transport("bad endpoint".into()))?,
            UrlStyle::Path,
            bucket.to_string(),
            "decentralized".to_string(),
        )
        .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(Self {
            bucket,
            creds: Credentials::new(access_key, secret),
            agent: agent_from_env()?,
        })
    }

    pub fn put(&self, key: &str, body: &[u8]) -> Result<(), Error> {
        let url = self.bucket.put_object(Some(&self.creds), key).sign(SIGN_TTL);
        self.agent.put(url.as_str())
            .send_bytes(body)
            .map_err(|e| Error::Transport(format!("put {key}: {}", redact(e))))?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Result<Vec<u8>, Error> {
        let url = self.bucket.get_object(Some(&self.creds), key).sign(SIGN_TTL);
        let resp = self.agent.get(url.as_str())
            .call()
            .map_err(|e| Error::Transport(format!("get {key}: {}", redact(e))))?;
        let mut buf = Vec::new();
        resp.into_reader()
            .read_to_end(&mut buf)
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(buf)
    }

    /// Does the object exist? (HEAD; used for delivery receipts: the
    /// recipient deletes after decrypt, so absence after presence = delivered.)
    pub fn exists(&self, key: &str) -> Result<bool, Error> {
        let url = self.bucket.head_object(Some(&self.creds), key).sign(SIGN_TTL);
        match self.agent.head(url.as_str()).call() {
            Ok(_) => Ok(true),
            Err(ureq::Error::Status(404, _)) => Ok(false),
            Err(e) => Err(Error::Transport(format!("head {key}: {}", redact(e)))),
        }
    }

    pub fn delete(&self, key: &str) -> Result<(), Error> {
        let url = self.bucket.delete_object(Some(&self.creds), key).sign(SIGN_TTL);
        self.agent.delete(url.as_str())
            .call()
            .map_err(|e| Error::Transport(format!("delete {key}: {}", redact(e))))?;
        Ok(())
    }

    /// Keys under `prefix`, in lexicographic (= ULID = chronological) order.
    pub fn list(&self, prefix: &str) -> Result<Vec<String>, Error> {
        let mut action = self.bucket.list_objects_v2(Some(&self.creds));
        action.with_prefix(prefix);
        let url = action.sign(SIGN_TTL);
        let resp = self.agent.get(url.as_str())
            .call()
            .map_err(|e| Error::Transport(format!("list: {}", redact(e))))?;
        let xml = resp.into_string().map_err(|e| Error::Transport(e.to_string()))?;
        let parsed = rusty_s3::actions::ListObjectsV2::parse_response(&xml)
            .map_err(|e| Error::Transport(format!("list parse: {e}")))?;
        // rusty-s3 requests encoding-type=url, so keys come back percent-encoded.
        Ok(parsed
            .contents
            .into_iter()
            .map(|o| {
                percent_encoding::percent_decode_str(&o.key)
                    .decode_utf8_lossy()
                    .into_owned()
            })
            .collect())
    }
}
