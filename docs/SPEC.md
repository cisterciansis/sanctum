# Technical Specification — Sanctum Messenger

**Working name:** Sanctum (rename before launch; must not use SimpleX marks)
**Status:** Draft v1 · 2026-07-28
**Companion docs:** [FEASIBILITY.md](FEASIBILITY.md) · [PRD.md](PRD.md)

## 1. Architecture overview

```
┌─────────────── iOS client (fork of simplex-chat/apps/ios) ───────────────┐
│  SwiftUI UI (rebranded, iMessage-style)                                  │
│  libsimplex (Haskell core, C FFI)  ── SQLite (local, encrypted)          │
│  Media Vault module (Swift, S3 SigV4) ── AES-256-GCM client-side crypto  │
└───────┬──────────────────┬───────────────────────┬───────────────────────┘
        │ TLS/TCP 5223     │ TLS 443               │ HTTPS
        ▼                  ▼                       ▼
┌───────────────┐  ┌───────────────┐      ┌──────────────────┐
│  smp-server   │  │  xftp-server  │      │ Hippius S3       │
│  (VPS #1)     │  │  (VPS #1)     │      │ s3.hippius.com   │
└───────┬───────┘  └───────────────┘      │ (SN75, encrypted │
        │ notifier creds                  │  blobs only)     │
        ▼                                 └──────────────────┘
┌───────────────┐        ┌─────────────────────────────────┐
│  ntf-server   │──APNs──▶ Apple Push (our bundle ID/key)  │
│  (VPS #2)     │        └─────────────────────────────────┘
└───────────────┘
┌──────────────────────────────────────────────────────────┐
│  AI bot backend (VPS #2): simplex-chat npm (Node.js)     │
│  → https://llm.chutes.ai/v1 (SN64, pay-per-token)        │
│  Appears in-app as a normal contact: "Sanctum AI"        │
└──────────────────────────────────────────────────────────┘
```

Message delivery, E2E encryption (double ratchet + post-quantum KEM), groups, calls: **unchanged upstream SimpleX protocol.** We add zero protocol modifications in v1. All Sanctum-specific features live in the app layer.

## 2. Server infrastructure

### 2.1 Hosts

| Host | Runs | Sizing | Notes |
|---|---|---|---|
| `relay1.<domain>` (VPS #1) | smp-server, xftp-server, Caddy | 2 vCPU / 4 GB / 80 GB SSD | Message + file relay |
| `svc1.<domain>` (VPS #2) | ntf-server, AI bot (Node.js) | 1 vCPU / 2 GB | Push + bot |

Both: Debian stable, Docker Compose, unattended-upgrades, UFW (5223, 443, 80, SSH only), fail2ban. Official images: `simplexchat/smp-server:latest`, `simplexchat/xftp-server:latest`; ntf-server built from `simplex-chat/simplexmq`.

- **DNS is load-bearing**: hostnames + offline-CA fingerprints are embedded in every client's queue addresses. Domains never lapse; CA key stored offline (it survives TLS cert rotation and is the server identity).
- **Persistence**: Docker volumes for `/etc/opt/simplex*` + `/var/opt/simplex*`. smp-server uses journal/store-log (Postgres migration when volume justifies it). Nightly encrypted off-site backup of config + store via restic.
- **XFTP** quota 50 GB initially; chunks auto-expire at 48 h.
- **Scaling path**: SimpleX clients accept multiple preset relays; add `relay2` in a second region and ship in an app update. Queue rotation migrates traffic gradually — no flag-day.

### 2.2 Hippius VM migration (deferred — decision gate)

When Hippius SEV-SNP VMs GA, re-evaluate against a hard checklist: public IPv4 + our port 5223, persistent disk, stable hostname or static IP, 30 days observed uptime running a canary smp-server, exit path (restore backup to VPS in <1 h). Until all pass, relays stay on VPS. The address-migration mechanism (new server added to presets, old retired over months) is the same as the scaling path — this is why we don't fear the move.

## 3. Client

### 3.1 Fork strategy

- Fork `simplex-chat/simplex-chat` at latest stable tag. iOS first (`apps/ios`), Android later (`apps/multiplatform`).
- **Minimal-diff discipline**: UI/branding/feature changes only in app layers; never touch `src/` (Haskell core) in v1. Rebase per upstream stable release (~monthly cadence).
- Preset server addresses (`smp://<fp>@relay1.<domain>`, `xftp://...`) replace SimpleX defaults; users may still add their own or SimpleX's public relays (openness is a trust feature; keep it).
- Rebrand completely: name, icons, color, onboarding, App Store listing. AGPL-3.0 retained; public GitHub repo from day one (compliance + trust marketing).

### 3.2 iMessage-style UX layer (SwiftUI, fork-local)

Blue/gray bubble aesthetic (legally distinct — no Apple trade dress copying), send animation, tapbacks mapped to SimpleX reactions (protocol-native), typing indicators / read receipts / edit / unsend (delete-for-everyone) — all already in the protocol; this is a **reskin, not new plumbing**. Details in PRD §4.

### 3.3 Media Vault (the Hippius feature)

Purpose: iMessage-style durable media library. XFTP transfers expire in 48 h; the Vault keeps what the user saves.

- Pure app-layer Swift module. Flow: user saves an attachment (or enables auto-save) → app encrypts blob with **AES-256-GCM, random per-object key** → uploads to Hippius S3 (SigV4, path-style, region `decentralized`, multipart >5 MB) → stores object key + content key + SHA-256 in the **local encrypted SQLite DB** (keys never leave the device unencrypted).
- Bucket-per-user is not possible without our own credential broker; v1 model: **user brings their own Hippius account** (advanced users) **or** app-provisioned sub-key from a Sanctum Hippius account with per-user prefix ACLs via a minimal broker service on VPS #2. Broker holds no content keys — it can enumerate ciphertext objects only. Start with brokered; document the trust boundary honestly.
- Key backup: content keys live in the chat DB, which the user can export encrypted (upstream feature). Losing the DB without export = losing vault contents. Stated plainly in UI.
- Cost: $0.003/GB-mo, zero egress → 10 GB vault ≈ $0.03/mo per user.

### 3.4 Push notifications

Run our own `ntf-server` with our Apple bundle ID + APNs token auth key; address preset in the fork. E2E-encrypted notification payloads (X25519) per upstream design — APNs sees timing/volume only. Android needs none of this (background service).

## 4. AI assistant (the Chutes feature)

- A standard SimpleX bot: Node.js service using the `simplex-chat` npm package (drives the Haskell core over its WebSocket ChatCommand API), running on VPS #2 with its own SQLite state.
- Users add "Sanctum AI" via a preset contact address. Every message is answered by calling **Chutes** (`https://llm.chutes.ai/v1`, OpenAI-compatible; default a cheap fast model, e.g. Mistral-class at ~$0.02–0.10/M tokens).
- Privacy contract, stated in-app: messages to the bot leave E2E encryption at the bot boundary and are processed by Chutes miners. The bot is opt-in, clearly labeled, and never in group chats uninvited. No logging beyond transient processing.
- v2 option: **"Sign in with Chutes"** OAuth (`chutes:invoke` scope) so users' AI usage bills their own Chutes account — decentralized AI where we don't even hold the inference bill.

## 5. Security invariants

1. **No protocol changes.** E2E crypto, queue rotation, PQ-KEM: upstream, untouched, upgraded by rebase.
2. **Hippius holds ciphertext only.** Every object client-side encrypted with a key that never leaves the user's device/DB. A full Hippius compromise yields ciphertext + object sizes/timing.
3. **Chutes sees only what users explicitly send to the labeled AI contact.** Never message content from human↔human chats.
4. **Broker (if used) is key-blind** and validates: authenticated app attestation, per-user prefix, size caps, rate limits. Reject-unknown-fields on all its endpoints; allowlist content-type; length/range checks on every field.
5. **Server identity keys (CA) offline.** TLS certs rotate; CA never does without a planned migration.
6. **Fail closed**: bot and broker down → core messaging unaffected (they're satellites, not dependencies).
7. All servers: TLS mandatory, no plaintext listeners, generic auth errors, throttling on broker/bot endpoints.

## 6. What we deliberately do NOT build (v1)

- No custom protocol client in Swift/TS (no spec-complete third-party implementation exists; the Haskell core is the product).
- No XFTP-on-Hippius storage backend (XFTP wants local disk; fighting it buys nothing at our scale).
- No relay hosting on Chutes (GPU-only, ephemeral, wrong in every dimension — see FEASIBILITY.md).
- No Android/desktop in v1 (upstream fork path exists; sequenced in PRD).
- No custom notification infra beyond upstream ntf-server.
- No tokenomics/wallet integration in the app. Paying infra bills in TAO ≠ putting a wallet in a messenger.

## 7. Verification plan

| Milestone | Check |
|---|---|
| Relays up | Official SimpleX app connects via manually-added `smp://…@relay1` address; two devices exchange messages + a file |
| Fork builds | CI produces signed TestFlight build from public repo |
| Push works | Message received as APNs notification with app killed, via our ntf-server |
| Vault round-trip | Encrypt→upload→delete local→download→decrypt = byte-identical; object on Hippius verified unreadable without key |
| Bot works | "Sanctum AI" answers in <5 s p50; Chutes outage degrades to error message, not app failure |
| Rebase drill | Upstream stable release merged into fork in <1 day with green CI |
| Restore drill | Relay restored from backup onto fresh VPS with same hostname; existing clients reconnect without re-pairing |
