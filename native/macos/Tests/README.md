# Mac app screenshots

The Swift Testing suite renders the real SwiftUI/AppKit views from fixtures in an
offscreen window that never becomes key or main. It does not start the engine,
send input, touch the clipboard, change login items or request permissions. It
also decodes `Fixtures/snapshot.json`, the bridge's JSON contract written by
`cargo test -p legato-swift-bridge`.

```sh
native/macos/test.sh
```

Baselines live in `Snapshots`; `metadata.json` records the OS that made them. Views
render at a fixed 2x scale, whatever display is attached, so the same OS gives the
same pixels. After a deliberate UI change, run with `LEGATO_UPDATE_SNAPSHOTS=1`,
look at the new PNGs and commit them with the change.

A comparison fails when more than 0.5% of pixels differ by more than 16 color
levels. That tolerates progress spinners and antialiasing but catches layout
changes. A macOS update can change native controls and need new baselines.
`LEGATO_SNAPSHOT_SMOKE=1` (used by CI, whose macOS build differs) renders every
fixture and checks size and that it isn't blank, without comparing pixels. The
rendered PNGs are always written to `/tmp/legato-swift-snapshots`, or to
`LEGATO_SNAPSHOT_ARTIFACTS`.

Settings screenshots compare the input and startup sections; permission rows show
this process's real state, so they're left out. AppKit's vibrant sidebar inside
NavigationSplitView doesn't render through `cacheDisplay`, so there are no
full-window screenshots: check the sidebar, toolbar and menu bar by hand.
