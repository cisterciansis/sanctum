//! Hippius provisioning: mint per-contact inbox buckets and bucket-scoped
//! sub-tokens through the management API, and the wallet sign-in that gets
//! the API token from nothing but a 12-word phrase.
//!
//! Only starting chats needs this (joining never does). The token lives in
//! Settings.hippius_api_token, from `sign_in` or pasted from the console,
//! and never leaves the device. Endpoints per api.hippius.com's OpenAPI
//! spec (/api/swagger.json), plus what the live server turned out to need.

use serde_json::{json, Value};

use crate::Error;

const API: &str = "https://api.hippius.com/api";

/// What an unfunded account's create-bucket call turns into.
pub const NO_CREDITS: &str = "your mailbox account has no credits yet: add TAO in Settings → Mailbox";

/// The API's own reason (`detail`/`error`) instead of a bare status code.
fn api_err(what: &str) -> impl Fn(ureq::Error) -> Error + '_ {
    move |e| match e {
        ureq::Error::Status(code, r) => {
            let body = r.into_string().unwrap_or_default();
            let why = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| ["detail", "error", "message"].iter().find_map(|k| v[*k].as_str().map(str::to_string)))
                .unwrap_or(body);
            if why.contains("zero_balance") || why.to_lowercase().contains("insufficient") {
                return Error::Transport(NO_CREDITS.into());
            }
            Error::Transport(format!("{what}: HTTP {code}: {}", why.chars().take(200).collect::<String>()))
        }
        e => Error::Transport(format!("{what}: {e}")),
    }
}

fn json_of(r: ureq::Response) -> Result<Value, Error> {
    r.into_json().map_err(|e| Error::Encoding(e.to_string()))
}

/// A fresh 12-word phrase for a new mailbox account.
pub fn new_phrase() -> String {
    let mut entropy = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut entropy);
    subxt_signer::bip39::Mnemonic::from_entropy(&entropy).expect("16 bytes is a valid entropy length").to_string()
}

/// Sign in to the Hippius account a phrase controls, opening it if it's new.
/// Returns (API token, account address). The phrase gives the two keys the
/// Hippius console derives from it, an EVM key (m/44'/60'/0'/0/0) and the
/// sr25519 account key, and both sign the server's one-time challenge.
pub fn sign_in(phrase: &str) -> Result<(String, String), Error> {
    use subxt_signer::{bip39::Mnemonic, eth, sr25519};
    let bad = |e: String| Error::Crypto(e);
    let m = Mnemonic::parse(phrase.split_whitespace().collect::<Vec<_>>().join(" "))
        .map_err(|_| bad("not a valid recovery phrase".into()))?;
    let evm = eth::Keypair::from_phrase(&m, None, eth::DerivationPath::eth(0, 0)).map_err(|e| bad(e.to_string()))?;
    let sub = sr25519::Keypair::from_phrase(&m, None).map_err(|e| bad(e.to_string()))?;
    let address = evm.public_key().to_account_id().to_string();
    let ss58 = sub.public_key().to_account_id().to_string();

    // Same proxy as everything else, so signing up doesn't expose our IP.
    let agent = crate::transport::agent_from_env()?;
    let ids = json!({ "address": address, "substrate_address": ss58 });
    let resp = agent.post(&format!("{API}/auth/mnemonic/")).send_json(ids).map_err(api_err("sign-in challenge"))?;
    // The challenge is bound to the session cookie that comes with it.
    let cookie = resp
        .all("set-cookie")
        .iter()
        .filter_map(|c| c.split(';').next())
        .collect::<Vec<_>>()
        .join("; ");
    let c = json_of(resp)?;
    let challenge = c["challenge"].as_str().unwrap_or_default();
    let msg = c["message"].as_str().unwrap_or_default();
    // Sign only what we expect: a short notice carrying a hex nonce, never
    // arbitrary server-chosen text.
    if !(32..=128).contains(&challenge.len())
        || !challenge.bytes().all(|b| b.is_ascii_hexdigit())
        || msg.len() > 256
        || !msg.ends_with(challenge)
        || !msg.bytes().all(|b| (0x20..0x7f).contains(&b))
    {
        return Err(Error::Encoding("unexpected sign-in challenge".into()));
    }

    // EIP-191 personal_sign, v as 27/28.
    let mut prefixed = format!("\x19Ethereum Signed Message:\n{}", msg.len()).into_bytes();
    prefixed.extend_from_slice(msg.as_bytes());
    let mut sig = evm.sign(&prefixed).0;
    sig[64] += 27;
    let r = agent
        .post(&format!("{API}/auth/verify/"))
        .set("Cookie", &cookie)
        .send_json(json!({
            // Required, though the spec leaves it out.
            "challenge": challenge,
            "signature": crate::tao::hex(&sig),
            "substrate_signature": crate::tao::hex(&sub.sign(msg.as_bytes()).0),
            "address": address,
            "substrate_address": ss58,
        }))
        .map_err(api_err("sign-in"))?;
    let r = json_of(r)?;
    let token = r["token"]
        .as_str()
        .filter(|t| (20..=128).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_alphanumeric()))
        .ok_or_else(|| Error::Encoding("sign-in returned no usable token".into()))?;
    if r["substrate_address"].as_str() != Some(ss58.as_str()) {
        return Err(Error::Encoding("sign-in answered for a different account".into()));
    }
    Ok((token.into(), ss58))
}

/// Today's TAO price in USD (public endpoint).
pub fn tao_usd() -> Result<f64, Error> {
    let agent = crate::transport::agent_from_env()?;
    let v = json_of(agent.get(&format!("{API}/billing/latest-tao-price/")).call().map_err(api_err("TAO price"))?)?;
    v["price_usd"]
        .as_str()
        .and_then(|p| p.parse::<f64>().ok())
        .filter(|p| p.is_finite() && *p > 0.0)
        .ok_or_else(|| Error::Encoding("no TAO price".into()))
}

pub struct Provisioner {
    token: String,
    agent: ureq::Agent,
}

impl Provisioner {
    /// None when no token is configured — callers fall back to static creds.
    pub fn from_settings() -> Option<Self> {
        Self::new(crate::demo::Settings::load().hippius_api_token?)
    }

    pub fn new(token: String) -> Option<Self> {
        // Same proxy as the gateway: provisioning from the bare IP would tie
        // it to the buckets it creates.
        Some(Self { token, agent: crate::transport::agent_from_env().ok()? })
    }

    fn post(&self, path: &str, body: Value) -> Result<Value, Error> {
        let resp = self
            .agent
            .post(&format!("{API}{path}"))
            .set("Authorization", &format!("Token {}", self.token))
            .send_json(body)
            .map_err(api_err(&format!("POST {path}")))?;
        json_of(resp)
    }

    fn get(&self, path: &str) -> Result<Value, Error> {
        let resp = self
            .agent
            .get(&format!("{API}{path}"))
            .set("Authorization", &format!("Token {}", self.token))
            .call()
            .map_err(api_err(&format!("GET {path}")))?;
        json_of(resp)
    }

    /// Buckets are made through the API, which wants the account to hold an
    /// active S3 master token. We never use its secret, so it's dropped.
    pub fn ensure_master_token(&self) -> Result<(), Error> {
        let v = self.get("/objectstore/master-tokens/")?;
        let list = v.get("results").unwrap_or(&v).as_array().cloned().unwrap_or_default();
        if !list.iter().any(|t| t["status"] == "active") {
            self.post("/objectstore/master-tokens/", json!({ "name": "sanctum" }))?;
        }
        Ok(())
    }

    /// Credit balance in USD.
    pub fn credits(&self) -> Result<f64, Error> {
        let v = self.get("/billing/credits/balance/")?;
        v["balance"]
            .as_str()
            .map(str::to_string)
            .or_else(|| v["balance"].as_f64().map(|b| b.to_string()))
            .and_then(|b| b.parse::<f64>().ok())
            .filter(|b| b.is_finite())
            .ok_or_else(|| Error::Encoding("no credit balance".into()))
    }

    /// Where TAO turns into credits. Not the account address: TAO sent there
    /// is not credited, so that's refused here too.
    pub fn deposit_address(&self, account: Option<&str>) -> Result<String, Error> {
        let v = self.get("/billing/substrate-address/")?;
        let a = v["ss58_address"].as_str().unwrap_or_default();
        crate::tao::parse_address(a)?;
        if Some(a) == account {
            return Err(Error::Encoding("deposit address is the account address".into()));
        }
        Ok(a.into())
    }

    /// Create a bucket; names are globally unique, so callers pass something
    /// random like `inbox-<ulid>`.
    pub fn create_bucket(&self, name: &str) -> Result<(), Error> {
        self.post("/objectstore/buckets/", json!({ "name": name, "region": "auto" }))?;
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
            json!({
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

#[cfg(test)]
mod tests {
    /// Live, read-only: signs in to an existing account and reads it.
    /// SANCTUM_TEST_PHRASE_FILE=… cargo test -p sanctum-core --lib provision::tests::live -- --ignored
    #[test]
    #[ignore]
    fn live_sign_in() {
        let path = std::env::var("SANCTUM_TEST_PHRASE_FILE").expect("SANCTUM_TEST_PHRASE_FILE");
        let (token, address) = super::sign_in(&std::fs::read_to_string(path).unwrap()).unwrap();
        let p = super::Provisioner::new(token).unwrap();
        let deposit = p.deposit_address(Some(&address)).unwrap();
        assert_ne!(deposit, address);
        assert!(p.credits().unwrap() >= 0.0);
        assert!(super::tao_usd().unwrap() > 0.0);
        println!("{address} deposit {deposit}");
    }
}
