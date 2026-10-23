# sanctum

Private messaging with no servers. Messages are end-to-end encrypted dead
drops on decentralized storage ([Hippius](https://hippius.com), Bittensor).
An invite link carries everything the other person needs — no account, no
signup, no phone number. Nothing to run, nothing to trust, nothing to shut
down.

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
