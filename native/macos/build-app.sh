#!/bin/bash
# Builds a development Legato.app in target/swiftui, signed ad hoc unless
# LEGATO_SIGN_IDENTITY names a signing identity. It uses its own bundle id, so its
# permissions and login item stay apart from the released app's. (Both still share
# this Mac's Legato identity and state; only one can run at a time.)
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
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
native/macos/bundle.sh "$bin_dir/Legato" "$app" io.github.tvolk131.legato.dev "${version%%-*}" 1
codesign --force --sign "${LEGATO_SIGN_IDENTITY:--}" "$app"
printf '%s\n' "$app"
