//! TAO (Bittensor) wallet: keys, balances, transfers, and offline cheques.
//!
//! Chain access is JSON-RPC over the same HTTP agent as the mailboxes, so the
//! Tor/proxy setting covers it too (no websocket that would bypass it).
//! Signing is done by subxt from live chain metadata; Bittensor's custom
//! transaction extensions carry no data, so stock Substrate signing applies.
//!
//! Cheques are transfers signed offline (immortal, explicit nonce) from a
//! cached chain snapshot. Whoever gets online first submits them; the nonce
//! makes them single-use. They can bounce: if the balance runs short, or the
//! nonce is used by another transaction first.

use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use subxt::backend::LegacyBackend;
use subxt::config::substrate::{SpecVersionForRange, SubstrateConfig};
use subxt::dynamic::Value;
use subxt::utils::AccountId32;
use subxt::{OfflineClient, OnlineClient};
use subxt_signer::{bip39::Mnemonic, sr25519::Keypair};

use crate::Error;

pub const RAO_PER_TAO: u128 = 1_000_000_000;
/// Bittensor uses the generic Substrate SS58 prefix.
const SS58_PREFIX: u16 = 42;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Net {
    #[serde(rename = "main")]
    Main,
    #[serde(rename = "test")]
    Test,
}

impl Net {
    fn url(self) -> &'static str {
        match self {
            Net::Main => "https://entrypoint-finney.opentensor.ai:443",
            Net::Test => "https://test.finney.opentensor.ai:443",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Net::Main => "main",
            Net::Test => "test",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        match s {
            "main" => Some(Net::Main),
            "test" => Some(Net::Test),
            _ => None,
        }
    }
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::Transport(format!("tao: {e}"))
}

// ---------------------------------------------------------------------------
// Amounts and addresses
// ---------------------------------------------------------------------------

/// "1.5" / "0.000000001" / "2" → rao. Strict: at most 9 decimals, no sign,
/// no exponent, overflow-checked, and never zero.
pub fn parse_tao(s: &str) -> Option<u128> {
    let s = s.trim().trim_end_matches(['τ', 't', 'T', 'a', 'A', 'o', 'O']).trim();
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    if whole.is_empty() && frac.is_empty()
        || frac.len() > 9
        || !whole.chars().chain(frac.chars()).all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let whole: u128 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
    let frac: u128 = format!("{frac:0<9}").parse().ok()?;
    whole.checked_mul(RAO_PER_TAO)?.checked_add(frac).filter(|&r| r > 0)
}

/// rao → "1.5 τ" (trailing zeros trimmed).
pub fn format_tao(rao: u128) -> String {
    let (w, f) = (rao / RAO_PER_TAO, rao % RAO_PER_TAO);
    if f == 0 {
        return format!("{w} τ");
    }
    let frac = format!("{f:09}");
    format!("{w}.{} τ", frac.trim_end_matches('0'))
}

/// A Bittensor (prefix 42) SS58 address, checksum verified. Addresses from
/// other Substrate chains are refused so funds can't go to the wrong network.
pub fn parse_address(s: &str) -> Result<AccountId32, Error> {
    let s = s.trim();
    let raw = bs58_decode(s).ok_or_else(|| err("not an SS58 address"))?;
    // Prefix 42 encodes as one byte; account (32) + checksum (2).
    if raw.len() != 35 || raw[0] as u16 != SS58_PREFIX {
        return Err(err("not a Bittensor address (SS58 prefix 42)"));
    }
    s.parse::<AccountId32>().map_err(|_| err("bad address checksum"))
}

pub fn address_of(acc: &AccountId32) -> String {
    acc.to_string() // subxt formats with the generic prefix 42
}

fn bs58_decode(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    if s.is_empty() || s.len() > 64 {
        return None;
    }
    let mut out: Vec<u8> = Vec::new();
    for c in s.bytes() {
        let mut carry = ALPHABET.iter().position(|&a| a == c)? as u32;
        for b in out.iter_mut().rev() {
            carry += (*b as u32) * 58;
            *b = carry as u8;
            carry >>= 8;
        }
        while carry > 0 {
            out.insert(0, carry as u8);
            carry >>= 8;
        }
    }
    let zeros = s.bytes().take_while(|&c| c == b'1').count();
    let mut v = vec![0u8; zeros];
    v.extend(out);
    Some(v)
}

// ---------------------------------------------------------------------------
// Accounts
// ---------------------------------------------------------------------------

/// A wallet account. `phrase` is None for watch-only accounts (an address
/// whose keys live in btcli / Talisman / Polkadot.js).
#[derive(Clone, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub phrase: Option<String>,
}

impl Account {
    pub fn generate(name: &str) -> Result<Self, Error> {
        let mut entropy = [0u8; 16]; // 12 words
        use rand_core::RngCore;
        rand_core::OsRng.fill_bytes(&mut entropy);
        let phrase = Mnemonic::from_entropy(&entropy).map_err(err)?;
        Self::import(name, &phrase.to_string())
    }

    pub fn import(name: &str, phrase: &str) -> Result<Self, Error> {
        let phrase = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
        let m = Mnemonic::parse(&phrase).map_err(|_| err("not a valid recovery phrase"))?;
        let kp = Keypair::from_phrase(&m, None).map_err(err)?;
        Ok(Self {
            name: name.into(),
            address: address_of(&kp.public_key().into()),
            phrase: Some(phrase),
        })
    }

    pub fn watch(name: &str, address: &str) -> Result<Self, Error> {
        let acc = parse_address(address)?;
        Ok(Self { name: name.into(), address: address_of(&acc), phrase: None })
    }

    pub fn can_sign(&self) -> bool {
        self.phrase.is_some()
    }

    fn keypair(&self) -> Result<Keypair, Error> {
        let phrase = self.phrase.as_deref().ok_or_else(|| err("watch-only account cannot sign"))?;
        Keypair::from_phrase(&Mnemonic::parse(phrase).map_err(err)?, None).map_err(err)
    }
}

// ---------------------------------------------------------------------------
// Chain access: JSON-RPC over our HTTP agent, driven on a small runtime.
// ---------------------------------------------------------------------------

fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .expect("tao runtime")
    })
}

/// Run a chain operation to completion from synchronous code.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    rt().block_on(f)
}

struct HttpRpc {
    url: &'static str,
    agent: ureq::Agent,
}

impl subxt::rpcs::RpcClientT for HttpRpc {
    fn request_raw<'a>(
        &'a self,
        method: &'a str,
        params: Option<Box<RawValue>>,
    ) -> subxt::rpcs::client::RawRpcFuture<'a, Box<RawValue>> {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":{},"params":{}}}"#,
            serde_json::Value::from(method),
            params.as_deref().map(RawValue::get).unwrap_or("[]")
        );
        let (agent, url) = (self.agent.clone(), self.url);
        Box::pin(async move {
            let client_err = |e: String| subxt::rpcs::Error::Client(e.into());
            let text = tokio::task::spawn_blocking(move || {
                agent
                    .post(url)
                    .set("Content-Type", "application/json")
                    .send_string(&body)
                    .map_err(|e| e.to_string())?
                    .into_string()
                    .map_err(|e| e.to_string())
            })
            .await
            .map_err(|e| client_err(e.to_string()))?
            .map_err(client_err)?;
            // `result` may legitimately be JSON null (e.g. absent storage),
            // so check for `error` first and keep `result` raw.
            #[derive(Deserialize)]
            struct Failure {
                error: Option<serde_json::Value>,
            }
            #[derive(Deserialize)]
            struct Success {
                result: Box<RawValue>,
            }
            let de = subxt::rpcs::Error::Deserialization;
            if let Some(e) = serde_json::from_str::<Failure>(&text).map_err(de)?.error {
                return Err(client_err(format!("{method}: {e}")));
            }
            Ok(serde_json::from_str::<Success>(&text).map_err(de)?.result)
        })
    }

    fn subscribe_raw<'a>(
        &'a self,
        sub: &'a str,
        _params: Option<Box<RawValue>>,
        _unsub: &'a str,
    ) -> subxt::rpcs::client::RawRpcFuture<'a, subxt::rpcs::client::RawRpcSubscription> {
        Box::pin(async move {
            Err(subxt::rpcs::Error::Client(
                format!("{sub}: subscriptions need a websocket; HTTP only").into(),
            ))
        })
    }
}

type Api = OnlineClient<SubstrateConfig>;

/// One client per network, reused (it caches chain metadata).
async fn api(net: Net) -> Result<Api, Error> {
    static APIS: OnceLock<Mutex<Vec<(Net, Api)>>> = OnceLock::new();
    let apis = APIS.get_or_init(Default::default);
    if let Some((_, a)) = apis.lock().expect("apis").iter().find(|(n, _)| *n == net) {
        return Ok(a.clone());
    }
    let rpc = HttpRpc { url: net.url(), agent: crate::transport::agent_from_env()? };
    let backend = LegacyBackend::builder().build(subxt::rpcs::RpcClient::new(rpc));
    let a = Api::from_backend(Arc::new(backend)).await.map_err(err)?;
    apis.lock().expect("apis").push((net, a.clone()));
    Ok(a)
}

fn transfer_call(to: &AccountId32, rao: u128) -> impl subxt::transactions::Payload {
    subxt::dynamic::tx(
        "Balances",
        "transfer_keep_alive",
        vec![Value::unnamed_variant("Id", [Value::from_bytes(to.0)]), Value::u128(rao)],
    )
}

/// Free balance in rao.
pub fn balance(net: Net, address: &str) -> Result<u128, Error> {
    #[derive(subxt::ext::scale_decode::DecodeAsType)]
    #[decode_as_type(crate_path = "subxt::ext::scale_decode")]
    struct Info {
        data: Data,
    }
    #[derive(subxt::ext::scale_decode::DecodeAsType)]
    #[decode_as_type(crate_path = "subxt::ext::scale_decode")]
    struct Data {
        free: u128,
    }
    let acc = parse_address(address)?;
    block_on(async {
        let at = api(net).await?.at_current_block().await.map_err(err)?;
        let addr = subxt::dynamic::storage::<([u8; 32],), Info>("System", "Account");
        let v = at.storage().fetch(addr, (acc.0,)).await.map_err(err)?;
        Ok(v.decode().map_err(err)?.data.free)
    })
}

/// Where a transfer landed; enough for anyone to verify it on chain.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    /// Extrinsic hash, 0x-hex.
    pub tx: String,
    /// Hash of the block it was included in, 0x-hex.
    pub block: String,
}

/// Validate (dry), submit, and wait until the transfer is in a finalized
/// block and succeeded. Typically 30–60 s.
pub fn send(net: Net, from: &Account, to: &str, rao: u128) -> Result<Receipt, Error> {
    let to = parse_address(to)?;
    let kp = from.keypair()?;
    block_on(async {
        let api = api(net).await?;
        let at = api.at_current_block().await.map_err(err)?;
        let start = at.block_number();
        let tx = at
            .transactions()
            .create_signed(&transfer_call(&to, rao), &kp, Default::default())
            .await
            .map_err(err)?;
        // The chain's own checks first, so "insufficient funds" is a clear
        // error instead of a transaction that silently never lands.
        if let subxt::transactions::ValidationResult::Invalid(why) = tx.validate().await.map_err(err)? {
            return Err(Error::Transport(rejection(&format!("{why:?}"))));
        }
        tx.submit().await.map_err(err)?;
        wait_for_inclusion(&api, tx.hash(), start).await
    })
}

/// Scan finalized blocks from `start` for our extrinsic, then check it
/// succeeded. HTTP has no subscriptions, so this polls every 4 s (≤ 3 min).
async fn wait_for_inclusion(
    api: &Api,
    hash: subxt::utils::H256,
    start: u64,
) -> Result<Receipt, Error> {
    let mut next = start;
    for _ in 0..45 {
        let head = api.at_current_block().await.map_err(err)?.block_number();
        while next <= head {
            let at = api.at_block(next).await.map_err(err)?;
            let exts = at.extrinsics().fetch().await.map_err(err)?;
            for ext in exts.iter() {
                let ext = ext.map_err(err)?;
                if ext.hash() != hash {
                    continue;
                }
                let events = ext.events().await.map_err(err)?;
                for ev in events.iter() {
                    let ev = ev.map_err(err)?;
                    if ev.pallet_name() == "System" && ev.event_name() == "ExtrinsicFailed" {
                        return Err(err("transaction failed on chain"));
                    }
                }
                return Ok(Receipt { tx: hex(&hash.0), block: hex(&at.block_hash().0) });
            }
            next += 1;
        }
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    }
    Err(err("not included after 3 minutes; check your balance before retrying"))
}

/// Check on chain that `receipt` is a successful transfer of exactly `rao`
/// from `from` to `to`. This is what turns a peer's "I paid you" into a fact.
pub fn verify(net: Net, receipt: &Receipt, from: &str, to: &str, rao: u128) -> Result<bool, Error> {
    let (from, to) = (parse_address(from)?, parse_address(to)?);
    let tx = unhex32(&receipt.tx)?;
    let block = unhex32(&receipt.block)?;
    #[derive(subxt::ext::scale_decode::DecodeAsType)]
    #[decode_as_type(crate_path = "subxt::ext::scale_decode")]
    struct Transfer {
        from: AccountId32,
        to: AccountId32,
        amount: u128,
    }
    block_on(async {
        let api = api(net).await?;
        let at = api.at_block(subxt::utils::H256(block)).await.map_err(err)?;
        let exts = at.extrinsics().fetch().await.map_err(err)?;
        for ext in exts.iter() {
            let ext = ext.map_err(err)?;
            if ext.hash().0 != tx {
                continue;
            }
            let events = ext.events().await.map_err(err)?;
            let mut ok = false;
            for ev in events.iter() {
                let ev = ev.map_err(err)?;
                match (ev.pallet_name(), ev.event_name()) {
                    ("System", "ExtrinsicFailed") => return Ok(false),
                    ("Balances", "Transfer") => {
                        let t: Transfer = ev.decode_fields_unchecked_as().map_err(err)?;
                        ok |= t.from == from && t.to == to && t.amount == rao;
                    }
                    _ => {}
                }
            }
            return Ok(ok);
        }
        Ok(false)
    })
}

// ---------------------------------------------------------------------------
// Offline cheques
// ---------------------------------------------------------------------------

/// What offline signing needs, captured while online (not secret).
#[derive(Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub net: Net,
    pub genesis: String,
    pub spec_version: u32,
    pub tx_version: u32,
    /// SCALE metadata, 0x-hex (a few hundred KB).
    pub metadata: String,
    /// The account's next nonce at snapshot time.
    pub nonce: u64,
}

/// Capture a snapshot for `address` while online.
pub fn snapshot(net: Net, address: &str) -> Result<Snapshot, Error> {
    let acc = parse_address(address)?;
    let rpc = HttpRpc { url: net.url(), agent: crate::transport::agent_from_env()? };
    block_on(async {
        use subxt::rpcs::RpcClientT;
        let call = |m: &'static str, p: &'static str| {
            let p = RawValue::from_string(p.into()).ok();
            rpc.request_raw(m, p)
        };
        let meta: String = serde_json::from_str(call("state_getMetadata", "[]").await.map_err(err)?.get())
            .map_err(err)?;
        let ver: serde_json::Value =
            serde_json::from_str(call("state_getRuntimeVersion", "[]").await.map_err(err)?.get())
                .map_err(err)?;
        let genesis: String =
            serde_json::from_str(call("chain_getBlockHash", "[0]").await.map_err(err)?.get())
                .map_err(err)?;
        let who = RawValue::from_string(format!(r#"["{}"]"#, address_of(&acc))).map_err(err)?;
        let nonce: u64 = serde_json::from_str(
            rpc.request_raw("system_accountNextIndex", Some(who)).await.map_err(err)?.get(),
        )
        .map_err(err)?;
        Ok(Snapshot {
            net,
            genesis,
            spec_version: ver["specVersion"].as_u64().ok_or_else(|| err("no specVersion"))? as u32,
            tx_version: ver["transactionVersion"].as_u64().ok_or_else(|| err("no txVersion"))? as u32,
            metadata: meta,
            nonce,
        })
    })
}

fn offline(snap: &Snapshot) -> Result<OfflineClient<SubstrateConfig>, Error> {
    let meta = unhex(&snap.metadata)?;
    let metadata = subxt::metadata::Metadata::decode_from(&meta[..]).map_err(err)?;
    let config = SubstrateConfig::builder()
        .set_genesis_hash(subxt::utils::H256(unhex32(&snap.genesis)?))
        .set_metadata_for_spec_versions([(snap.spec_version, metadata.into())])
        .set_spec_version_for_block_ranges([SpecVersionForRange {
            block_range: 0..u64::MAX,
            spec_version: snap.spec_version,
            transaction_version: snap.tx_version,
        }])
        .build();
    Ok(OfflineClient::new_with_config(config))
}

/// Sign a transfer with no network: immortal, at `nonce`. The caller tracks
/// nonces so consecutive cheques don't collide.
pub fn sign_cheque(snap: &Snapshot, from: &Account, to: &str, rao: u128, nonce: u64) -> Result<Vec<u8>, Error> {
    let to = parse_address(to)?;
    let kp = from.keypair()?;
    let client = offline(snap)?;
    let at = client.at_block(0u64).map_err(err)?;
    let params = subxt::config::substrate::SubstrateExtrinsicParamsBuilder::new()
        .nonce(nonce)
        .immortal()
        .build();
    let call = transfer_call(&to, rao);
    let mut partial = at.transactions().create_signable_offline(&call, params).map_err(err)?;
    Ok(partial.sign(&kp).map_err(err)?.into_encoded())
}

/// What a cheque pays, read from the signed bytes themselves (never from
/// the message that carried it).
#[derive(Clone, Debug, PartialEq)]
pub struct Cheque {
    pub from: String,
    pub to: String,
    pub rao: u128,
    pub tx: String,
}

pub fn inspect_cheque(snap: &Snapshot, bytes: &[u8]) -> Result<Cheque, Error> {
    #[derive(subxt::ext::scale_decode::DecodeAsType)]
    #[decode_as_type(crate_path = "subxt::ext::scale_decode")]
    enum Dest {
        Id(AccountId32),
    }
    #[derive(subxt::ext::scale_decode::DecodeAsType)]
    #[decode_as_type(crate_path = "subxt::ext::scale_decode")]
    struct Fields {
        dest: Dest,
        value: u128,
    }
    if bytes.len() > 4096 {
        return Err(err("cheque too large"));
    }
    let client = offline(snap)?;
    let at = client.at_block(0u64).map_err(err)?;
    let exts = block_on(at.extrinsics().from_bytes(vec![bytes.to_vec()]));
    let ext = exts.iter().next().ok_or_else(|| err("empty cheque"))?.map_err(err)?;
    if (ext.pallet_name(), ext.call_name()) != ("Balances", "transfer_keep_alive") {
        return Err(err("cheque is not a plain TAO transfer"));
    }
    let from = ext.address_bytes().ok_or_else(|| err("cheque is unsigned"))?;
    // MultiAddress::Id: variant byte 0 then the 32-byte account.
    let from: [u8; 32] = from.get(1..33).and_then(|b| b.try_into().ok()).ok_or_else(|| err("bad signer"))?;
    let Fields { dest: Dest::Id(to), value } = ext.decode_call_data_fields_unchecked_as().map_err(err)?;
    Ok(Cheque {
        from: address_of(&AccountId32(from)),
        to: address_of(&to),
        rao: value,
        tx: hex(&ext.hash().0),
    })
}

/// Submit a cheque (either side may) and wait for it to land.
pub fn cash_cheque(net: Net, bytes: &[u8]) -> Result<Receipt, Error> {
    block_on(async {
        let api = api(net).await?;
        let at = api.at_current_block().await.map_err(err)?;
        let start = at.block_number();
        let tx = at.transactions().from_bytes(bytes.to_vec());
        if let subxt::transactions::ValidationResult::Invalid(why) = tx.validate().await.map_err(err)? {
            return Err(Error::Transport(rejection(&format!("{why:?}"))));
        }
        tx.submit().await.map_err(err)?;
        wait_for_inclusion(&api, tx.hash(), start).await
    })
}

/// The chain validator's verdict in words.
fn rejection(why: &str) -> String {
    let words = match why {
        w if w.starts_with("Payment") => "not enough TAO for the amount plus fee",
        w if w.starts_with("Stale") => "already used: that transaction number was spent",
        w if w.starts_with("Future") => "waiting on an earlier transaction from this account",
        w if w.starts_with("BadProof") => "invalid signature (BadProof)",
        w if w.starts_with("AncientBirthBlock") => "expired",
        w => return format!("rejected by the chain: {w}"),
    };
    words.into()
}

/// An error as a short sentence for payment cards.
pub fn reason(e: &Error) -> String {
    let s = e.to_string();
    s.trim_start_matches("transport: ").trim_start_matches("tao: ").to_string()
}

// ---------------------------------------------------------------------------

pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(2 + b.len() * 2);
    s.push_str("0x");
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

pub fn unhex(s: &str) -> Result<Vec<u8>, Error> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.len() % 2 != 0 {
        return Err(err("odd hex length"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| err("bad hex")))
        .collect()
}

fn unhex32(s: &str) -> Result<[u8; 32], Error> {
    unhex(s)?.try_into().map_err(|_| err("expected 32 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts() {
        assert_eq!(parse_tao("1.5"), Some(1_500_000_000));
        assert_eq!(parse_tao("0.000000001"), Some(1));
        assert_eq!(parse_tao(".5"), Some(500_000_000));
        assert_eq!(parse_tao("2 τ"), Some(2 * RAO_PER_TAO));
        for bad in ["0", "", "-1", "1e3", "1.0000000001", "abc", "1.2.3", "340282366920938463463374607431768211456"] {
            assert_eq!(parse_tao(bad), None, "{bad}");
        }
        assert_eq!(format_tao(1_500_000_000), "1.5 τ");
        assert_eq!(format_tao(1), "0.000000001 τ");
        assert_eq!(format_tao(3 * RAO_PER_TAO), "3 τ");
    }

    #[test]
    fn addresses_and_accounts() {
        let a = Account::generate("t").unwrap();
        assert!(a.can_sign() && a.address.starts_with('5'));
        let b = Account::import("t", a.phrase.as_deref().unwrap()).unwrap();
        assert_eq!(a.address, b.address, "phrase round-trips to the same key");
        assert!(parse_address(&a.address).is_ok());
        // Corrupted checksum, other-chain prefix (Polkadot, prefix 0), junk.
        let mut bad = a.address.clone();
        bad.replace_range(10..11, if &bad[10..11] == "a" { "b" } else { "a" });
        assert!(parse_address(&bad).is_err());
        assert!(parse_address("15oF4uVJwmo4TdGW7VfQxNLavjCXviqxT9S1MgbjMNHr6Sp5").is_err());
        assert!(parse_address("hello").is_err());
        assert!(!Account::watch("w", &a.address).unwrap().can_sign());
    }

    /// Live, read-only against mainnet (run with --ignored): a fresh unfunded
    /// key exercises balance, snapshot, offline signing, cheque decoding and
    /// the chain's validator. Every path must stop at "no funds"; nothing is
    /// ever broadcast.
    #[test]
    #[ignore]
    fn live_mainnet_read_only() {
        let me = Account::generate("me").unwrap();
        let them = Account::generate("them").unwrap();
        assert_eq!(balance(Net::Main, &me.address).unwrap(), 0);

        let err = send(Net::Main, &me, &them.address, RAO_PER_TAO).unwrap_err().to_string();
        assert!(err.contains("not enough TAO"), "online send stops at validation: {err}");

        let snap = snapshot(Net::Main, &me.address).unwrap();
        assert_eq!(snap.nonce, 0);
        let bytes = sign_cheque(&snap, &me, &them.address, 1_500_000_000, snap.nonce).unwrap();
        let c = inspect_cheque(&snap, &bytes).unwrap();
        assert_eq!((c.from.as_str(), c.to.as_str(), c.rao), (me.address.as_str(), them.address.as_str(), 1_500_000_000));

        let err = cash_cheque(Net::Main, &bytes).unwrap_err().to_string();
        assert!(err.contains("not enough TAO"), "valid signature, bounced on funds: {err}");
        let mut forged = bytes.clone();
        let n = forged.len();
        forged[n - 3] ^= 1; // tamper with the amount
        let err = cash_cheque(Net::Main, &forged).unwrap_err().to_string();
        assert!(err.contains("BadProof"), "tampered cheque fails the signature: {err}");
    }
}
