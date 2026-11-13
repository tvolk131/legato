#!/bin/bash
# Assembles Legato.app from a built Swift executable. Used by build-app.sh and the
# release workflow, which signs the result.
#   bundle.sh <executable> <Legato.app> <bundle id> <version> <build number>
# The version must be numeric (e.g. 0.3.0): Apple's bundle versions have no suffixes.
set -euo pipefail
if [ $# -ne 5 ]; then
    echo "usage: $0 <executable> <Legato.app> <bundle id> <version> <build number>" >&2
    exit 2
fi
exe=$1 app=$2 id=$3 version=$4 build=$5
here="$(cd "$(dirname "$0")" && pwd)"
plist="$here/../../packaging/macos/Info.plist"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Library/LaunchAgents"
cp "$exe" "$app/Contents/MacOS/Legato"
sed "s/__VERSION__/$version/g" "$plist" > "$app/Contents/Info.plist"
plutil -replace CFBundleIdentifier -string "$id" "$app/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$build" "$app/Contents/Info.plist"
sed "s/__BUNDLE_ID__/$id/g" "$here/LoginAgent.plist" \
    > "$app/Contents/Library/LaunchAgents/$id.login.plist"
plutil -lint -s "$app/Contents/Info.plist" "$app/Contents/Library/LaunchAgents/$id.login.plist"
