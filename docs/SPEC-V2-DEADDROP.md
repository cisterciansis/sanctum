# Spec v2 — "Dead-Drop" Architecture: Signal-grade messaging with no message servers

**Status:** Design + experiment plan · 2026-10-22
**Relation to v1:** [SPEC.md](SPEC.md) (SimpleX fork + own relays) remains the ship-now path. This is the trustless-native alternative your b1m/libsignal/subtensor links unlocked. Different enough to be its own doc.

## 1. Thesis

A messenger is four primitives. v1 ported SimpleX's *servers*; v2 ports the *functions* onto what the decentralized stack actually offers — and the servers disappear:

| Primitive | Signal (centralized) | v2 (this design) | Verified? |
|---|---|---|---|
| E2E crypto | libsignal | **libsignal, unchanged** — Rust core, official Swift/Java/TS bindings, PQXDH + double ratchet + sealed sender, AGPL | ✅ v0.105.x, active |
| Key directory | Signal's server (trust them) | Prekey bundles on **Hippius public-read prefixes**; log head anchored on **subtensor commitments pallet** | ✅ both primitives exist |
| Mailbox | Signal-Server + FoundationDB/Redis | **Hippius S3 dead-drop buckets** (write-only ACL grants) | ⚠️ ACLs verified in source; anonymous-PUT path unverified |
| Push nudge | APNs/FCM via their server | Minimal stateless pinger (~100 LoC) — the **only** server | — unavoidable on iOS for anyone |

No SMP relay, no XFTP server, no Signal-Server fork (confirmed non-viable: FoundationDB + 4 Redis clusters + DynamoDB + SGX enclaves). Message state lives in client devices and erasure-coded ciphertext blobs across Hippius miners.

## 2. Identity & key distribution

- **Identity = libsignal identity keypair.** No phone number, no username requirement, no registration anywhere.
- Each user publishes a **prekey bundle** (identity key, signed prekey, PQ one-time prekeys) as an object on Hippius at an unguessable public-read path (prefix-scoped anonymous `s3:GetObject` is supported — verified in `public_prefix_policy.py`). The bundle URL + fingerprint IS the contact invite (QR / link), like SimpleX addresses.
- **MITM resistance, two layers:**
  1. Invite links carry the identity-key fingerprint — out-of-band verification, plus Signal-style safety numbers in-app.
  2. Optional **key-transparency anchor**: we run an append-only log of (bundle-hash, pointer) entries; its head hash is published via `commitments::set_commitment` (free, `Pays::No`, 512-byte BigRaw field, ~3100 bytes/epoch budget) from our registered hotkey. Anyone can audit; we cannot rewrite history without the chain showing it. This beats centralized Signal's directory trust.
- **Not** per-user on-chain identity: commitments require a hotkey registered on a subnet (registration burn) — fine for the operator + auditors, wrong for end users. Deliberate simplification; revisit only if per-user anchoring is ever worth the cost.

## 3. Transport: dead-drop queues

Mirrors SimpleX's pairwise unidirectional queues, built from verified Hippius ACL semantics (`acl_service._permission_implies`: WRITE does **not** imply READ; middleware maps PUT/POST/DELETE→WRITE, GET/HEAD/List→READ):

- One **bucket per contact per direction** (bucket-level grants are the finest write-scope available — prefix scoping exists only for public reads). Randomly named, unguessable.
- Recipient owns the bucket, grants **WRITE-only** to the sender's access key (`x-amz-grant-write`, grantee type `AccessKey`). Sender can deposit, never read or list. WRITE includes DELETE — harmless here because each bucket has exactly one sender (self-sabotage only).
- **Send:** libsignal-encrypt (sealed sender) → `PUT <bucket>/<ULID>.bin`. ULID names give ordering; double ratchet tolerates reorder/replay anyway.
- **Receive:** recipient Lists + GETs + deletes on poll. Foreground: poll every few seconds. Background: woken by push nudge (§5).
- **Queue rotation** (SimpleX's metadata trick): periodically create a new bucket, hand it to the contact inside the E2E channel, drain and delete the old one. Caps what gateway access logs can correlate.

### Trust model vs v1
Hippius miners hold erasure-coded **ciphertext shards** — 30-shard Reed-Solomon, client-side encrypted twice (libsignal payload + Hippius layer). The **gateway** (s3.hippius.com) sees access patterns per access key: it is the metadata observer, in the role SimpleX relays play in v1. Mitigations: per-queue random credentials, rotation, Tor-friendly (plain HTTPS), and — because `hippius-s3` is Apache-2.0 — **anyone can run their own gateway** against the subnet, including us and power users. Availability of the official gateway is the main centralization residue.

## 4. The decisive experiment (run before writing any app code)

Costs one Hippius account and an afternoon; kills or confirms the design:

1. Account A creates bucket `q-<random>`; via ACL API grants WRITE (not READ) to account B's access key.
2. As B: `PUT` succeeds; `GET`/`ListObjects` return 403. ← **core claim**
3. As B: attempt `PutBucketAcl`/policy escalation → must fail (only READ_ACP/WRITE_ACP holders may).
4. Anonymous `PUT` to a bucket with AllUsers WRITE grant — source suggests grant evaluation would honor it but the anonymous routing path is **unverified**. If this works, senders need **no Hippius account at all** (bucket URL = capability, pure SimpleX-style) and §3 drops the biggest onboarding wart.
5. Sub-token test: can console sub-tokens call ACL endpoints? (Docs only show read-only/read-write tiers; undocumented.)
6. Measure: PUT→List visibility latency, GET p50/p95 (regional cache endpoints), cost per 1k small objects vs the 1-hour-minimum billing quirk.

**Gate:** steps 1–3 pass and p95 end-to-end under ~5 s → v2 is real. Step 4 failing is survivable (fall back to a key-blind **credential broker** issuing per-queue sender keys — small trusted component, still no message server). Steps 1–3 failing → v2 dies, v1 stands.

### Results (run 2026-10-23 against live s3.hippius.com — `experiment/test-deaddrop.sh`)

| Claim | Result |
|---|---|
| WRITE grant ⇏ READ: grantee GET / LIST / read of others' objects denied | ✅ PASS |
| Pre-grant PUT denied; ACL escalation by grantee denied | ✅ PASS |
| `accessKey="hip_…"` WRITE grant accepted and persisted in bucket ACL | ✅ PASS (syntax: quoted, `accessKey=` not `id=`; owner canonical ID is an SS58 address) |
| Grantee PUT after grant | ⚠️ blocked in our setup — deployed middleware makes sub-token scope *authoritative for same-account* requests and never consults ACLs; **cross-account requests explicitly fall through to ACL grants** ("contractor pattern", `gateway/middlewares/acl.py`). Needs a second account for the positive proof. |
| Anonymous PUT (AllUsers WRITE) | ❌ grant silently dropped; anonymous PUT → 403. Senders need credentials → **credential broker** (or sender-owned accounts) is required. |
| Latency | ✅ PUT→visible ≈ 2.1–2.7 s, GET ≈ 1 s, LIST ≈ 0.9 s — within the ≤5 s gate |

Gateway quirks worth remembering: region `decentralized`, path-style; `SignatureDoesNotMatch` is returned for *any* bad credential (even unknown keys), so it carries no diagnostic signal; console-created buckets and API-created buckets can lag in the console UI.

**Standing conclusion:** superseded by §4b below — the capability-invite model removes the need for cross-account grants and the credential broker entirely.

## 4b. v2.1 — Capability-invite model (verified 2026-10-23; supersedes broker & cross-account ACLs)

The same-account sub-token scoping that blocked the ACL test is the better primitive. Verified live:

- A sub-token scoped to one bucket has **full mailbox power inside it** (PUT/GET/LIST/DELETE all pass) and **nothing outside it**: other buckets denied in every mode, CreateBucket denied, ACL self-escalation denied. A scope-less sub-token default-denies everything; a bucket-scoped one can never CreateBucket (`sub_token_scope.py`).
- So: **recipient mints one inbox bucket + one sub-token per contact and ships the credential inside the invite link/QR.** The sender's "account" travels with the invitation — zero signup. Sender sees only their own ciphertext; recipient revokes the token to cut them off. SimpleX's per-queue capability model, rebuilt on Hippius.

Provisioning is fully programmatic (verified against api.hippius.com's live Swagger spec; Hippius's own smoke tests mint sub-tokens this way in production):

- Management API `api.hippius.com/api/`, auth `Authorization: Token <platform DRF token>`: `POST /objectstore/buckets/`, `POST /objectstore/sub-tokens/` (`scope_type: single_bucket`, `bucket_names`, plus `actions`, `allowed_prefixes`, `ip_allowlist`, `not_before`, `expires_at`), `/revoke/`, `/rotate/`. Master tokens likewise. Rotate is atomic against the gateway scope store.
- Caps: no visible bucket-count cap (closed-source backend quotas unknown — probe); bucket names are **globally unique** (use random names) and reserved first segments banned; ≤1000 buckets per sub-token scope (we use 1). Gateway rate-limit middleware currently removed. `allowed_prefixes`/`actions` may later permit one-bucket-many-prefixes or write-only tokens — probe, could simplify further.
- **Payments are wallet-native after signup:** credits live on-chain (`Credits.FreeCredits`, read by the gateway every 5 min; S3 returns 402 when empty). Top up by sending TAO to a per-user deposit address (`GET /billing/substrate-address/`, min $20) or `POST /billing/native/buy/`; also x402 USDC and BTCPay. No Stripe required.
- **Signup is the one centralized residue:** wallet/mnemonic auth endpoints exist (`POST /auth/mnemonic/` + `/auth/verify/`, response has `is_new`, server flag `WALLET_LOGIN_REQUIRE_SUBSTRATE_PROOF`) but docs gate mnemonic auth to pre-Nov-2025 accounts; new users bootstrap once via Google/GitHub/Apple OAuth, then never again (everything after runs on the DRF token + TAO). Whether `/auth/verify/` still opens fresh wallet accounts behind the flag is untested; worth one probe and/or asking the Hippius team to re-enable — the code is written for it. Privacy note: the OAuth bootstrap ties mailbox ownership to a Google/GitHub identity; senders stay anonymous, owners don't (burner accounts or mnemonic-auth re-enable are the mitigations).

**Revised message flow:** invite = {inbox bucket name, sub-token key+secret, prekey-bundle pointer, identity fingerprint}. On accept, the counterparty provisions the reciprocal inbox via their own account and sends its capability through the first inbox. Each direction: sender PUTs libsignal-encrypted blobs; recipient polls, GETs, deletes. Rotation = mint new bucket+token, swap inside the E2E channel, revoke old.

## 5. The irreducible server: push pinger

iOS forbids background sockets *and* background polling; everyone (Signal, SimpleX) runs an APNs bridge. Ours is minimal and stateless: after a PUT, sender POSTs `{push_token_ciphertext}` (an APNs token the recipient handed out E2E, encrypted to a pinger key) → pinger decrypts the token and sends an empty "check your queues" APNs nudge. Sees timing + token, never content, never queue names. ~100 lines; any $5 host; horizontally dumb. Android: WorkManager polling or FCM equivalent.

## 6. The rest

- **Attachments / media:** encrypt (AES-256-GCM, random key) → PUT to Hippius → send pointer+key in-message. The v1 "Media Vault" (SPEC.md §3.3) is now just the same path with longer retention — one storage system total, XFTP gone.
- **Groups:** v1 of v2 = client-side pairwise fan-out (fine ≤ ~50 members; cost = N PUTs). Later: libsignal sender keys over shared drop buckets, or MLS — watch Beam's `mls-go`. `// ceiling: fan-out O(N) per message; sender-keys when group sizes demand`.
- **AI contact:** unchanged from v1 — a bot client (Node + libsignal TS bindings now, no Haskell) answering via Chutes `llm.chutes.ai`.
- **Beam/b1m (SN105):** not load-bearing. Bulk-transfer subnet; no hosting, no persistence, centralized coordinator (BeamCore), homepage stats hardcoded. Its MLS "Rooms" pub/sub (free messages) is a candidate **optional fast path** (sub-second delivery, typing indicators) layered over the durable dead-drop path — revisit in 6 months if it decentralizes coordination.
- **Client:** this path means building the app proper (SwiftUI + libsignal-swift + SQLCipher), not forking SimpleX's UI. More client work than v1, massively less server work, and in mainstream languages.

## 7. Honest comparison

| | v1: SimpleX fork | v2: dead-drop |
|---|---|---|
| Time to TestFlight | ~2 months (fork, proven core) | ~4–5 months (new client, after experiment gate) |
| Servers we run | SMP + XFTP + ntf (VPS) | Push pinger only (+ optional broker) |
| Trust residue | Our relays (queue metadata) | Hippius gateway (access metadata) — self-hostable |
| Crypto | SimpleX double ratchet + PQ | libsignal PQXDH/ratchet/sealed-sender |
| Client languages | Swift UI over Haskell core FFI | Swift/Kotlin/TS, no Haskell anywhere |
| Latency | Sub-second (persistent TCP) | Seconds (poll + nudge) — iMessage-async OK, typing indicators degraded |
| Novelty risk | Low (upstream-proven) | Real: unverified anonymous-PUT, Hippius maturity, polling battery cost |
| "Runs on Bittensor" story | Partial (storage + AI satellites) | Strong: all message persistence on SN75, AI on SN64, chain-anchored keys |

**Recommendation:** run the §4 experiment first — it's cheap and decides everything. If it passes, v2 is the product and v1's relay work is skipped entirely; if it fails, v1 ships and v2 waits for Hippius to mature. Don't build both.
