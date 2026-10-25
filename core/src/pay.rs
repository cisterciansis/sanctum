//! Bitcoin and Zcash in chat, bitchat-style, with no keys in Sanctum: Cashu
//! ecash tokens, lightning:/bitcoin:/BOLT11 links and Zcash addresses and
//! zcash: requests are recognised in messages and summarised as chips that
//! hand off to an external wallet. Nothing here contacts a mint or a node.
//!
//! The Cashu decoder is a port of bitchat's CashuTokenDecoder.swift
//! (Unlicense). It parses attacker-controlled message text, so every path is
//! bounds-checked, size-capped and fails closed (None) instead of panicking.

use base64::{
    alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
    Engine,
};

/// Shown next to every Cashu chip and after every `/pay`.
pub const BEARER_WARNING: &str =
    "Cashu tokens are bearer money: whoever redeems one first gets it.";

/// Real tokens are a few KB; anything much bigger is abuse.
pub const MAX_TOKEN_LEN: usize = 60_000;
/// Per-proof and total cap (about all the sats there will ever be).
const MAX_AMOUNT: u64 = 2_100_000_000_000_000;
/// Only the start of a huge message is scanned, for at most this many links.
const MAX_SCAN: usize = 128 * 1024;
const MAX_LINKS: usize = 4;
/// BOLT11 invoices and LNURLs; long ones carry routing hints.
const MAX_LN_LEN: usize = 4096;

#[derive(Clone, Debug, PartialEq)]
pub struct CashuInfo {
    /// 'A' (V3, JSON) or 'B' (V4, CBOR).
    pub version: char,
    /// Sum of all proof amounts; None when no valid amount was found.
    pub amount: Option<u64>,
    pub unit: Option<String>,
    /// Host of the first mint URL, for display.
    pub mint_host: Option<String>,
    /// Sender memo, sanitized for display.
    pub memo: Option<String>,
}

impl CashuInfo {
    /// "500 sat"; the unit defaults to sats per NUT-00.
    pub fn display_amount(&self) -> Option<String> {
        self.amount.map(|a| format!("{a} {}", self.unit.as_deref().unwrap_or("sat")))
    }
}

/// `s` minus an ASCII prefix matched case-insensitively.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len()).filter(|h| h.eq_ignore_ascii_case(prefix)).map(|_| &s[prefix.len()..])
}

/// The bare `cashuA…`/`cashuB…` token from raw text that may be a
/// `cashu:`/`cashu://` URI and/or percent-encoded; None if it isn't one.
pub fn bare_token(raw: &str) -> Option<String> {
    let t = raw.trim();
    let t = strip_prefix_ci(t, "cashu://").or_else(|| strip_prefix_ci(t, "cashu:")).unwrap_or(t);
    if t.len() > 3 * MAX_TOKEN_LEN {
        return None; // don't even percent-decode it
    }
    let t = if t.contains('%') {
        percent_encoding::percent_decode_str(t).decode_utf8().ok()?.into_owned()
    } else {
        t.to_string()
    };
    // Base64 / base64url ('.' appears in some legacy multi-part tokens).
    let charset = |b: u8| b.is_ascii_alphanumeric() || b"-_+/=.".contains(&b);
    let ok = (12..=MAX_TOKEN_LEN).contains(&t.len())
        && (t.starts_with("cashuA") || t.starts_with("cashuB"))
        && t.bytes().all(charset);
    ok.then_some(t)
}

/// Decode a token (raw or `cashu:` URI) into a display summary.
///
/// Permissive mode is for rendering: a V4 token whose CBOR this minimal
/// reader can't walk still yields a generic summary (no amount). Strict mode
/// (`/pay`) has no fallback: the token must decode cleanly and carry a
/// positive amount, so junk and truncated tokens are never sent as money.
pub fn decode(raw: &str, strict: bool) -> Option<CashuInfo> {
    let token = bare_token(raw)?;
    let version = token.as_bytes()[5] as char;
    let payload = b64url(&token[6..]).filter(|p| !p.is_empty())?;
    let info = match version {
        'A' => decode_v3(&payload)?,
        'B' => match decode_v4(&payload) {
            Some(info) => info,
            None if strict => return None,
            None => CashuInfo { version, amount: None, unit: None, mint_host: None, memo: None },
        },
        _ => return None,
    };
    if strict && !info.amount.is_some_and(|a| a > 0) {
        return None;
    }
    Some(info)
}

/// Wallets emit base64url and standard base64, padded and unpadded.
fn b64url(s: &str) -> Option<Vec<u8>> {
    const LENIENT: GeneralPurpose = GeneralPurpose::new(
        &alphabet::URL_SAFE,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::RequireNone)
            .with_decode_allow_trailing_bits(true),
    );
    let s: String = s
        .chars()
        .filter(|&c| c != '=')
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            c => c,
        })
        .collect();
    LENIENT.decode(s).ok()
}

/// Adds one proof amount; skips nonsense, Err once the sum is absurd.
fn add_amount(total: &mut u64, seen: &mut bool, amount: u64) -> Result<(), ()> {
    if amount == 0 || amount > MAX_AMOUNT {
        return Ok(());
    }
    *total += amount; // both ≤ MAX_AMOUNT: no overflow
    *seen = true;
    if *total > MAX_AMOUNT { Err(()) } else { Ok(()) }
}

fn decode_v3(payload: &[u8]) -> Option<CashuInfo> {
    let v: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let entries = v.get("token")?.as_array().filter(|e| !e.is_empty())?;
    let (mut total, mut seen, mut mint_host) = (0, false, None);
    for entry in entries {
        if mint_host.is_none() {
            mint_host = entry.get("mint").and_then(|m| m.as_str()).and_then(sanitized_host);
        }
        let proofs = entry.get("proofs").and_then(|p| p.as_array()).into_iter().flatten();
        for amount in proofs.filter_map(|p| p.get("amount")?.as_u64()) {
            add_amount(&mut total, &mut seen, amount).ok()?;
        }
    }
    let text = |k: &str| v.get(k).and_then(|x| x.as_str());
    Some(CashuInfo {
        version: 'A',
        amount: seen.then_some(total),
        unit: text("unit").and_then(sanitized_unit),
        mint_host,
        memo: text("memo").and_then(sanitized_memo),
    })
}

/// Minimal walk of the NUT-00 TokenV4 CBOR map:
/// { "m": mint, "u": unit, "d": memo, "t": [ { "i": bytes, "p": [ { "a": amount, … } ] } ] }
fn decode_v4(payload: &[u8]) -> Option<CashuInfo> {
    let Cbor::Map(pairs) = (Reader { b: payload, i: 0, budget: 50_000 }).value(0)? else {
        return None;
    };
    let mut info = CashuInfo { version: 'B', amount: None, unit: None, mint_host: None, memo: None };
    let (mut total, mut seen) = (0, false);
    for (key, value) in pairs {
        let Cbor::Text(key) = key else { continue };
        match (key.as_str(), value) {
            ("m", Cbor::Text(m)) => info.mint_host = sanitized_host(&m),
            ("u", Cbor::Text(u)) => info.unit = sanitized_unit(&u),
            ("d", Cbor::Text(d)) => info.memo = sanitized_memo(&d),
            ("t", Cbor::Array(groups)) => {
                for (k, v) in groups.into_iter().filter_map(Cbor::into_map).flatten() {
                    let (Cbor::Text(k), Cbor::Array(proofs)) = (k, v) else { continue };
                    if k != "p" {
                        continue;
                    }
                    for (k, v) in proofs.into_iter().filter_map(Cbor::into_map).flatten() {
                        if let (Cbor::Text(k), Cbor::Uint(amount)) = (k, v) {
                            if k == "a" {
                                add_amount(&mut total, &mut seen, amount).ok()?;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    info.amount = seen.then_some(total);
    Some(info)
}

// Display sanitization: every value below is attacker-controlled.

/// Plain ASCII hostnames only: an IDN or odd host gets no label rather than
/// a lookalike one.
fn sanitized_host(mint: &str) -> Option<String> {
    if mint.len() > 512 {
        return None;
    }
    let authority = mint.split_once("://")?.1.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    let ok = !host.is_empty()
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    ok.then(|| host.to_ascii_lowercase().chars().take(48).collect())
}

fn sanitized_unit(unit: &str) -> Option<String> {
    let ok = (1..=12).contains(&unit.len()) && unit.bytes().all(|b| b.is_ascii_alphanumeric());
    ok.then(|| unit.to_string())
}

/// Control and invisible formatting characters (bidi overrides, zero-width
/// joiners…) are dropped, as are newlines; at most 80 characters remain.
fn sanitized_memo(memo: &str) -> Option<String> {
    if memo.chars().count() > 512 {
        return None;
    }
    let invisible = |c: char| {
        c.is_control()
            || matches!(c, '\u{AD}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
    };
    let cleaned: String = memo.chars().filter(|&c| !invisible(c)).collect();
    let cleaned = cleaned.trim();
    (!cleaned.is_empty()).then(|| cleaned.chars().take(80).collect())
}

/// Just enough definite-length CBOR to traverse a TokenV4 map. Bounded in
/// depth, item count and byte length; indefinite lengths and anything else
/// exotic fail the parse.
enum Cbor {
    Uint(u64),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    /// Parsed and skipped (byte strings, negatives, floats…).
    Opaque,
}

impl Cbor {
    fn into_map(self) -> Option<Vec<(Cbor, Cbor)>> {
        match self {
            Cbor::Map(m) => Some(m),
            _ => None,
        }
    }
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
    /// Total item budget, so hostile nesting can't run away.
    budget: u32,
}

impl<'a> Reader<'a> {
    const MAX_DEPTH: u32 = 16;
    const MAX_CONTAINER: u64 = 10_000;

    fn value(&mut self, depth: u32) -> Option<Cbor> {
        if depth >= Self::MAX_DEPTH || self.budget == 0 {
            return None;
        }
        self.budget -= 1;
        let (major, arg) = self.head()?;
        Some(match major {
            0 => Cbor::Uint(arg),
            1 | 7 => Cbor::Opaque, // argument already consumed
            2 => {
                self.take(arg)?;
                Cbor::Opaque
            }
            3 => match std::str::from_utf8(self.take(arg)?) {
                Ok(s) => Cbor::Text(s.to_string()),
                Err(_) => Cbor::Opaque,
            },
            4 if arg <= Self::MAX_CONTAINER => {
                Cbor::Array((0..arg).map(|_| self.value(depth + 1)).collect::<Option<_>>()?)
            }
            5 if arg <= Self::MAX_CONTAINER => Cbor::Map(
                (0..arg)
                    .map(|_| Some((self.value(depth + 1)?, self.value(depth + 1)?)))
                    .collect::<Option<_>>()?,
            ),
            6 => return self.value(depth + 1), // tag: parse the tagged value
            _ => return None,
        })
    }

    /// Head byte plus its argument; indefinite lengths are refused.
    fn head(&mut self) -> Option<(u8, u64)> {
        let h = *self.b.get(self.i)?;
        self.i += 1;
        let arg = match h & 0x1F {
            n @ 0..=23 => n as u64,
            n @ 24..=27 => {
                let bytes = self.take(1 << (n - 24))?;
                bytes.iter().fold(0, |v, &b| v << 8 | b as u64)
            }
            _ => return None,
        };
        Some((h >> 5, arg))
    }

    fn take(&mut self, n: u64) -> Option<&'a [u8]> {
        let b = self.b;
        let end = self.i.checked_add(usize::try_from(n).ok()?)?;
        let s = b.get(self.i..end)?;
        self.i = end;
        Some(s)
    }
}

// ---------------------------------------------------------------------------
// Links in messages
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Enum))]
pub enum PayKind {
    Cashu,
    Lightning,
    Bitcoin,
    Zcash,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct PayLink {
    pub kind: PayKind,
    /// cashu:, lightning: or bitcoin: URI for an external wallet, rebuilt
    /// from validated parts only.
    pub uri: String,
    /// The bare token, invoice or address, for copying.
    pub raw: String,
    /// "500 sat · mint.example.com", "Lightning invoice · 2500 sat"…
    pub label: String,
    pub memo: Option<String>,
}

/// A message's payment links, plus its text with long ones shortened (the
/// chip shows them; a 2 KB token is just noise in a bubble).
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "ffi", derive(uniffi::Record))]
pub struct Payments {
    pub text: String,
    pub links: Vec<PayLink>,
}

pub fn scan(text: &str) -> Payments {
    let mut end = text.len().min(MAX_SCAN);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut links: Vec<PayLink> = Vec::new();
    for word in text[..end].split_whitespace() {
        let word = word.trim_matches(|c: char| "()[]{}<>\"'`,.;:!?".contains(c));
        if let Some(link) = parse_link(word) {
            if !links.iter().any(|l| l.raw == link.raw) {
                links.push(link);
            }
            if links.len() == MAX_LINKS {
                break;
            }
        }
    }
    let mut text = text.to_string();
    for l in links.iter().filter(|l| l.raw.len() > 40) {
        text = text.replace(&l.raw, &format!("{}…", &l.raw[..16])); // raw is ASCII
    }
    Payments { text, links }
}

/// `/pay <token>`: the bare token to send. Only a cleanly decoding Cashu
/// token with a positive amount passes.
pub fn pay_token(arg: &str) -> Result<(String, CashuInfo), String> {
    let info = decode(arg, true).ok_or("not a valid Cashu token (cashuA… / cashuB…)")?;
    Ok((bare_token(arg).expect("decoded"), info))
}

fn parse_link(word: &str) -> Option<PayLink> {
    if strip_prefix_ci(word, "cashu").is_some() {
        let token = bare_token(word)?;
        let info = decode(&token, false)?;
        let parts: Vec<String> = info.display_amount().into_iter().chain(info.mint_host).collect();
        return Some(PayLink {
            kind: PayKind::Cashu,
            uri: format!("cashu:{token}"),
            raw: token,
            label: if parts.is_empty() { "Cashu token".into() } else { parts.join(" · ") },
            memo: info.memo,
        });
    }
    if let Some(rest) = strip_prefix_ci(word, "bitcoin:") {
        return parse_bitcoin(rest);
    }
    if let Some(link) = parse_zcash(word) {
        return Some(link);
    }
    let ln = strip_prefix_ci(word, "lightning:").unwrap_or(word).to_ascii_lowercase();
    if ln.len() > MAX_LN_LEN {
        return None;
    }
    let label = if ln.starts_with("lnurl") && bech32(&ln, "lnurl") {
        "LNURL".to_string()
    } else {
        let amount = bolt11_msat(&ln)?;
        match amount {
            Some(msat) if msat % 1000 == 0 => format!("Lightning invoice · {} sat", msat / 1000),
            Some(msat) => format!("Lightning invoice · {msat} msat"),
            None => "Lightning invoice".to_string(),
        }
    };
    Some(PayLink { kind: PayKind::Lightning, uri: format!("lightning:{ln}"), raw: ln, label, memo: None })
}

/// `hrp` + '1' + bech32 data (lowercase input).
fn bech32(s: &str, hrp: &str) -> bool {
    const CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    match s.rsplit_once('1') {
        Some((h, data)) => {
            h == hrp && data.len() >= 6 && data.bytes().all(|b| CHARSET.contains(&b))
        }
        None => false,
    }
}

/// A BOLT11 invoice (lowercase): Some(amount in msat, if it names one).
fn bolt11_msat(s: &str) -> Option<Option<u64>> {
    let (hrp, _) = s.rsplit_once('1')?;
    let amount = ["lnbcrt", "lnbc", "lntbs", "lntb", "lnsb"]
        .iter()
        .find_map(|net| hrp.strip_prefix(net))?;
    if s.len() < 20 || !bech32(s, hrp) {
        return None;
    }
    if amount.is_empty() {
        return Some(None);
    }
    let (digits, mult) = match amount.as_bytes()[amount.len() - 1] {
        b'm' | b'u' | b'n' | b'p' => amount.split_at(amount.len() - 1),
        _ => (amount, ""),
    };
    if digits.is_empty() || digits.len() > 18 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    let msat = match mult {
        "" => n.checked_mul(100_000_000_000),
        "m" => n.checked_mul(100_000_000),
        "u" => n.checked_mul(100_000),
        "n" => n.checked_mul(100),
        _ => n.is_multiple_of(10).then_some(n / 10), // pico-BTC: tenths of a msat
    }?;
    Some(Some(msat))
}

/// `bitcoin:<address>[?amount=<btc>]`; other BIP21 parameters are dropped.
fn parse_bitcoin(rest: &str) -> Option<PayLink> {
    let (addr, query) = rest.split_once('?').unwrap_or((rest, ""));
    if !(25..=90).contains(&addr.len()) || !addr.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let amount = query.split('&').find_map(|kv| kv.strip_prefix("amount=")).filter(|a| {
        let (whole, frac) = a.split_once('.').unwrap_or((a, ""));
        (1..=8).contains(&whole.len())
            && frac.len() <= 8
            && whole.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit())
    });
    let short = format!("{}…{}", &addr[..6], &addr[addr.len() - 4..]);
    Some(PayLink {
        kind: PayKind::Bitcoin,
        uri: match amount {
            Some(a) => format!("bitcoin:{addr}?amount={a}"),
            None => format!("bitcoin:{addr}"),
        },
        raw: addr.to_string(),
        label: match amount {
            Some(a) => format!("Bitcoin · {short} · {a} BTC"),
            None => format!("Bitcoin · {short}"),
        },
        memo: None,
    })
}

/// ZIP-321 `zcash:` requests and bare Zcash addresses, checksums verified:
/// transparent t1/t3 (Base58Check), Sapling zs (Bech32), Unified u and TEX
/// (Bech32m). Only amount and (shielded) memo survive from the URI.
fn parse_zcash(word: &str) -> Option<PayLink> {
    let (rest, is_uri) = match strip_prefix_ci(word, "zcash:") {
        Some(r) => (r, true),
        None => (word, false),
    };
    let (addr, query) = rest.split_once('?').unwrap_or((rest, ""));
    let shielded = zcash_address(addr)?;
    // Base58 is case-sensitive; Bech32(m) is canonically lowercase.
    let addr = if addr.starts_with("t1") || addr.starts_with("t3") { addr.to_string() } else { addr.to_ascii_lowercase() };
    let param = |k: &str| query.split('&').find_map(|kv| kv.strip_prefix(k).map(str::to_string));
    let amount = param("amount=").filter(|a| {
        let (whole, frac) = a.split_once('.').unwrap_or((a, ""));
        is_uri
            && (1..=8).contains(&whole.len())
            && frac.len() <= 8
            && whole.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit())
    });
    // ZIP-321 memos are base64url of up to 512 bytes, for shielded payees.
    let memo_param = param("memo=").filter(|m| {
        is_uri && shielded && m.len() <= 684 && m.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    });
    let memo = memo_param.as_deref().and_then(b64url).and_then(|b| String::from_utf8(b).ok());
    let params: Vec<String> = [("amount", &amount), ("memo", &memo_param)]
        .into_iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| format!("{k}={v}")))
        .collect();
    let uri = match params.is_empty() {
        true => format!("zcash:{addr}"),
        false => format!("zcash:{addr}?{}", params.join("&")),
    };
    let short = format!("{}…{}", &addr[..6], &addr[addr.len() - 4..]);
    let kind = if shielded { "shielded" } else { "transparent" };
    Some(PayLink {
        kind: PayKind::Zcash,
        uri,
        raw: addr.clone(),
        label: match amount {
            Some(a) => format!("Zcash · {kind} · {short} · {a} ZEC"),
            None => format!("Zcash · {kind} · {short}"),
        },
        memo: memo.as_deref().and_then(sanitized_memo),
    })
}

/// Some(shielded?) for a valid mainnet Zcash address.
fn zcash_address(a: &str) -> Option<bool> {
    if a.len() > 2048 {
        return None;
    }
    if a.starts_with("t1") || a.starts_with("t3") {
        let raw = crate::tao::bs58_decode(a)?;
        let (body, check) = raw.split_at(raw.len().checked_sub(4)?);
        use sha2::{Digest, Sha256};
        let ok = raw.len() == 26
            && matches!(body[..2], [0x1c, 0xb8] | [0x1c, 0xbd])
            && Sha256::digest(Sha256::digest(body))[..4] == *check;
        return ok.then_some(false);
    }
    // Bech32(m) is single-case.
    if a.bytes().any(|b| b.is_ascii_lowercase()) && a.bytes().any(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let a = a.to_ascii_lowercase();
    const BECH32: u32 = 1;
    const BECH32M: u32 = 0x2bc8_30a3;
    match a.rsplit_once('1')? {
        ("zs", d) if d.len() == 75 => bech32_check(&a, BECH32).then_some(true),
        ("u", _) => bech32_check(&a, BECH32M).then_some(true),
        ("tex", _) => bech32_check(&a, BECH32M).then_some(false),
        _ => None,
    }
}

/// BIP-173/350 checksum over `hrp1data` (lowercase).
fn bech32_check(s: &str, constant: u32) -> bool {
    const CHARSET: &[u8] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
    let Some((hrp, data)) = s.rsplit_once('1') else { return false };
    if data.len() < 6 {
        return false;
    }
    let mut values: Vec<u8> = hrp.bytes().map(|b| b >> 5).collect();
    values.push(0);
    values.extend(hrp.bytes().map(|b| b & 31));
    for b in data.bytes() {
        match CHARSET.iter().position(|&c| c == b) {
            Some(v) => values.push(v as u8),
            None => return false,
        }
    }
    let mut chk: u32 = 1;
    for v in values {
        let top = chk >> 25;
        chk = (chk & 0x1ff_ffff) << 5 ^ v as u32;
        for (i, g) in [0x3b6a_57b2, 0x2650_8e6d, 0x1ea1_19fa, 0x3d42_33dd, 0x2a14_62b3].iter().enumerate() {
            if (top >> i) & 1 == 1 {
                chk ^= g;
            }
        }
    }
    chk == constant
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    /// Vectors from an independent reference encoder (Python, BIP-350
    /// algorithm and hashlib Base58Check), random payloads.
    #[test]
    fn zcash_addresses_and_requests() {
        let sapling = "zs12tezvedxpsfd9zgctk2sa6ypxcy3vmmtzy730rtvplfeq8lj8xs6p90jp7fe2egvlyuqkaagr2v";
        let unified = "u13mdjyjntyj9payjw3lg2uts6jjf2xvzlrzxtvyysp70rglaw3pkuv5rhjhk8ghzv8l9javk88c2fxnyx0ms90wnjfxdl5ys7sd4j4s2hymh866c276438suwjt9wp52s27c4nxrljnx8gywhzlc527dj4ggqlwanf7je8l4w6fey3dmzuw44sp0skj35yy";
        assert_eq!(zcash_address(sapling), Some(true));
        assert_eq!(zcash_address(unified), Some(true));
        assert_eq!(zcash_address("tex1wedzh8qa0c8n03zfyx7n7etyat0h79p28jtaql"), Some(false));
        assert_eq!(zcash_address("t1UJVtMxqZ83ukDxakgKjgs1aWpmKFkRpYm"), Some(false));
        assert_eq!(zcash_address("t3N2yyXZNZGf4h68QndWDJf1yD4LvYs4ez7"), Some(false));
        // Wrong checksum flavour, a flipped character, mixed case, junk.
        assert_eq!(zcash_address("zs1jpglxg9smwpl88484k7s6a8xmmrl8hawej8kgetxvsd8hgnxpucprlp4wq53c4uep5dqq9whvjg"), None);
        assert_eq!(zcash_address(&sapling.replace("zs12t", "zs12u")), None);
        assert_eq!(zcash_address("t1UJVtMxqZ83ukDxakgKjgs1aWpmKFkRpYn"), None);
        assert_eq!(zcash_address("Zs12tezvedxpsfd9zgctk2sa6ypxcy3vmmtzy730rtvplfeq8lj8xs6p90jp7fe2egvlyuqkaagr2v"), None);
        assert_eq!(zcash_address("u1hello"), None);

        let memo = URL_SAFE_NO_PAD.encode("thanks for lunch");
        let p = scan(&format!("send to zcash:{sapling}?amount=1.5&memo={memo}&message=<x> please"));
        assert_eq!(p.links[0].kind, PayKind::Zcash);
        assert_eq!(p.links[0].uri, format!("zcash:{sapling}?amount=1.5&memo={memo}"));
        assert_eq!(p.links[0].label, "Zcash · shielded · zs12te…gr2v · 1.5 ZEC");
        assert_eq!(p.links[0].memo.as_deref(), Some("thanks for lunch"));
        // Bare addresses are chips too; transparent ones never carry a memo.
        let p = scan(&format!("my addr {unified} and t1UJVtMxqZ83ukDxakgKjgs1aWpmKFkRpYm"));
        assert_eq!(p.links.len(), 2);
        assert_eq!(p.links[1].label, "Zcash · transparent · t1UJVt…RpYm");
        let p = scan("zcash:t1UJVtMxqZ83ukDxakgKjgs1aWpmKFkRpYm?amount=99999999999&memo=aGk");
        assert_eq!(p.links[0].uri, "zcash:t1UJVtMxqZ83ukDxakgKjgs1aWpmKFkRpYm");
    }

    fn v3(json: serde_json::Value) -> String {
        format!("cashuA{}", URL_SAFE_NO_PAD.encode(json.to_string()))
    }

    fn v3_token(amounts: &[i64], memo: Option<&str>) -> String {
        let proofs: Vec<_> =
            amounts.iter().map(|a| serde_json::json!({"amount": a, "id": "00", "secret": "s", "C": "02"})).collect();
        let mut j = serde_json::json!({"token": [{"mint": "https://Mint.Example.com:3338/x", "proofs": proofs}], "unit": "sat"});
        if let Some(m) = memo {
            j["memo"] = m.into();
        }
        v3(j)
    }

    // Tiny definite-length CBOR encoder, independent of the reader.
    fn head(major: u8, v: u64) -> Vec<u8> {
        match v {
            0..=23 => vec![major << 5 | v as u8],
            24..=0xFF => vec![major << 5 | 24, v as u8],
            _ => [vec![major << 5 | 27], v.to_be_bytes().to_vec()].concat(),
        }
    }
    fn text(s: &str) -> Vec<u8> {
        [head(3, s.len() as u64), s.as_bytes().to_vec()].concat()
    }
    fn map(pairs: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
        let mut out = head(5, pairs.len() as u64);
        for (k, v) in pairs {
            out.extend(text(k));
            out.extend(v);
        }
        out
    }
    fn array(items: Vec<Vec<u8>>) -> Vec<u8> {
        [head(4, items.len() as u64), items.concat()].concat()
    }
    fn v4_token(amounts: &[u64]) -> String {
        let proofs = amounts
            .iter()
            .map(|&a| map(vec![("a", head(0, a)), ("s", text("secret")), ("c", [head(2, 2), vec![2, 0xAB]].concat())]))
            .collect();
        let cbor = map(vec![
            ("m", text("https://mint.example.com")),
            ("u", text("sat")),
            ("d", text("Thank you")),
            ("t", array(vec![map(vec![("i", [head(2, 1), vec![0]].concat()), ("p", array(proofs))])])),
        ]);
        format!("cashuB{}", URL_SAFE_NO_PAD.encode(cbor))
    }

    #[test]
    fn cashu_decodes_v3_and_v4() {
        let a = decode(&v3_token(&[2, 8], Some("thanks!")), false).unwrap();
        assert_eq!(a.version, 'A');
        assert_eq!(a.display_amount().as_deref(), Some("10 sat"));
        assert_eq!(a.mint_host.as_deref(), Some("mint.example.com"));
        assert_eq!(a.memo.as_deref(), Some("thanks!"));
        let b = decode(&v4_token(&[1, 4, 16]), true).unwrap();
        assert_eq!((b.version, b.amount, b.memo.as_deref()), ('B', Some(21), Some("Thank you")));
        // URI and percent-encoded forms.
        let t = v3_token(&[7], None);
        for w in [format!("cashu:{t}"), format!("CASHU://{t}"), format!("cashu:{}", t.replace('e', "%65"))] {
            assert_eq!(decode(&w, false).unwrap().amount, Some(7), "{w}");
        }
    }

    #[test]
    fn cashu_fails_closed_on_hostile_input() {
        // Negative, zero and absurd amounts don't count; an absurd sum rejects.
        assert_eq!(decode(&v3_token(&[-5, 3], None), false).unwrap().amount, Some(3));
        assert_eq!(decode(&v3_token(&[0], None), false).unwrap().amount, None);
        assert!(decode(&v3_token(&[0], None), true).is_none());
        assert_eq!(decode(&v3_token(&[i64::MAX / 2], None), false).unwrap().amount, None);
        assert!(decode(&v3_token(&[1_500_000_000_000_000; 3], None), false).is_none());
        // Memo: no newlines, control or bidi characters; capped.
        let memo = format!("line1\nline2\u{7}\u{202E}{}", "x".repeat(300));
        let m = decode(&v3_token(&[1], Some(&memo)), false).unwrap().memo.unwrap();
        assert!(!m.contains(['\n', '\u{7}', '\u{202E}']) && m.chars().count() == 80);
        // Unwalkable V4: generic when rendering, refused for /pay.
        let junk = format!("cashuB{}", URL_SAFE_NO_PAD.encode([0xFF, 0xFF, 0xFF, 0, 1, 2]));
        assert_eq!(decode(&junk, false).unwrap().amount, None);
        assert!(decode(&junk, true).is_none());
        let v4 = v4_token(&[1, 4, 16]);
        assert!(decode(&v4[..v4.len() - 12], true).is_none());
        // Deep nesting is bounded.
        let mut deep = head(0, 1);
        for _ in 0..64 {
            deep = array(vec![deep]);
        }
        assert!(decode(&format!("cashuB{}", URL_SAFE_NO_PAD.encode(deep)), true).is_none());
        // Truncations and random bytes never panic.
        let v3t = v3_token(&[1, 2, 4], Some("memo"));
        for t in [&v3t, &v4] {
            for n in (0..t.len()).step_by(3) {
                let _ = decode(&t[..n], false);
            }
        }
        assert!(decode(&v3t[..v3t.len() - 10], false).is_none());
        let mut x: u32 = 1;
        for len in 0..300 {
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    x as u8
                })
                .collect();
            let _ = decode(&format!("cashuA{}", URL_SAFE_NO_PAD.encode(&bytes)), false);
            let _ = decode(&format!("cashuB{}", URL_SAFE_NO_PAD.encode(&bytes)), false);
        }
        for bad in ["hello world", "cashuA", "cashuA{not-base64!}", &format!("cashuC{}", "a".repeat(50))] {
            assert!(bare_token(bad).is_none(), "{bad}");
        }
        assert!(bare_token(&format!("cashuA{}", "Q".repeat(500_000))).is_none());
    }

    #[test]
    fn links_are_found_in_messages() {
        let t = v3_token(&[1], None);
        let p = scan(&format!("here you go: cashu:{t} enjoy! and again ({t})."));
        assert_eq!(p.links.len(), 1);
        assert_eq!((p.links[0].kind.clone(), p.links[0].raw.as_str()), (PayKind::Cashu, t.as_str()));
        assert!(!p.text.contains(&t) && p.text.starts_with("here you go: cashu:cashuA"));

        let inv = "lnbc2500u1pvjluezpp5qqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqqqsyqcyq5rqwzqfqypqdq5xysxxatsyp3k7enxv4jsxqzpu";
        let p = scan(&format!("pay me LIGHTNING:{} or bitcoin:bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq?amount=0.001&label=x", inv.to_uppercase()));
        assert_eq!(p.links[0].label, "Lightning invoice · 250000 sat");
        assert_eq!(p.links[0].uri, format!("lightning:{inv}"));
        assert_eq!(p.links[1].uri, "bitcoin:bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq?amount=0.001");
        assert_eq!(p.links[1].label, "Bitcoin · bc1qar…5mdq · 0.001 BTC");
        assert_eq!(bolt11_msat("lnbc1pvjluezpp5qqqsyqcyq5rqwz"), Some(None));
        assert_eq!(bolt11_msat("lnbc25p1pvjluezpp5qqqsyqcyq5r"), None); // pico must end in 0

        let odd = scan("bitcoin:bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq?amount=1e9&x=<script>");
        assert_eq!(odd.links[0].uri, "bitcoin:bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq");
        for none in ["just talking about cashu here", "cashuAshort", "lnbcfoo1qqqqqqqqqqqqqqqqqqqqq",
            "bitcoin:not;an&address", "lightning:javascript:alert(1)"]
        {
            assert!(scan(none).links.is_empty(), "{none}");
        }
        assert!(pay_token("cashuAjunk").is_err());
        assert_eq!(pay_token(&format!(" cashu:{t} ")).unwrap().0, t);
    }
}
