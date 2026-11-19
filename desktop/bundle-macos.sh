#!/bin/sh
set -eu
cd "$(dirname "$0")"
cargo build --locked --manifest-path Cargo.toml --target-dir target
cargo build --locked --manifest-path ../Cargo.toml --bin aep --target-dir target
bundle="target/AEP Studio.app"
mkdir -p "$bundle/Contents/MacOS"
cp target/debug/aep-studio "$bundle/Contents/MacOS/aep-studio"
cp target/debug/aep "$bundle/Contents/MacOS/aep"
cp Info.plist "$bundle/Contents/Info.plist"
codesign --force --sign - "$bundle"
printf '%s\n' "$PWD/$bundle"
