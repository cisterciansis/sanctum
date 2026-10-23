#!/bin/sh
# Sanctum one-liner install: binary download, no toolchain needed.
#
#   curl -fsSL https://raw.githubusercontent.com/cisterciansis/sanctum/master/install.sh | sh
#
# With an invite link, it drops you straight into the chat:
#
#   curl -fsSL https://raw.githubusercontent.com/cisterciansis/sanctum/master/install.sh | sh -s -- "sanctum://v2/..."
#
# Installs sanctum-tui and sanctum (CLI) into ~/.sanctum/bin. No sudo,
# nothing else touched. Joining an invite needs no account of any kind.
set -eu

REPO="cisterciansis/sanctum"

fail() { printf 'sanctum: %s\n' "$*" >&2; exit 1; }

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target="macos-arm64" ;;
  *) fail "no prebuilt binary for $(uname -s)/$(uname -m) yet — build from source:
  git clone https://github.com/$REPO && cd sanctum && cargo build --release -p sanctum-tui
  (needs Rust and protobuf; Linux builds land with CI soon)" ;;
esac

bin_dir="$HOME/.sanctum/bin"
mkdir -p "$bin_dir"
base="https://github.com/$REPO/releases/latest/download"

for name in sanctum-tui sanctum; do
  echo "downloading $name ($target)…"
  curl --proto '=https' --tlsv1.2 -fsSL -o "$bin_dir/$name" "$base/$name-$target" \
    || fail "download failed: $base/$name-$target"
  chmod +x "$bin_dir/$name"
done

echo ""
echo "Installed to $bin_dir"
case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) echo "Add it to your PATH:  export PATH=\"\$HOME/.sanctum/bin:\$PATH\"" ;;
esac

if [ "$#" -ge 1 ]; then
  case "$1" in
    sanctum://v2/*)
      echo "joining invite…"
      # stdin is the curl pipe here; the TUI needs the real terminal.
      exec "$bin_dir/sanctum-tui" "$1" < /dev/tty
      ;;
  esac
fi
echo "Run:  $bin_dir/sanctum-tui"
