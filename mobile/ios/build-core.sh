#!/bin/sh
# Build the Rust core for iPhone + simulator, regenerate Swift bindings, and
# package both as SanctumCore.xcframework for the Xcode project.
set -eu
cd "$(dirname "$0")/../.."

# rustup's toolchain must win over any Homebrew rustc (no iOS std there).
export PATH="$HOME/.cargo/bin:$PATH"

# Match the app's minimum iOS so C deps (OpenSSL, ring) agree with Rust.
export IPHONEOS_DEPLOYMENT_TARGET=17.0
# iOS only needs the static library; skip the dylib link. `tor` embeds Arti
# (no Tor daemon to point at on iOS); it only runs if enabled in Settings.
for t in aarch64-apple-ios aarch64-apple-ios-sim; do
  cargo rustc --release -p sanctum-core --features ffi,tor --target "$t" --crate-type staticlib
done
cargo build -p sanctum-core --features ffi
cargo run -q -p sanctum-core --features ffi-cli --bin uniffi-bindgen -- \
  generate --library target/debug/libsanctum_core.dylib \
  --language swift --out-dir mobile/ios/Generated

out=mobile/ios/SanctumCore.xcframework
rm -rf "$out"
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libsanctum_core.a \
  -library target/aarch64-apple-ios-sim/release/libsanctum_core.a \
  -output "$out"
echo "built $out"
