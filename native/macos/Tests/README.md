# Native macOS screenshots

The Swift Testing suite renders real SwiftUI/AppKit views using fixtures in a
unordered offscreen window that never becomes key or main. It does not start the Rust
engine, send input, modify the
clipboard, change login items or request permissions. Build the Rust bridge first
using the native app build instructions.

```sh
native/macos/test.sh
```

Baselines live in `Tests/Snapshots`; `metadata.json` records the generating OS and rendering configuration. To intentionally update them on the current
macOS version, run with `LEGATO_UPDATE_SNAPSHOTS=1`, inspect the resulting PNGs,
and commit them with the UI change. Comparison allows at most 0.5% of pixels to
differ by more than 16 color levels. This tolerates native progress spinners and
small text antialiasing differences while catching layout changes. Baselines are
native OS renderings: a macOS appearance change may require deliberate review.
`LEGATO_SNAPSHOT_SMOKE=1` renders every fixture and checks nonblank content and image geometry without comparing OS-dependent pixels. Hosted CI uses this mode when its macOS patch differs from the recorded baseline. Actual PNGs are always written to `/tmp/legato-swift-snapshots`, overridable with
`LEGATO_SNAPSHOT_ARTIFACTS`. Tests also reject blank renderings.

Settings screenshots compare the deterministic input/startup section. Permission
rows report read-only process state and are excluded from the image comparison.
Screenshot coverage cannot establish sharing, real-device input, screen capture,
login registration or signed-upgrade permission behavior.

The suite covers 12 content screenshots. AppKit’s layer-backed vibrant sidebar
inside `NavigationSplitView` is not captured reliably by `cacheDisplay`; full
window/sidebar screenshots are therefore excluded rather than committing blank
sidebar images. Verify the native sidebar, toolbar and menu bar interactively.
