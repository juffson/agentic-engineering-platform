#!/bin/sh
# Build AEP Studio (Tauri) with the aep runner next to the app binary.
set -eu
cd "$(dirname "$0")"
cargo build --locked --manifest-path ../Cargo.toml --bin aep
[ -d node_modules ] || npx -y pnpm@9 install --frozen-lockfile
cargo tauri build --debug --bundles app
app="src-tauri/target/debug/bundle/macos/AEP Studio.app"
cp ../target/debug/aep "$app/Contents/MacOS/aep"
codesign --force --deep --sign - "$app"
printf '%s\n' "$PWD/$app"
