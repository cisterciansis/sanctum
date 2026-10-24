# sanctum

Private messaging with no servers. Messages are end-to-end encrypted dead
drops on decentralized storage ([Hippius](https://hippius.com), Bittensor).
An invite link carries everything the other person needs — no account, no
signup, no phone number. Nothing to run, nothing to trust, nothing to shut
down.

- **Signal protocol.** Official [libsignal](https://github.com/signalapp/libsignal):
  PQXDH handshake (X25519 + Kyber1024, post-quantum) and the double ratchet.
  Forward secrecy and post-compromise security, per message.
- **No man in the middle.** The invite carries the inviter's identity key;
  a key bundle not signed by it is rejected.
- **No accounts.** No phone number, email or username. You are a keypair.
- **Invites are capabilities.** Each chat gets two fresh mailboxes; the link
  holds a token scoped to just those two.
- **Unstoppable.** Messages live on [Hippius](https://hippius.com),
  decentralized storage on Bittensor (subnet 75). No Sanctum servers exist.
- **Nearly free.** An invite costs the inviter cents of storage; joining is free.
- **Encrypted at rest.** History in SQLCipher, key in the OS keychain.
- **Tor built in** on iOS (any SOCKS proxy on desktop). Off by default.
- **You decide what stays.** Disappearing messages (1 min to 1 year),
  delete for everyone, opt-in reciprocal read receipts.
- **Open and checked.** AGPL-3.0; protocol laws machine-checked in Bend.
- **Next:** wallet-keypair credentials for storage, no API token at all.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/cisterciansis/sanctum/master/install.sh | sh
```

Join an invite straight into the chat:

```sh
curl -fsSL https://raw.githubusercontent.com/cisterciansis/sanctum/master/install.sh | sh -s -- "sanctum://v2/..."
```

## Use

- `sanctum-tui` — terminal client. `Ctrl-N` invite · paste a link to join ·
  `/file <path>` attach · `/name <name>` rename · `Ctrl-R` react · `Ctrl-U`
  unsend · `Ctrl-S` settings
- `sanctum-desktop` — GUI (build from source)
- Inviting people needs a [Hippius](https://console.hippius.com) API token in
  `~/.sanctum/settings.json` (`"hippius_api_token": "..."`). Joining needs
  nothing.

## Build

```sh
cargo build --release        # needs Rust and protobuf (brew install protobuf)
bend proofs/PROOF.bend       # machine-checked protocol laws (bend-lang.com)
```

AGPL-3.0
