#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/../.."
export MACOSX_DEPLOYMENT_TARGET=26.0
export CLANG_MODULE_CACHE_PATH="${TMPDIR:-/tmp}/legato-clang-cache"
export SWIFTPM_MODULECACHE_OVERRIDE="${TMPDIR:-/tmp}/legato-swift-module-cache"
cargo build --locked -p legato-swift-bridge
flags=(--disable-xctest)
developer=$(xcode-select -p)
if [[ "$developer" == */CommandLineTools ]]; then
    frameworks="$developer/Library/Developer/Frameworks"
    flags+=(-Xswiftc -F -Xswiftc "$frameworks" -Xlinker "-F$frameworks" -Xlinker -rpath -Xlinker "$frameworks" -Xlinker -rpath -Xlinker "$developer/Library/Developer/usr/lib")
fi
swift test --package-path native/macos "${flags[@]}" "$@"
