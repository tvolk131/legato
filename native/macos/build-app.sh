#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/../.."
export MACOSX_DEPLOYMENT_TARGET=26.0
export CLANG_MODULE_CACHE_PATH="${TMPDIR:-/tmp}/legato-clang-cache"
export SWIFTPM_MODULECACHE_OVERRIDE="${TMPDIR:-/tmp}/legato-swift-module-cache"
cargo build --locked -p legato-swift-bridge
build_path="${LEGATO_SWIFT_BUILD_PATH:-native/macos/.build}"
swift build --package-path native/macos --scratch-path "$build_path"
bin_dir=$(swift build --package-path native/macos --scratch-path "$build_path" --show-bin-path)
app="$PWD/target/swiftui/Legato.app"
mkdir -p "$app/Contents/MacOS"
cp "$bin_dir/Legato" "$app/Contents/MacOS/Legato"
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
sed "s/__VERSION__/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
codesign --force --sign "${LEGATO_SIGN_IDENTITY:--}" "$app"
printf '%s\n' "$app"
