# Sanctum

Private messaging with **no message servers**. Mailboxes are dead-drop buckets
on [Hippius](https://hippius.com) (Bittensor subnet 75) — decentralized,
erasure-coded storage that holds only ciphertext. An invite link carries
everything a stranger needs to message you, including their write-only
credentials: **senders never sign up for anything.**

```
you ──invite link (bucket + scoped token + your public key)──▶ contact
contact ──E2E-encrypted blob──▶ Hippius dead drop ──poll──▶ you (decrypt, delete)
```

- **No servers to run, none to trust, none to shut down.** Message persistence
  lives on a permissionless miner network; anyone can run their own gateway
  ([hippius-s3](https://github.com/thenervelab/hippius-s3) is Apache-2.0).
- **E2E encrypted** (X25519 + XSalsa20-Poly1305 today; libsignal PQXDH/double
  ratchet is the planned upgrade — the envelope carries a version byte).
- **Capability security.** A sender's token can write into exactly one inbox
  bucket and nothing else — verified live against the gateway's ACL semantics.
  Revoke the token, the contact is gone.
- **Machine-checked protocol laws.** [`proofs/LAWS.bend`](proofs/LAWS.bend)
  states delivery invariants; `bend PROOF.bend` proves them
  ([Bend](https://bend-lang.com)) and must print `ALL PROOFS CHECK`.

## Layout

| Path | What |
|---|---|
| `core/` | `sanctum-core` — identity, invites, envelopes, Hippius transport (Rust, portable to mobile via FFI) |
| `cli/` | `sanctum` — keygen / send / recv / live demo |
| `tui/` | `sanctum-tui` — terminal client (ratatui); runs anywhere, no GUI stack |
| `desktop/` | `sanctum-desktop` — iMessage-style GUI (gpui-kit, Zed's engine) |
| `proofs/` | Protocol model + laws + proofs (Bend) |
| `docs/` | Feasibility research, SPEC (v1 relay design), SPEC-V2 (this dead-drop design), PRD |
| `experiment/` | The live-gateway experiment that validated the design |

Both clients share the same core session; run two instances (`SANCTUM_ROLE=alice`
/ `SANCTUM_ROLE=bob`, creds in env) and they pair through the dead drop and chat
— GUI and TUI interchangeably.

## Quickstart

```sh
cargo build
cargo test
bend proofs/PROOF.bend        # ALL PROOFS CHECK

# live round-trip (needs Hippius credentials, see experiment/hippius.env.example)
source experiment/hippius.env
SENDER_KEY=$SENDER_KEY SENDER_SECRET=$SENDER_SECRET \
OWNER_KEY=$OWNER_KEY OWNER_SECRET=$OWNER_SECRET cargo run -p sanctum-cli demo
```

## Metadata privacy: Tor / UR proxy

The storage gateway only ever holds ciphertext, but it can see client IPs.
Set `SANCTUM_PROXY` and every gateway request (all clients) is routed through
a SOCKS5/HTTP proxy — no proxy, no silent fallback to direct:

```sh
brew install tor && tor   # local Tor SOCKS proxy on 9050
SANCTUM_PROXY=socks5://127.0.0.1:9050 cargo run -p sanctum-cli demo
```

The same knob accepts a [UR network](https://ur.xyz) (Bittensor SN25 dVPN)
proxy endpoint. Verified live through Tor: full E2E round-trip in ~4.5 s.

## Design

Read [docs/SPEC-V2-DEADDROP.md](docs/SPEC-V2-DEADDROP.md). Short version: a
messenger is crypto + key directory + mailbox + push nudge. Crypto is a
library, the mailbox is decentralized storage with capability ACLs, the key
directory anchors on the Bittensor chain's commitments pallet, and the only
server left anywhere is a ~100-line push pinger for iOS.

## License

AGPL-3.0. The whole point is that you can read it, run it, and host it.
