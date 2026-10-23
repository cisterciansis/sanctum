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
        Ok(Self { bucket, creds: Credentials::new(access_key, secret) })
    }

    pub fn put(&self, key: &str, body: &[u8]) -> Result<(), Error> {
        let url = self.bucket.put_object(Some(&self.creds), key).sign(SIGN_TTL);
        ureq::put(url.as_str())
            .send_bytes(body)
            .map_err(|e| Error::Transport(format!("put {key}: {e}")))?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Result<Vec<u8>, Error> {
        let url = self.bucket.get_object(Some(&self.creds), key).sign(SIGN_TTL);
        let resp = ureq::get(url.as_str())
            .call()
            .map_err(|e| Error::Transport(format!("get {key}: {e}")))?;
        let mut buf = Vec::new();
        resp.into_reader()
            .read_to_end(&mut buf)
            .map_err(|e| Error::Transport(e.to_string()))?;
        Ok(buf)
    }

    pub fn delete(&self, key: &str) -> Result<(), Error> {
        let url = self.bucket.delete_object(Some(&self.creds), key).sign(SIGN_TTL);
        ureq::delete(url.as_str())
            .call()
            .map_err(|e| Error::Transport(format!("delete {key}: {e}")))?;
        Ok(())
    }

    /// Keys under `prefix`, in lexicographic (= ULID = chronological) order.
    pub fn list(&self, prefix: &str) -> Result<Vec<String>, Error> {
        let mut action = self.bucket.list_objects_v2(Some(&self.creds));
        action.with_prefix(prefix);
        let url = action.sign(SIGN_TTL);
        let resp = ureq::get(url.as_str())
            .call()
            .map_err(|e| Error::Transport(format!("list: {e}")))?;
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
