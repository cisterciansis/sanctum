//! Stealth addresses for TAO, after Bitcoin's silent payments (BIP352), on
//! sr25519 with nothing but schnorrkel's own primitives.
//!
//! Each wallet account has a scan key s and a spend key b, both derived from
//! its phrase; its meta-address is (S = sG, B = bG) and can be shown to
//! anyone. To pay it from an address with key a, a sender takes the shared
//! point aS = sA and, with an index k, hashes it into a chain code c; the
//! one-time address is B soft-derived by c (Substrate's own soft derivation),
//! and only the holder of b can spend from it. The paying address doubles as
//! the ephemeral key, so nothing extra goes on chain, and k is the paying
//! transaction's nonce, so no one-time address ever repeats.

use merlin::Transcript;
use schnorrkel::derive::{ChainCode, Derivation};
use schnorrkel::{ExpansionMode, Keypair, MiniSecretKey, PublicKey};
use sha2::{Digest, Sha256, Sha512};
use subxt::config::substrate::SubstrateConfig;
use subxt::utils::{AccountId32, MultiSignature};
use subxt_signer::bip39::Mnemonic;

use crate::Error;

/// Meta-addresses read `st1…` (base58check of version, S and B).
pub const META_PREFIX: &str = "st1";
const META_VERSION: u8 = 1;

fn err(e: impl std::fmt::Display) -> Error {
    Error::Crypto(e.to_string())
}

/// The mini secret Substrate derives from a phrase (as subxt-signer and
/// polkadot.js do), so an account's key matches its address.
fn mini_secret(phrase: &str) -> Result<[u8; 32], Error> {
    let phrase = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
    let m = Mnemonic::parse(phrase).map_err(|_| err("not a valid recovery phrase"))?;
    let (entropy, len) = m.to_entropy_array();
    let mut seed = [0u8; 64];
    pbkdf2::pbkdf2::<hmac::Hmac<Sha512>>(&entropy[..len], b"mnemonic", 2048, &mut seed).map_err(err)?;
    Ok(seed[..32].try_into().expect("32 bytes"))
}

fn expand(mini: &[u8; 32]) -> Keypair {
    MiniSecretKey::from_bytes(mini).expect("32 bytes").expand_to_keypair(ExpansionMode::Ed25519)
}

/// An account's own signing key, from its phrase.
pub fn account_key(phrase: &str) -> Result<Keypair, Error> {
    Ok(expand(&mini_secret(phrase)?))
}

/// The chain code for one payment: the shared point and the index, hashed.
fn tweak(dh: impl FnOnce(&mut Transcript), k: u64) -> ChainCode {
    let mut t = Transcript::new(b"sanctum-stealth-v1");
    dh(&mut t);
    t.append_u64(b"k", k);
    let mut cc = [0u8; 32];
    t.challenge_bytes(b"cc", &mut cc);
    ChainCode(cc)
}

/// An account's scan and spend keys: its secret hashed under two labels.
pub struct StealthKeys {
    scan: Keypair,
    spend: Keypair,
}

impl StealthKeys {
    pub fn of(phrase: &str) -> Result<Self, Error> {
        let mini = mini_secret(phrase)?;
        let key = |label: &[u8]| {
            let h = Sha512::new().chain_update(b"sanctum/stealth/v1/").chain_update(label).chain_update(mini);
            expand(&h.finalize()[..32].try_into().expect("32 bytes"))
        };
        Ok(Self { scan: key(b"scan"), spend: key(b"spend") })
    }

    pub fn meta(&self) -> Meta {
        Meta { scan: self.scan.public, spend: self.spend.public }
    }

    /// If `to` is our one-time address for a payment from `from` at index
    /// `k`, the tweak that spends it.
    pub fn claim(&self, from: &AccountId32, k: u64, to: &AccountId32) -> Option<[u8; 32]> {
        let a = PublicKey::from_bytes(&from.0).ok()?;
        let cc = tweak(|t| self.scan.secret.commit_raw_key_exchange(t, b"dh", &a), k);
        (self.spend.public.derived_key_simple(cc, []).0.to_bytes() == to.0).then_some(cc.0)
    }

    /// The key for a one-time address we claimed.
    pub fn note_key(&self, tweak: [u8; 32]) -> Keypair {
        self.spend.secret.derived_key_simple(ChainCode(tweak), []).0.to_keypair()
    }
}

/// Where to send stealth payments: public, shareable, never on chain.
#[derive(Clone, Debug, PartialEq)]
pub struct Meta {
    scan: PublicKey,
    spend: PublicKey,
}

fn checksum(payload: &[u8]) -> [u8; 4] {
    Sha256::digest(Sha256::digest(payload))[..4].try_into().expect("4 bytes")
}

impl Meta {
    pub fn encode(&self) -> String {
        let mut raw = vec![META_VERSION];
        raw.extend_from_slice(&self.scan.to_bytes());
        raw.extend_from_slice(&self.spend.to_bytes());
        let sum = checksum(&raw);
        raw.extend_from_slice(&sum);
        format!("{META_PREFIX}{}", crate::tao::bs58_encode(&raw))
    }

    pub fn parse(s: &str) -> Result<Self, Error> {
        let bad = || err("not a private TAO address");
        let raw = s.trim().strip_prefix(META_PREFIX).and_then(crate::tao::bs58_decode).ok_or_else(bad)?;
        if raw.len() != 69 || raw[0] != META_VERSION || raw[65..] != checksum(&raw[..65]) {
            return Err(bad());
        }
        // The identity point would make every one-time address public.
        let key = |b: &[u8]| PublicKey::from_bytes(b).ok().filter(|_| b.iter().any(|&x| x != 0));
        Ok(Self { scan: key(&raw[1..33]).ok_or_else(bad)?, spend: key(&raw[33..65]).ok_or_else(bad)? })
    }

    /// The sender's side: where a payment from `payer` at index `k` goes.
    pub fn one_time(&self, payer: &Keypair, k: u64) -> AccountId32 {
        let cc = tweak(|t| payer.secret.commit_raw_key_exchange(t, b"dh", &self.scan), k);
        AccountId32(self.spend.derived_key_simple(cc, []).0.to_bytes())
    }
}

/// Signs transactions with a schnorrkel key exactly as subxt-signer does, so
/// one-time addresses can pay like any account.
pub struct Signer(pub Keypair);

impl subxt::transactions::Signer<SubstrateConfig> for Signer {
    fn account_id(&self) -> AccountId32 {
        AccountId32(self.0.public.to_bytes())
    }

    fn sign(&self, payload: &[u8]) -> MultiSignature {
        MultiSignature::Sr25519(self.0.sign(schnorrkel::signing_context(b"substrate").bytes(payload)).to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use subxt_signer::{DeriveJunction, sr25519};

    const ALICE: &str = "bottom drive obey lake curtain smoke basket hold race lonely fit walk";
    const BOB: &str = "legal winner thank year wave sausage worth useful legal winner thank yellow";

    #[test]
    fn account_key_matches_subxt_signer() {
        let ours = account_key(ALICE).unwrap();
        let theirs = sr25519::Keypair::from_phrase(&Mnemonic::parse(ALICE).unwrap(), None).unwrap();
        assert_eq!(ours.public.to_bytes(), theirs.public_key().0);
        // The well-known address of the bare dev phrase.
        assert_eq!(AccountId32(ours.public.to_bytes()).to_string(), "5DfhGyQdFobKM8NsWvEeAKk5EQQgYe9AydgJ7rMB6E1EqRzV");
    }

    #[test]
    fn payer_and_recipient_agree_and_nobody_else_can_claim() {
        let (alice, bob) = (StealthKeys::of(ALICE).unwrap(), StealthKeys::of(BOB).unwrap());
        let payer = account_key(BOB).unwrap();
        let from = AccountId32(payer.public.to_bytes());
        let to = alice.meta().one_time(&payer, 7);
        let tweak = alice.claim(&from, 7, &to).expect("alice finds her payment");
        assert_eq!(alice.note_key(tweak).public.to_bytes(), to.0);
        assert!(alice.claim(&from, 8, &to).is_none(), "wrong index");
        assert!(bob.claim(&from, 7, &to).is_none(), "not bob's");
        assert_ne!(to, alice.meta().one_time(&payer, 8), "no address repeats");
    }

    #[test]
    fn one_time_spend_key_matches_substrate_soft_derivation() {
        // Rebuild the spend key through subxt-signer and soft-derive it with
        // the same chain code: an independent path to the same address.
        let mini = mini_secret(ALICE).unwrap();
        let h = Sha512::new().chain_update(b"sanctum/stealth/v1/").chain_update(b"spend").chain_update(mini);
        let spend = sr25519::Keypair::from_secret_key(h.finalize()[..32].try_into().unwrap()).unwrap();
        let alice = StealthKeys::of(ALICE).unwrap();
        let payer = account_key(BOB).unwrap();
        let to = alice.meta().one_time(&payer, 1);
        let tweak = alice.claim(&AccountId32(payer.public.to_bytes()), 1, &to).unwrap();
        assert_eq!(spend.derive([DeriveJunction::Soft(tweak)]).public_key().0, to.0);
    }

    #[test]
    fn meta_round_trips_and_rejects_typos() {
        let meta = StealthKeys::of(ALICE).unwrap().meta();
        let s = meta.encode();
        assert!(s.starts_with(META_PREFIX));
        assert_eq!(Meta::parse(&s).unwrap(), meta);
        let typo: String = s.chars().enumerate().map(|(i, c)| if i == 10 { if c == 'a' { 'b' } else { 'a' } } else { c }).collect();
        assert!(Meta::parse(&typo).is_err());
        assert!(Meta::parse("5DfhGyQdFobKM8NsWvEeAKk5EQQgYe9AydgJ7rMB6E1EqRzV").is_err());
    }

    #[test]
    fn signer_signs_like_subxt_signer() {
        let kp = account_key(ALICE).unwrap();
        let MultiSignature::Sr25519(sig) =
            <Signer as subxt::transactions::Signer<SubstrateConfig>>::sign(&Signer(kp.clone()), b"payload")
        else {
            unreachable!()
        };
        assert!(sr25519::verify(&sr25519::Signature(sig), b"payload", &sr25519::PublicKey(kp.public.to_bytes())));
    }
}
