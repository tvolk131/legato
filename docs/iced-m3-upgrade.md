# iced-m3 beta.4 → beta.6

Checked against crates.io on October 29, 2026. Legato pins `iced-m3` to
`0.1.0-beta.6` and enables its optional `material-symbols` feature.

## Changes supplied by the dependency

Beta.5:

- Cheaper hover elevation: cached endpoint shadows and tiled corner/edge
  templates avoid large blur operations on every animation frame. Fractional
  elevation and sheets also use bounded shadow templates.
- Cheaper press ripples: avoid full-surface SVG clipping masks during expansion
  and reuse the raster while fading. Animation timing remains unchanged.
- Determinate wavy progress travels at one wavelength per second by default,
  even while its value is unchanged. `.wave_speed(0.)` restores stationary waves.
  Pause, viewport clipping, reduced motion and completion stop continuous work;
  speed changes preserve phase. Legato currently uses loading indicators and
  sliders, so it does not acquire an always-running progress animation.
- More rendering, interaction and scheduling regressions, plus performance
  fixtures for GPU and software rendering. Upstream benchmark improvements
  describe its fixtures; Legato has not measured a whole-app performance gain.

Beta.6:

- Official, offline Material Symbols: 4,157 typed names and 66,512 selectable
  variants across Outlined/Rounded, filled/unfilled, and optical sizes 20/24/40/48.
  Weight is fixed at 400 and grade at 0; Sharp and other weights are not bundled.
  SVGs use the existing theme-aware icon renderer, including disabled alpha.
- `NavigationItem::selected_icon` supplies separate selected artwork for bars
  and collapsed, expanded and modal rails.
- Snackbar dismissal timers continue when the window loses focus. Hover in an
  active window and modal coverage still pause them. Applications can restore
  the former behavior with `.pause_on_window_blur(true)`.
- Held pointer/keyboard actions are cancelled when retained dialogs close,
  reopen or briefly cover a control. Reopened children resume interaction
  timers, tooltips remain usable, and covered keyboard focus is restored.
- Side- and bottom-sheet transitions continue while the window is unfocused;
  animation timing remains separate from input suspension.
- More rendered interruption regressions for floating labels, selection
  controls, dialogs, sheets and FAB reveals, including reversal, retained
  rebuilds, reduced motion, exact endpoints and idle redraw scheduling.

iced remains at 0.14.0; iced-m3's minimum Rust version remains 1.88, below
Legato's 1.99 requirement. Existing application calls need no breaking migration.

## Legato's icon migration

All five custom icons are replaced with official Rounded symbols:

- Devices: `devices`.
- Arrangement: `dashboard`.
- Settings: `settings`.
- Expand/collapse navigation: `menu`.
- Native tray/menu bar: `screen_share`.

Navigation uses unfilled artwork normally and filled artwork for the selected
page. The tray rasterizes the bundled SVG to black RGBA with antialiased alpha,
using the same `resvg` version already used by iced. macOS keeps template tinting.
System-provided pointer shapes and native menu controls remain OS-managed.

The catalog embeds approximately 6.8 MiB of compressed artwork and decodes only
requested chunks. Its decoded-chunk cache is limited to 1 MiB; requested SVG
handles are cached separately. Builds and runtime need no icon downloads or
font registration. The companion's exact build-time `serde_json = 1.0.145` pin
causes Cargo to resolve the workspace's compatible `serde_json = "1"` requirement
to 1.0.145, down from 1.0.151. No other existing package versions change.

Legato remains MIT licensed. Google's Apache-2.0 artwork license and attribution
are preserved in `licenses/material-symbols/` and included in release archives.

Sources: [beta.5 release](https://github.com/tvolk131/iced-m3/releases/tag/v0.1.0-beta.5),
[beta.6 release](https://github.com/tvolk131/iced-m3/releases/tag/v0.1.0-beta.6),
[icon API](https://docs.rs/iced-m3/0.1.0-beta.6/iced_m3/material_symbols/).
