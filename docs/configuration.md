# Configuration and diagnostics

The tray app and CLI share state. Quit the app from its tray/menu-bar menu before
running CLI commands, including `legato doctor`; closing the window leaves the
app running. Only one process can hold a particular state directory's lock.

## State and settings

| Platform | Default state directory |
| --- | --- |
| macOS | `~/Library/Application Support/io.github.tvolk131.Legato/` |
| Windows | `%APPDATA%\tvolk131\Legato\data\` |

`LEGATO_HOME` overrides this directory for both front ends. CLI `--home DIR`
overrides the environment setting. Separate directories give separate identities,
pairings, and settings; they are useful for isolated tests.

| File | Purpose |
| --- | --- |
| `identity.key` | Persistent private device identity; keep it private |
| `peers.json` | Paired identities and names |
| `legato.toml` | Arrangement, switching, keys, scrolling, control policy, clipboard and extra-display settings |
| `screens.json` | Remembered peer display configurations |
| `legato.lock` | OS file lock held while the engine runs; a leftover file alone is harmless |
| `legato-app.log` | Current app run's diagnostics |
| `legato-app.previous.log` | Previous run's log, including the crash before an automatic restart |

The identity is stored in a file, not an OS keychain; signing does not migrate it.
Quit Legato before externally editing `legato.toml`, then restart to load it.
Prefer the app's controls for ordinary edits. TOML sections and defaults are
defined in [`config.rs`](../crates/legato-engine/src/config.rs); unknown fields are
rejected. Pairing and settings survive replacing the application binary.

## Transfers

Explicit sends and Windows edge drops save to the user's `Downloads/Legato`
folder (or a `Legato` folder in the system temporary directory if Downloads cannot
be resolved). Files received for clipboard pasting use the system temporary directory
under `legato-clipboard`. Do not delete that cache while its file list is still
needed for pasting.

Clipboard text is limited to 8 MiB, images to 67,108,864 total pixels (8192×8192),
and a copied-file batch to 512 MiB. Encoded clipboard blobs are also bounded by
the protocol's 64 MiB limit. Use an explicit send for larger file batches; explicit
sends have no configured total-byte cap. They still depend on destination disk space.

## Reporting a problem

Keep the app version, both operating-system versions, display arrangement, exact
steps, and relevant log lines. `legato doctor` prints display, permission, network,
and paired-device diagnostics. The CLI logs to stderr; `-v` selects debug and
`-vv` trace logging. `LEGATO_LOG` overrides the log filter for either front end,
for example `LEGATO_LOG=debug` before launching from a terminal.

In the Windows viewer, **F10** toggles stream statistics and **F11** toggles full
screen. Save an F10 screenshot together with the decoder backend and timing log
lines. Decode time includes GPU waiting and NV12 readback/copy; the GPU-wait field
alone cannot identify a transfer bottleneck. The network component is estimated
from half the round trip. Use sustained motion to assess throughput; a still
screen's low frame rate is not a throughput measurement.

The app attempts one automatic restart after a main-thread crash, suppressing
another restart within a minute. Preserve both logs when investigating a crash.
GPU errors are logged, but recovery does not establish the underlying cause.

For permission problems, check Accessibility and Input Monitoring for input
sharing, Screen Recording on the Mac hosting the extra display, and Local Network
access when prompted. Older ad-hoc alpha upgrades may need fresh grants. See
[macOS signing](macos-signing.md) and the
[release hardware gates](releases/0.3.0-plan.md) before resetting permissions.
