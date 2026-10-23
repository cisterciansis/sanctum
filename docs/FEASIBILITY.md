# Feasibility Report — "Sanctum": SimpleX-based Messenger × Bittensor (Hippius + Chutes)

**Date:** 2026-07-28
**Question:** Can we build an iMessage-style private messenger on the SimpleX protocol, with its infrastructure running on Hippius (Bittensor SN75) and/or hosted on Chutes (Bittensor SN64)?

## Verdict

**As literally proposed — SimpleX relays running on Hippius or Chutes — not feasible today.**
**As a re-scoped hybrid — feasible and attractive:** SimpleX relays on conventional VPS (migrating to Hippius confidential VMs when they ship), Hippius S3 for encrypted media/backup storage, Chutes for AI features. Each Bittensor subnet does the one thing it's actually good at.

---

## What each component actually requires / offers

### SimpleX (the protocol stack)

Verified against the simplexmq and simplex-chat repos and official hosting docs:

- Three servers needed for full iOS parity: **smp-server** (message relay, port 5223 TLS/TCP), **xftp-server** (file chunks, port 443), **ntf-server** (APNs push bridge). All Haskell, all AGPL-3.0, all with official Docker images (`simplexchat/smp-server`, `simplexchat/xftp-server`).
- Servers are lightweight (community deployments run on 1 vCPU / 1 GB RAM) but **require**:
  - a **long-lived raw TLS/TCP listener** — not HTTP, incompatible with serverless;
  - a **stable hostname + CA fingerprint**, because both are baked into every queue address held by every contact (`smp://<fingerprint>@host`). Changing host breaks all existing connections;
  - **persistent storage** (store log / journal / PostgreSQL as of v6.4+) so queues survive restarts.
- Servers store almost nothing: ephemeral message queues (default 21-day undelivered expiry), file chunks (48 h expiry). No accounts, no identifiers, no message history.
- Client reality: the only complete protocol implementation is the Haskell core. Official iOS/Android/desktop apps link it via C FFI. There is a current **Node.js package (`simplex-chat` npm)** driving the core over a WebSocket ChatCommand API — the standard bot/backend surface. No browser-native client (raw TCP transport).
- **AGPL-3.0 everywhere**: a branded fork is legal but the entire modified client and any server modifications must be published under AGPL. Rebrand fully (SimpleX name/logo are trademarks).
- iOS push requires running our own **ntf-server with our own Apple bundle ID + APNs key** — clean for a branded fork (impossible for a re-skin that isn't a real fork).

### Hippius (Bittensor SN75)

- **Today it is S3-compatible object storage, period.** Endpoint `https://s3.hippius.com` (+ `eu-central-1`, `us-east-1` caches), AWS SigV4, path-style, works with any S3 client. Signup via Google/GitHub OAuth; pay by Stripe card or TAO (1 credit = $1).
- The original IPFS pinning offering is **dead** (replaced Feb 2026 by their Rust "Arion" engine: client-side encryption, Reed-Solomon 10+20 erasure coding across miners, CRUSH placement). The old CLI/Python SDK is deprecated mid-rewrite — S3 API is the only supported path.
- **Compute (AMD SEV-SNP confidential VMs) is roadmap, not product** — console says "coming soon", no specs, no pricing, no networking model. Nothing can host a daemon on Hippius today.
- No published latency benchmarks, **no SLA**, young codebase. Erasure-coded fan-out reads = object-storage latencies. Architecturally wrong for real-time messaging; fine for blobs.
- Pricing is genuinely good: **$0.003/GB-month, zero egress fees**.

### Chutes (Bittensor SN64)

- A "chute" is an arbitrary Docker container behind a stable HTTPS subdomain — but **GPU-only** (NodeSelector minimum: 1 GPU, 16 GB VRAM; no CPU tier despite doc-site filler claiming otherwise). Running a CPU-shaped relay there means renting a ≥16 GB GPU at ~$1+/hr for nothing.
- Raw TCP is possible only via **Jobs**: pinned to a single miner instance, per-instance host:port (ports ≥8002 only — 5223 not even allowed), no persistent volumes, `miner_terminated` is a tracked normal event, ToS explicitly disclaims uptime. Everything a messaging relay must not be.
- **"Chutes Apps" as a hosting product does not exist.** The term in the wild means either (a) the catalog page every chute gets, (b) "Sign in with Chutes" OAuth apps — which run on *your* hosting while inference bills to the *user's* Chutes account, or (c) Rayon's own consumer apps.
- What Chutes is genuinely good at: **pay-per-token LLM inference** (OpenAI-compatible `https://llm.chutes.ai/v1`), custom model serving, and the "Sign in with Chutes" model where users pay for their own AI usage.

---

## Why the literal architecture fails

| SimpleX relay requirement | Chutes | Hippius |
|---|---|---|
| Long-lived raw TLS/TCP listener (5223) | Jobs only, ports ≥8002, single miner, unstable | No compute product at all |
| Stable hostname baked into queue addresses | Per-instance host:port, churns on miner exit | n/a |
| Persistent disk (queue store / Postgres) | No volumes; filesystem ephemeral | S3 blobs only (no block storage / mounts) |
| Always-on, cheap CPU | GPU-only, ≥16 GB VRAM billed hourly | n/a |
| SLA / reliability | ToS: "as-is", no guarantees | No SLA |

## The feasible architecture (summary — full detail in SPEC.md)

1. **Relays (SMP + XFTP + ntf) on boring infrastructure**: 1–2 small VPS with Docker, stable DNS, persistent volumes. ~$10–20/month total. This is also what SimpleX's own docs, Linode/DO marketplace images, and every production deployment do. **Planned migration path**: when Hippius SEV-SNP VMs ship with public IPs + persistent disks, relays move there — the "runs on Bittensor" goal deferred, not abandoned. Decision gate, not a promise.
2. **Hippius S3 as the encrypted blob layer** where object storage fits the job:
   - **Media vault**: client-side-encrypted attachment/backup storage beyond XFTP's 48 h window (iMessage-style "keep my photos").
   - Optionally later, XFTP chunk storage offload — but XFTP writes local disk; don't fight it in v1.
3. **Chutes for AI**: an in-app AI assistant contact (a SimpleX bot via the Node.js API, backend calling `llm.chutes.ai`), optionally "Sign in with Chutes" so power users bill their own account. This is decentralized AI inference — the honest Chutes integration.
4. **Client**: fork the AGPL iOS app (SwiftUI + Haskell core FFI), full rebrand, our three server addresses preset, our APNs credentials in our ntf-server. iMessage-clone UI polish happens in the fork's SwiftUI layer.

## Cost & effort envelope

| Item | Cost |
|---|---|
| 1–2 VPS (relays) | $10–20/mo |
| Hippius storage | $0.003/GB-mo, zero egress (≈$3/mo per TB stored) |
| Chutes inference | Pay-per-token (fractions of a cent per message); or user-billed via OAuth |
| Apple Developer | $99/yr |
| Effort | MVP (fork + rebrand + own servers): weeks, small team. Hippius media vault: + weeks (app-layer Swift/Kotlin, no Haskell changes). AI bot: days. |

## Top risks

1. **AGPL** — the entire client fork must stay open source. Acceptable for a privacy product (it's a trust feature); fatal if proprietary client code was the plan.
2. **Haskell core** — any protocol-level change requires Haskell competence. Mitigation: don't change the protocol; build features in the app layer (Swift/Kotlin/TS).
3. **Hippius maturity** — no SLA, young engine, team pseudonymous. Mitigation: only client-side-encrypted, re-uploadable data goes there; nothing load-bearing for message delivery.
4. **Upstream drift** — SimpleX moves fast (Postgres store, chat relays for large groups). Mitigation: minimal-diff fork, rebase on stable releases.
5. **App Store** — AGPL apps on the App Store are a known gray area (SimpleX itself ships this way); our fork is our copyright for our additions, which is the accepted pattern.

**Bottom line:** ship a rebranded SimpleX fork on cheap stable relays now; let Hippius carry encrypted blobs (its actual product) and Chutes carry AI inference (its actual product); revisit relay decentralization when Hippius VMs are real.
