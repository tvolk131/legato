# Native macOS GUI

An opt-in SwiftUI front end for macOS 26+ on Apple silicon. The existing Iced app
remains the release default; Windows continues to use it. Both front ends use the
same Rust engine, identity, settings, protocol, and state-directory lock. Quit the
installed app before running this one. No wire-version change is needed.

Build an app bundle from the repository root:

```sh
native/macos/build-app.sh
open target/swiftui/Legato.app
```

Requires Rust 1.99+ and Swift 6.2+ with the macOS 26 SDK. The script builds a debug
bridge and Swift binary and ad-hoc signs the development bundle. It does not
install, replace the installed app, notarize or publish a release. Use the existing
Developer ID signing/notarization process before distributing a candidate. Do not
assume development permissions carry over from the installed signed application.
`LEGATO_SIGN_IDENTITY` selects a local signing identity. For custom Cargo output,
set `LEGATO_BRIDGE_LIB_DIR` when invoking SwiftPM.

The native interface uses NavigationSplitView for Devices/Arrangement, a Settings
scene (Command-comma), native toolbar controls and sheets, and MenuBarExtra.
Standard controls adopt the system appearance and accessibility preferences.
The engine stays alive when the main window closes; Quit waits for sharing to
stop and restore input. Settings changes edit the latest committed configuration.

Implemented Mac feature surfaces:

- LAN discovery, incoming/outgoing pairing, code comparison, cancel/decline and
  waiting states; sharing resumes for paired devices and starts after pairing.
- Connection state/path, active device, file/folder picker, window drops,
  persistent transfer failures, and Finder reveal for successful edge drops.
- Draggable multi-display arrangement using the existing snap algorithm,
  keyboard position menus, primary-display markers, offline caches and diagnostics.
- Controller policy, edge resistance, modifier mapping, reverse wheel, clipboard,
  login item registration, identity/settings-folder access, and permission links.
- Shared engine clipboard/input/network behavior and Mac extra-display hosting.
  Viewer/display-options UI remains Windows-only, as in the original Mac app.

Verification:

```sh
MACOSX_DEPLOYMENT_TARGET=26.0 cargo build --locked -p legato-swift-bridge
native/macos/test.sh
```

See [Tests/README.md](Tests/README.md) for baseline recording, strict comparisons
and hosted rendering checks. The bridge has Rust tests for commands, settings,
pairing cancellation, and arrangement snapping. CI builds/tests Rust on all three
supported targets and builds/renders SwiftUI on macOS 26.

Hardware parity is pending: pair with the same candidate on Windows, test input
handover and recovery, clipboard/files, display arrangement and hosting, close
and reopen the main window during pairing, and verify login and signed-upgrade
permission retention. Screenshot tests establish layout coverage, not those
runtime behaviors. The Swift app remains an opt-in candidate until these pass.

Design references: [Apple's Liquid Glass adoption guide](https://developer.apple.com/documentation/TechnologyOverviews/adopting-liquid-glass),
[materials](https://developer.apple.com/design/human-interface-guidelines/materials),
and [native Settings](https://developer.apple.com/documentation/foundation/adding-a-settings-interface-to-your-app).
