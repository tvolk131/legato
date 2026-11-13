# Legato for Mac

The Mac app: a SwiftUI menu bar app for macOS 26+ on Apple silicon. It links the
Rust engine (`crates/legato-swift-bridge`) as a static library and shares its
identity, settings, protocol and state-directory lock with the CLI and with the
Iced app that Windows ships. Releases package it as `Legato.app`.

## Building

Requires Rust 1.99+ and Swift 6.2+ with the macOS 26 SDK. From the repository root:

```sh
native/macos/build-app.sh
open target/swiftui/Legato.app
```

The script builds a debug bridge and Swift binary, assembles the bundle with
`bundle.sh`, and signs it ad hoc (`LEGATO_SIGN_IDENTITY` picks another identity).
The development bundle's id is `io.github.tvolk131.legato.dev`, so its permissions
and login item stay apart from the released app's. It still uses this Mac's Legato
identity and state: quit the installed app first. Permissions granted to the
installed app don't carry over to a development build.

The release workflow builds the bridge and the Swift app in release mode, bundles
them with the same `bundle.sh`, and signs and notarizes the result; see
[the signing guide](../../docs/macos-signing.md). SwiftPM links the bridge from
`target/debug` unless `LEGATO_BRIDGE_LIB_DIR` points elsewhere.

## How it fits together

- `Sources/LegatoBridge/include/LegatoBridge.h` is the C ABI. Commands and the
  state snapshot are JSON; `Tests/LegatoTests/Fixtures/snapshot.json` is a sample
  the Rust tests write and the Swift tests decode, so both sides agree on it.
  Regenerate it with `LEGATO_UPDATE_FIXTURES=1 cargo test -p legato-swift-bridge`.
- The engine calls an observer when its state changes; `AppModel` then reads one
  snapshot. Calls run on the `Bridge` actor, off the main thread.
- Arrangement editing and snapping live in `legato_engine::arrange`, shared with
  the Iced app.
- The window uses NavigationSplitView (Devices, Arrangement), sheets for adding a
  device and pairing, a Settings scene (Command-comma) and a MenuBarExtra. Legato
  appears in the Dock only while its window is open. Closing the window leaves the
  engine running; Quit waits for sharing to stop and input to be restored.
- Opening a second copy shows the running one instead.
- "Open at login" registers `Contents/Library/LaunchAgents/<bundle id>.login.plist`
  ([LoginAgent.plist](LoginAgent.plist)) with SMAppService. It starts Legato with
  `--hidden`, in the menu bar only, and starts it again if it crashes (at most once
  a minute). The first launch replaces the Iced app's
  `~/Library/LaunchAgents/Legato.plist`, keeping it on if it was on.
- Logs go to `legato-app.log` in the state directory, like the other front ends.

The display viewer and display options are Windows-only.

## Tests

```sh
cargo test -p legato-swift-bridge
native/macos/test.sh
```

The bridge tests cover commands, settings, pairing cancellation, notices and the
JSON contract. The Swift suite decodes the contract fixture and compares screenshots;
see [Tests/README.md](Tests/README.md). CI runs both on macOS 26.

Screenshots show layout, not runtime behavior. Check these on real hardware with
the same candidate on both machines: pairing and arrangement with Windows, input
handover, clipboard and files, extra-display hosting, closing and reopening the
window during pairing, the login item and hidden launch, crash restart, and
permission retention across a signed upgrade from the Iced app.

Design references: [Apple's Liquid Glass adoption guide](https://developer.apple.com/documentation/TechnologyOverviews/adopting-liquid-glass),
[materials](https://developer.apple.com/design/human-interface-guidelines/materials),
and [native Settings](https://developer.apple.com/documentation/foundation/adding-a-settings-interface-to-your-app).
