# Product Requirements Document — Sanctum Messenger

**Working name:** Sanctum · Draft v1 · 2026-07-28
**Companion docs:** [FEASIBILITY.md](FEASIBILITY.md) · [SPEC.md](SPEC.md)

## 1. Vision

The private messenger that *feels* like iMessage but *works* like a privacy tool: no phone number, no account, no user identifiers anywhere in the infrastructure — with media that lives on decentralized storage (Hippius/Bittensor) and an optional AI assistant served by decentralized compute (Chutes/Bittensor). Built on the SimpleX protocol, the only messaging protocol without user identifiers; fully open source (AGPL).

**One-liner:** *iMessage polish. Signal-class privacy. No identity required. Powered by Bittensor.*

## 2. Users

| Persona | Need | What wins them |
|---|---|---|
| Privacy-curious iPhone user | Leave WhatsApp/iMessage without a UX downgrade | Familiar bubbles, invite via link/QR, it "just works" |
| Crypto/Bittensor native | Wants apps actually using subnets, not narratives | Real Hippius storage + Chutes inference, verifiable in the open-source repo |
| High-threat-model user (journalist, activist) | Metadata protection above all | SimpleX's no-identifier design, self-host option, reproducible builds |

Primary market: persona 1 acquired through persona 2's community energy.

## 3. Why now / why us

- SimpleX is technically superior on metadata privacy but its stock UX is utilitarian; nobody has shipped a consumer-polished fork.
- Bittensor's app layer is starving for real consumer products; Hippius and Chutes are both GA for exactly the pieces we need (blob storage, inference) — and only those pieces (see FEASIBILITY.md).
- AGPL means our differentiation is design, defaults, and distribution — not secret code. We accept and market that.

## 4. Features

### P0 — MVP (TestFlight)

| # | Feature | Notes |
|---|---|---|
| F1 | 1:1 chat, E2E encrypted | Upstream protocol, our relays preset |
| F2 | iMessage-style UI | Bubble layout, send animation, chat list, dark mode. Legally distinct styling |
| F3 | Invite via link / QR | Upstream one-time + long-term contact addresses |
| F4 | Images, video, voice notes, files | XFTP (≤48 h transfer window) |
| F5 | Tapbacks (reactions), replies, edit, unsend | All protocol-native; UI mapping only |
| F6 | Typing indicators + read receipts (per-chat toggle) | Protocol-native |
| F7 | Push notifications (iOS) | Our ntf-server + APNs |
| F8 | Local DB encryption + passcode/Face ID lock | Upstream |
| F9 | Full rebrand + public repo | AGPL compliance from day one |

### P1 — Differentiators (App Store launch)

| # | Feature | Notes |
|---|---|---|
| F10 | **Media Vault on Hippius** | Save attachments durably; client-side encrypted; auto-save toggle per chat; storage meter |
| F11 | **Sanctum AI contact (Chutes)** | Opt-in bot contact; clear "leaves E2E at the bot" disclosure; summarize/draft/translate/answer |
| F12 | Group chats | Protocol-native (decentralized small groups) |
| F13 | Audio/video calls | Protocol-native (WebRTC via E2E signaling) |
| F14 | Encrypted chat DB export/backup | Upstream feature, surfaced properly (also the Vault key backup) |
| F15 | Advanced: use your own servers / own Hippius account | Openness as a trust feature |

### P2 — Later

Android (fork `apps/multiplatform`) · desktop · large groups/channels (upstream chat-relays feature as it stabilizes) · "Sign in with Chutes" (user-billed AI) · relay migration to Hippius confidential VMs (gate in SPEC §2.2) · reproducible builds.

### Non-goals

No phone-number contact discovery (identity-free is the product; a discovery service would destroy it). No ads, no analytics SDKs, no proprietary components. No crypto wallet, token, or payments in-app. No custom crypto/protocol work. No Android before iOS is excellent.

## 5. UX requirements

- Onboarding to first message < 60 seconds: install → pick display name (local only) → share QR/link → chat. Zero forms, zero permissions except notifications prompt in context.
- Explain-nothing default, learn-more everywhere: privacy tech invisible until tapped ("Why no phone number?" sheet).
- AI + Vault are opt-in with one-screen plain-language disclosures (Vault: "encrypted on your device, stored on Hippius decentralized network"; AI: "messages to this contact are processed by Chutes — your other chats never are").
- Performance: cold launch < 1.5 s on iPhone 12+; message send→render < 150 ms local echo; scroll at 60 fps on 10k-message chats.

## 6. Success metrics

| Metric | Target (6 mo post-launch) |
|---|---|
| TestFlight → App Store | ≤ 3 months from MVP |
| Onboarding completion (install→first message sent) | ≥ 60 % |
| Week-4 retention of users with ≥1 contact | ≥ 35 % |
| Crash-free sessions | ≥ 99.5 % |
| Relay uptime (self-measured) | ≥ 99.9 % monthly |
| Vault adoption among WAU | ≥ 20 % |
| AI contact opt-in among WAU | ≥ 15 % |
| GitHub stars / external contributors | Community heartbeat — track, no vanity target |

No user analytics in-app: metrics come from server-side aggregates that contain no identifiers (relay counts, TestFlight/App Store stats, opt-in crash reports).

## 7. Milestones

| Phase | Duration | Exit criteria (verifiable) |
|---|---|---|
| M0 Infra | 1–2 wk | Stock SimpleX app works E2E through our relays + push via our ntf-server (SPEC §7 checks 1,3) |
| M1 Fork | 2–3 wk | Rebranded build on TestFlight from public repo; rebase drill passes |
| M2 iMessage UX | 3–4 wk | F2–F8 complete; design review vs. iMessage side-by-side |
| M3 Vault | 2–3 wk | Round-trip check (SPEC §7); broker hardened per SPEC §5.4 |
| M4 AI | 1 wk | Bot p50 < 5 s; failure degrades gracefully |
| M5 Launch | 2 wk | App Store review passed; groups + calls (F12–F13) verified; restore drill passed |

~3–4 months to App Store with 1–2 strong iOS engineers + 1 infra/backend (Node/DevOps). Haskell skills **not** required for v1 (hard requirement of the plan).

## 8. Risks & mitigations

| Risk | Sev | Mitigation |
|---|---|---|
| AGPL forces full client publication | — | Embraced: open source is the trust story; differentiation = UX + defaults + community |
| Upstream fork drift | High | Minimal-diff rule (SPEC §3.1); monthly rebase drill is a CI-verified milestone |
| Hippius outage/immaturity | Med | Vault is satellite, not dependency; ciphertext-only exposure; messaging unaffected |
| Chutes outage or model quality | Low | Bot degrades to error message; provider swap is one env var (OpenAI-compatible API) |
| App Store rejection (AGPL / crypto association) | Med | SimpleX precedent on the Store; no token/wallet in app; standard content-moderation responses (user-hosted E2E content, report/block flows) |
| Apple trade-dress complaint | Med | Distinct palette/shapes; "iMessage-style" never used in marketing copy |
| Relay host/domain loss | High | Offline CA, restic backups, restore drill (SPEC §7); second relay in second region at P1 |
| Small-team bus factor | Med | Everything in the public repo incl. infra-as-code; no snowflake servers |

## 9. Open questions

1. Final name + domain (blocks CA generation and APNs setup — decide in M0).
2. Brokered Hippius sub-keys vs. bring-your-own-account only for Vault v1 (SPEC §3.3 recommends brokered; confirm the trust-boundary copy).
3. Default AI model on Chutes (cost vs. quality — benchmark 2–3 during M4).
4. Monetization (out of scope for v1; candidates: paid Vault tiers above free GB, user-billed AI via Chutes OAuth — both fit the no-ads constraint).
