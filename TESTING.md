# Testing guide

Supported platforms are macOS 26+ on Apple silicon and Windows 11 on x64 and
ARM64, in both directions. Builds require Rust 1.99+; current stable runs the
platform suites and releases. The MSRV job only type-checks on Rust 1.99.

## Current CI and local checks

[ci.yml](.github/workflows/ci.yml) runs on every PR, pushes to `main` and
`release/**`, and manual dispatch. It has no documentation path filter.

| Job | Current coverage |
| --- | --- |
| Formatting on Ubuntu | `cargo fmt --all --check`; no Linux application support or platform tests |
| macOS, Windows x64, Windows ARM64 | Workspace Clippy/tests with warnings denied |
| Selected platform tests | Ignored macOS/Windows input tests; Windows app GPU/presentation tests using DX12 WARP |
| Rust 1.99 on macOS | `cargo check --locked --workspace` |

Run appropriate checks locally from the repository root:

```sh
cargo fmt --all --check
RUSTFLAGS='-D warnings' cargo clippy --locked --workspace --all-targets
RUSTFLAGS='-D warnings' cargo test --locked --workspace
```

On PowerShell set `$env:RUSTFLAGS = '-D warnings'` before the Cargo commands.
Normal Mac tests include display enumeration and main-thread AppKit cursor reads;
they need desktop access even though disruptive input/capture tests are ignored.
Hosted runners' permission state and synthetic input do not establish real-device
behavior. There is no self-hosted Windows runner.

For a Windows API type-check from Mac, install the target and select crates that
do not require native Windows build dependencies:

```sh
rustup target add x86_64-pc-windows-msvc
cargo check --locked --target x86_64-pc-windows-msvc -p legato-windows -p legato-screen
```

Full app/engine Windows builds belong in the hosted Windows environment. A
cross-check is not a Windows runtime test.

## Current regression coverage

Pure core tests exercise layout geometry, shared-pointer handover/parking, portals,
held input, key mapping, repeat timing, control policies, and Caps Lock. Protocol
messages round-trip through postcard; proptest feeds arbitrary bytes to decoders.
Leaving or disconnecting releases remote keys/buttons and drops their routes;
later physical releases are swallowed rather than sent to a newly active target.
`Enter` carries position/sequence only; Caps Lock has separate synchronization.

Loopback tests use real iroh endpoints for pairing acceptance/listening gates,
rejection of unpaired sessions, input, clipboard/file/video transport, updated
screens, cursor shapes, and failed persistence. They do not run a previous-release
matrix or an injectable loss/reorder transport. Pairing has one-at-a-time gating
and timeouts, not an attempt-rate limiter.

File receive regressions use isolated temporary directories and a real loopback transfer. They check that sanitized names and nested folders remain distinct, simultaneous receives never truncate existing files, batch ids are scoped to their sender, and a truncated transfer removes only its own partial file. These run with the normal engine tests.

Settings regressions exercise actual app actions and the config subscription: an incoming controller policy survives unrelated UI edits, queued notifications preserve newer values and drafts, and failed saves keep committed values. Engine tests cover concurrent edits, stale peer policies, failed publication, and readers observing only complete files during atomic replacement.

Input-routing regressions use the injection thread's peer selector with the real core receiver, without injecting OS events. They cover delayed motion from the previous controller after a handover (including a higher sequence number), held-key/button release, unrelated peer disconnects, local takeover, leave, duplicate packets, and sequence wraparound. Peer pointer reports retain their separate capture/controller path.


### Live physical-display changes

Display enumeration runs once a second off the sharing loop and updates the arrangement
editor even while sharing is stopped. Tests inject resolution, DPI, origin and monitor
count changes; empty/transient enumeration retains the last usable desk. Core tests
preserve a held drag's native peer position when the shared layout moves, clamp the local
return point after unplugging, and cross at the new edge. Loopback tests verify updated
screens reach both an existing session and the next handshake. Legato's Mac virtual
display remains excluded by the platform enumerator.

Hardware check: while sharing, unplug/reconnect a monitor and change its resolution,
scale and placement. Check both arrangement editors, cross each changed edge, repeat
while driving the other machine, and check an open viewer's alignment. Also change a
monitor with sharing stopped, then start sharing. Hosted tests do not simulate OS hotplug.

Native capture also re-pins its hidden cursor when local displays change during a remote
visit. A Windows unit test supplies a moved/shrunken primary monitor, verifies the new
pin effect without making OS calls, and checks that the next hook movement sends only
its four-by-three-pixel delta while preserving the held drag. Repeated unchanged layouts
must not re-pin. The core regression covers preserving the peer position and ending a
visit safely if its configured anchor disappears; the native effect also reinitializes
macOS's first-motion-after-warp handling.

Viewer-placement regressions change the peer's desk position and native client rectangle
without resizing the viewer. The new placement must be sent; identical placements and
macOS nudges must not repeat requests. The arrangement editor's actual canvas handler is
tested dragging out of and back through all four edges with a nonzero canvas origin.

### Confirmed file saves and failed batches

Protocol 11 moves files to bidirectional streams: the receiver acknowledges only after
writing and flushing the destination. Both machines need this protocol version. Loopback
tests prove that delivery alone does not complete a send and that destination creation,
write, flush, missing-acknowledgment and connection failures report failure. Truncated
files are removed without touching existing or successfully received files.

Batch tests cover cancellation during a stalled transfer, disconnect cleanup, late
headers/completions, and a source disappearing after the first file was saved. Incomplete
clipboard batches never become ready to paste; completed files are kept on disk. Sender
cancellation also notifies the receiver when the send future is dropped.

### Folder traversal and links

Transfers resolve links within each selected folder, preserving their relative names.
Nested links outside the selection, ancestor cycles and dangling links fail the scan
before any files are sent. Explicitly selecting a link selects its target. Canonical
manifest paths prevent changing an alias after scanning from redirecting that entry.
This is not a filesystem snapshot: moving/replacing source files can still fail a send.

The same background scan prepares explicit sends, drops and clipboard files, with the
clipboard size limit checked against that manifest. Cross-platform tests create file
and directory links, exercise repeated internal targets and rejected traversal, and
retarget an alias after scanning. A deterministic current-thread-runtime test blocks
the scanner while another async task continues; limit and overflow tests use manifests
without allocating large files.

### Empty directories in transfers

Protocol 12 distinguishes a directory from a zero-byte file. Loopback tests send mixed
folders and entirely empty selections through Send, Drop, and Clipboard, preserving
nested empty directories, empty files, existing destinations, and batch completion.
Reservation tests also cover sanitized directory-name collisions and directory entries
arriving after their children. Both peers must use the updated protocol.

## Opt-in tests

Read each test's prerequisites before running it on a user's desktop. Input tests
can move the cursor, type, change Caps Lock, and interact with foreground windows;
clipboard tests modify real contents. Run disruptive suites serially. Permissions
must belong to the responsible process running this build.

| Command (all start with `cargo test --locked`) | Current CI? | Prerequisites / effects |
| --- | --- | --- |
| `-p legato-macos -- --ignored --test-threads=1` | macOS | Accessibility and Input Monitoring; cursor, keys, Caps Lock |
| `-p legato-windows -- --ignored --test-threads=1 --nocapture` | Windows x64/ARM64 | Interactive desktop; cursor, keyboard, Caps Lock, test windows |
| `-p legato-app gpu_renders -- --ignored --nocapture` | Windows x64/ARM64 | `ICED_TEST_BACKEND=wgpu`, `WGPU_BACKEND=dx12`; WARP rendering/presentation |
| `-p legato-net -- --ignored` | No | Public relays/DNS; Unix silent-drop case starts/freezes helper processes |
| `-p legato-cli --test receive_e2e -- --ignored --test-threads=1` | No | Mac, Accessibility and Input Monitoring; launches real CLI and moves cursor |
| `-p legato-cli --test clipboard_e2e -- --ignored --test-threads=1` | No | Mac; launches CLI, writes clipboard and restores original text |
| `-p legato-screen --test mac_display -- --ignored --test-threads=1` | No | Mac; creates/removes a virtual display |
| `-p legato-screen --test mac_display_dpi -- --ignored --test-threads=1` | No | Mac/AppKit; changes display modes and arrangement, reopens displays |
| `-p legato-screen --test mac_stream -- --ignored --test-threads=1` | No | Mac Screen Recording; creates display and captures/encodes it |
| `-p legato-screen --release --test mac_encoder -- --ignored --nocapture` | No | Mac hardware encoder/scaling benchmarks |
| `-p legato-screen --release --test win_decode hardware_decodes_the_mac_stream_without_fallback -- --ignored --nocapture` | No | Physical Windows GPU; requires hardware decode without fallback |

### Connecting from anywhere

The ignored network suite disables mDNS and uses n0's public relays and DNS:

- Paired peers restart without cached addresses and find each other.
- A peer closes and restarts at a new address while the other keeps running.
- On Unix, a helper process is frozen to model a silently dead old connection;
  a new process with the same identity must reconnect. `helper_node` is an ignored
  subprocess entry point, not a separate connectivity scenario.

These models do not replace a real Wi-Fi/Ethernet or hotspot switch. Save
`legato-app.log` connection path, disconnect reason, and relay-reachability lines.

## UI, display and platform coverage

### UI snapshots

Transfer-error regressions check persistent feedback across all pages, individual dismissal,
repeated failures, immediate send errors, and scrolling multiple failures without hiding
the Arrangement canvas. Unpair tests cover the secondary menu, confirmation and cancellation
(including Escape), stale confirmation, failed removal, and preserving other pairings.
Settings rows and setup/offline guidance are rendered at compact widths in light and dark
themes; display options retain visible actions while scrolling and accurate stream-limit text.

The app's UI tests drive the real views headlessly with `iced_test` and compare each screen, pixel for pixel, with a golden image in `crates/legato-app/src/snapshots/`. These screens are covered: devices, the expanded navigation rail, the "Add a device" dialog, the display options (also at the main window's default size, where they must fit without scrolling), pairing dialog, arrangement, settings, and the display viewer while it waits. They're drawn with the app's own theme (`app_theme`), which uses iced-m3's Expressive springs and shape feedback. The goldens show each screen at rest.

Compact-layout regressions also render the supported 640×480 minimum, long device names, an expanded-rail preference at small and medium widths, and six paired devices. They check that device actions stay readable and clickable, controller selection preserves saved ID prefixes and works by keyboard beyond the visible menu items, and the arrangement canvas and unplaced-device instructions fit without overlapping. Selected cases use the dark theme; canvas-label tests cover Unicode names and displays too small to fit text.
Navigation snapshots use official Rounded Material Symbols, with filled artwork for
the selected destination. Check the native screen-sharing tray symbol separately on
macOS and Windows; it is rasterized from the same embedded catalog rather than drawn
by the UI snapshot renderer.
- They're rendered with tiny-skia on the CPU. `.cargo/config.toml` sets `ICED_TEST_BACKEND=tiny-skia` so local runs match CI.
- Device ids in the sample data are fixed, so nothing on screen changes between runs.
- On a mismatch, the test writes the new rendering and a diff (changed pixels in red) to `target/snapshots/`. CI uploads them as an artifact.
- After a deliberate UI change, run `LEGATO_UPDATE_SNAPSHOTS=1 cargo test -p legato-app` and review the new images in the pull request.
- The viewer's video picture is drawn by a GPU shader, which tiny-skia can't draw, so it isn't in a golden.

### Virtual-display cursor shapes

Protocol 13 carries cursor shapes separately from video and motion. Update both machines
for this feature. Normal workspace tests cover reliable delivery with no video/motion,
viewer generation/peer checks, close/reopen reset, unknown/nil-image fallback, stationary
UI notifications, letterboxing and leaving/re-entering the picture. A paused-clock test
checks change-only publication, the 250 ms fallback for a stalled AppKit read, recovery,
and the one-pending-read limit.

`cargo test --locked -p legato-screen --test mac_cursor_shapes` runs AppKit on the
process main thread. It matches separately allocated copies of real macOS built-in
cursor images (including each edge/corner and one-sided frame resize), and checks
missing, empty, oversized and unfamiliar images. It never sets/moves the cursor or
creates a display, and requests no permissions. Desktop access is needed for AppKit.

On the real Mac/Windows pair, open the extra display and use the **Windows mouse** to
hover over Mac text, links, and all four edges/corners of a normal window. Test both
windowed and full-screen viewers, then use the **Mac trackpad** on the same targets.
Keep the pointer stationary while the underlying UI changes its cursor. Check that
letterboxing and Windows window borders keep their normal behavior, and that leaving,
closing/reopening, disconnecting/reconnecting, resizing and switching quality do not
leave a resize/hand cursor behind. Check normal typing, clicks, scrolling and pointer
handover too. These real interactions remain a manual hardware check.

The Mac reader samples at 20 Hz only while streaming. It compares bounded normalized
images and hotspots against this OS's standard cursors. Custom, animated or unavailable
images fall back to an arrow. `NSCursor.currentSystemCursor` is deprecated; the
October 13, 2026 probe on macOS 26.6.2 observed all 12 test-page shapes on the Legato
display without nil readings, but does not guarantee future OS support or end-to-end
Windows behavior. The shipping Windows backend uses stock cursors: text, pointing hand,
resizes, crosshair and prohibited have direct equivalents; grab/grabbing use the stock
four-way move cursor, and unsupported icons such as zoom/copy/link use an arrow.

### Windows resize presentation

Legato uses the published `wgpu-hal`, with DX12 Mailbox presentation by default
when a DX12 adapter is available. The scaling patch in [PR #62](https://github.com/tvolk131/legato/pull/62)
was removed because its benefit for the reported main-window content wobble was
never verified on the PC. Neither that patch nor this rollback is a verified fix.

`cargo test --locked -p legato-app --test windows_presentation -- --ignored --nocapture`
uses DX12 WARP and a real HWND to exercise creation, growth, shrinkage, and surface
recreation with both Mailbox and FIFO presentation. It checks the acquired frame's
dimensions and submits/presents a frame at each size, using Iced's frame-latency
setting. Both Windows CI architectures run it in the existing `gpu_renders` step.
This is a rendering smoke test; it does not reproduce visible resize wobble.

To compare presentation timing without rebuilding, quit Legato from its tray menu
before each launch. In PowerShell in the folder containing `Legato.exe`, run:

```powershell
$env:WGPU_BACKEND = 'dx12'; $env:ICED_PRESENT_MODE = 'fifo'; .\Legato.exe
```

Repeat with `mailbox` for the baseline, keeping the binary, window size, screen,
and scaling the same. Setting both variables holds the graphics backend constant;
Legato respects either override instead of applying its defaults. These variables
affect this shell and programs launched from it, not persistent Windows settings.
Quit Legato and close that PowerShell window to return to normal launches.

On the PC, drag the main window's right and bottom edges repeatedly on Devices,
Arrangement, and Settings. Content near the top-left should remain steady except
for intended layout changes at width breakpoints. Also check left/top resizing,
maximize/restore, mixed monitor scaling, and resizing the Mac-display viewer.
Record whether stationary content shifts, stretches, or only reflows at a
breakpoint, along with the app version and GPU. Compare the viewer's F10 timings
under sustained motion too: Iced applies the presentation mode to all windows,
and FIFO may add latency. Do not change the default based on main-window appearance
alone.

Other options have limits in Iced 0.14: `ICED_PRESENT_MODE=immediate` is a diagnostic
only (it may tear and requires adapter support), and `ICED_BACKEND=tiny-skia` can
isolate GPU presentation on the main window but cannot draw the viewer's video
shader. `WGPU_DX12_PRESENTATION_SYSTEM=visual` is not a working override here:
Iced constructs default backend options without reading that environment variable.
Switching to DirectComposition or setting DXGI scaling therefore needs renderer
integration changes; neither is a demonstrated remedy for this report. Start with
the [supported presentation-mode override](https://docs.rs/iced_wgpu/0.14.0/src/iced_wgpu/settings.rs.html)
and the PC comparison before adding platform-specific code.

### Virtual monitor mode

The Mac encodes and Windows decodes, so the two ends are tested against each other through a committed fixture:
- **Encoder (macOS, every PR):** VideoToolbox encodes a known test pattern (`legato_screen::test_pattern`). The test checks the stream starts with SPS, PPS and an IDR slice and that later frames are P-slices.
- **Decoder (Windows, every PR):** Media Foundation decodes `crates/legato-screen/tests/fixtures/pattern.frames`, which the encoder test wrote, and checks the colours of every frame. It also checks that low-latency mode gives one picture per frame. Regenerate the fixture on a Mac with `LEGATO_WRITE_FIXTURES=1 cargo test -p legato-screen --test mac_encoder`.
- **Decoder fallback (Windows, every PR):** run the fixture through automatic selection and forced software, then reset a used decoder after a simulated failure and decode from a fresh keyframe. Repeated recovery stays in software. Hosted runners do not verify physical GPU decoding.
- **D3D11 readback (Windows, every PR):** WARP textures exercise Media Foundation's DXGI buffers, array-slice selection, staging reuse and resize, and visible pictures shorter than their textures. Pure buffer checks cover row padding, coded-height padding, invalid dimensions, and truncated data.
- **Hardware decoder (Windows, ignored):** `cargo test -p legato-screen --release --test win_decode hardware_decodes_the_mac_stream_without_fallback -- --ignored --nocapture` requires the GPU startup check and color fixture to pass without fallback. For the actual 4K performance check, use Sharpest with sustained scrolling/window motion for a minute. Confirm `H.264 decoding: D3D11 hardware on ... (NV12 readback)` in the Windows log, compare F10 decode/total timings against alpha.26's roughly 13–15 ms decode, and check colors, text, adaptive quality, resize, close/reopen, and sleep/wake. Readback is included in decode time. A fallback reason in the log is an actionable result, not a hardware pass.
- **Windows decode timing (alpha.29):** pure tests validate the five-second aggregation, per-input averages, independent maxima, counts, window reset, and separation after resolution/backend changes. Existing software-fixture and WARP readback checks cover the instrumented paths. Run the same candidate build on both machines (their protocol versions must match), and open 4K **Sharpest** for about a minute with sustained scrolling/window motion, followed by a still interval. Save the backend line, several `H.264 decode timing` summaries, and an F10 screenshot. `input_ms` includes sample preparation and `ProcessInput`; `output_ms` times `ProcessOutput`; `gpu_wait_ms` spans staging-copy submission through blocking `Map`; `cpu_copy_ms` includes NV12 allocation and row copying. Values are averages per successful input, and `worst_*` values are independent maxima. `other_ms` is the remaining decoder overhead. Separate decoder ids/resolutions keep adaptive tracks distinguishable. GPU wait can include queued decoding, so do not call it pure transfer time. No extra GPU flush, synchronization query, or pipeline change is introduced. No physical RTX 3070 per-stage timing results are recorded; collect them on the current candidate.
- **Virtual display (macOS, ignored):** the display appears with the requested point and pixel size and Legato's vendor and product ids. It switches to 144 Hz and to other sizes while shown (in about 0.2–0.3 s), and goes away when dropped. A process without an AppKit application can only show one virtual display in its lifetime (CoreGraphics stops reporting modes for later ones in that process), so each display test is in its own test binary.
- **Virtual-display mode changes (macOS):** the normal suite checks mode matching against ambiguous 1080p, unscaled 4K, Retina 4K, and wrong-refresh descriptions. `cargo test -p legato-screen --test mac_display_dpi -- --ignored --nocapture` runs AppKit on the main thread while a worker creates and arranges displays, alternates 1080p/144 Hz and Retina 4K/60 Hz, changes refresh, and recovers from forced low-DPI and unscaled modes. It covers reopening both within one process and across fresh processes. AppKit matters: the CLI-only version reproduced a stale mode error, but the old implementation also recovers when AppKit processes notifications. This test validates supported transitions, not a reproduction of the user's persistent app failure. The user confirmed recovery on alpha.28 after installation and permission restoration, but the cause remains unproven. Recheck repeated resolution-option changes and reopening from the Windows viewer on the current candidate.
- **Encode latency (macOS, ignored benchmark):** `cargo test -p legato-screen --release --test mac_encoder -- --ignored --nocapture`. On Apple silicon, each frame takes about 16 ms at 4K and 8 ms at 1440p when forced out right away. Left to itself, VideoToolbox keeps about five frames in flight (about 80 ms at 60 fps), so the stream waits for each frame.
- **Scaling (macOS, every PR):** a picture scaled down on the GPU for adaptive quality keeps its colours and encodes. `scaling_latency` (ignored benchmark) times 4K scaled to 1080p and 1440p: about 3 ms either way.
- **Adaptive quality (pure):** small changes stay on the sharp track, and so does a single big one; sustained movement switches to the moving track at once and back after 150 ms of stillness, with one touch-up later. The sharp track keeps to its frame rate without losing the last picture, keyframes go to the track asked for, and a picture held up by the network is sent once it clears. The viewer shows the newest picture of either track and merges both tracks' stats.
- **Viewer lifetime (normal engine tests):** a worker paused before publication resumes after close, same-peer reopen, or switching peers. Its frames, stats, keyframe/stop requests, and final cleanup must not affect the new viewer. Closing cancels idle readers, wakes waiting decoders, and discards queued frames. Track guards clean up on early initialization return and when a spawn closure is dropped without running; stats remain until the last active track ends. These tests use the real publication and cancellation code without a GPU. Rapid close/reopen and peer switching on the Windows PC remain the hardware smoke check.
- **Newest picture only (macOS, every PR):** after completing a warm-up encode, a thread stands in for capture, handing the real encoder pipeline (VideoToolbox) a picture every 2 ms, each stamped as appearing when it was due. Pictures the encoder hasn't reached are replaced, so some are skipped, and frames stay about as fresh at the end as at the start: they never queue. On an M4 Pro at 1080p, 400 pictures gave 67 frames, with 333 skipped and a delay of about 13–16 ms throughout. Cold VideoToolbox startup is excluded from the steady-state comparison; it can consume most of the 800 ms measurement window on hosted Macs.
- **Capture (macOS, ignored; needs Screen Recording):** a virtual display streams a keyframe first on each track it uses, both tracks share one clock, and a keyframe comes on request even while nothing on it changes, with adaptive quality and without.
- **Portal keyboard (Windows, every PR):** a stand-in viewer window on its own thread, focused or not and with its thread idle or busy. Clicks and keys over its picture reach the Mac: after moving, with no motion since the viewer appeared, and with no motion since the Mac took its cursor back. The test also opens Start with the Windows key and closes it three ways: Escape, a click outside it, and a click on the taskbar. After each, moving onto the picture and typing must reach the Mac. Cases are skipped where something else covers the window, such as the sign-in prompt on the Windows 11 ARM runner. The log shows what `WindowFromPoint` found at each step.
- **The extra display's edges (pure core):** with the extra display full screen on the PC's middle monitor, pushing off its left edge goes onto the PC's left monitor at the same height. Its top edge leads nowhere, since nothing is above the monitor. At its bottom edge the MacBook is below, so macOS moves the cursor there itself. With no picture known, no edge leads anywhere. In a window, its edges lead onto the monitor around the window. Coming back onto the picture brings the pointer home to the same spot on the extra display, and tells the PC. Dragging something on the PC across the picture keeps dragging it on the PC.
- **Paced pointer reports (pure core; Windows, every PR):** reports to the machine this one took over from go out about every 16 ms, however fast the mouse sends moves. The latest follows once moves stop. It isn't sent if the pointer has since changed hands, and reports for a shown display aren't paced. On Windows, 50 moves sent at once give a handful of reports, and the last (sent by the capture's timer) is where the pointer stopped.
- **Coming home and stale reports (pure core):** coming home from the Mac reports the pointer to the Mac right away, and whenever this machine's mouse moves it, until the Mac comes across again. So do leaving the portal's picture and moving the pointer after showing the Mac's pointer on it. A report the Mac sent before its `Enter` is dropped.
- **The Mac's extra display (pure core):** the Mac's cursor on its extra display, which isn't one of its shared screens, is never at an edge. Trackpad motion there doesn't hand the Mac's input to the PC. Pushing up from the MacBook's own top edge still crosses.
- **Keys with a Legato window in front (Windows, every PR):** a focused window registered for raw keyboard input the way winit registers every app, and a real winit window. With the pointer on their picture, keys reach the Mac, including when the raw-input registration comes after sharing started. Without the capture removing that registration, 0 of 6 keys got through.
- **Which window gets painted (Windows, every PR):** two windows on one thread, both marked for painting before each turn, with one `WM_PAINT` handled per turn as winit does. The window in front got every paint (300 to 0), which is how the viewer froze while the main window was in front. A window asking alone is always painted.
- **The PC's cursor when the Mac takes over (Windows, every PR):** the cursor shows where the Mac puts it, whether the PC was idle, driving the Mac, or parked, whichever order the Mac's messages come in. Coming out of parking leaves the cursor where the Mac put it. A late hand-back leaves it where the Mac put it. This checks Windows' own cursor state (showing, suppressed, image). The report of the cursor vanishing on real hardware isn't reproduced by this; the PC logs its cursor state after each takeover.
- **Parking (Windows, every PR; pure core):** a yield leaves the PC's cursor parked and hidden rather than bringing it back. Reports of the Mac's pointer on the MacBook's screen move the parked position, and the PC's next motion carries on from there. The core tests also cover a key or click taking the pointer back first, the Mac's pointer coming across unparking, losing the Mac bringing the cursor home, a machine reporting its own pointer to the one it took over from, and a report of the Mac's pointer on its own screen parking the PC.
- **The PC's cursor for the Mac's own pointer (Windows, every PR):** reports of the Mac's pointer on its extra display put the PC's cursor at the same spot on the picture, ignoring older reports, without handing the PC's input to the Mac, and not while the PC is driving the Mac. Parked, the Mac's pointer moving onto its extra display unparks and shows the PC's cursor there. Moving back onto the MacBook parks again, and the PC's mouse carries on from there. The Mac's side is a pure core test: its own motion on the shown display is reported, once more as it leaves, and not elsewhere.
- **Starting again after a crash (app, every PR, both platforms):** runs the real app binary, which crashes on its main thread when `LEGATO_TEST_CRASH` is set (debug builds only). The checks:
  - the first run's log is kept as `legato-app.previous.log` and says it's starting again;
  - the copy it starts waits for it to exit, then crashes the same way;
  - within a minute of the first crash, that copy doesn't start a third.
  The GPU error handler (log instead of crash) can't be provoked in a test: it needs a failing GPU surface.
- **Shared Caps Lock (pure core; macOS and Windows, every PR):** the core sync covers several cases:
  - a change is told to peers and set there, without echoing back;
  - on connecting, both settle on one state;
  - crossing changes end the same on both;
  - a peer's change that takes a moment to show isn't sent back;
  - the Caps Lock key toggles the machine it's pressed on and isn't forwarded, unless it's remapped.
  The hardware tests set Caps Lock and check that a reader on another thread sees it change and change back. On Windows that reader is a long-lived thread that handles no input, like the engine's.
- **Keys typed on the Mac (macOS, every PR):** a listen-only tap records the keys the injector posts. ← carries the function-key and keypad marks a Mac keyboard puts on it, F13 the function-key mark, keypad 1 the keypad mark, and A neither. Control-← carries Control and the function-key mark, which macOS's "Move left a space" needs. The log shows whether it reached the session or macOS took it as its shortcut first, next to a Control-only one posted for comparison. The CI Mac has one space, so the switch itself is only visible on real hardware.
- **Crossing from the Mac (macOS, every PR):** the Mac's trackpad crosses to a peer and keeps driving it. macOS reports a warp's jump in the next event's delta (rdar://11757097), so the test posts events the way the hardware reports them, with the spike in the delta. Neither the warp to the park point nor the warp back may bounce the pointer (both did, every other event, until 0.3.0-alpha.15). After the first event, each moves the peer's cursor by its own motion, however far the hidden cursor has drifted. A trackpad moves it though its events are dropped, and alpha.15 summed the drift, so the pointer moved like a pointing stick. Synthetic events move the cursor even when the tap drops them, so the held cursor itself can't be checked here.
- **Portal (pure core):** motion over the picture places the Mac cursor absolutely, and input over it goes to the Mac. Leaving (without a button held), closing, yielding and losing the Mac each end it. Dragging off the picture continues on the MacBook's screen, moving onto the extra display from the MacBook enters the portal, and pushing past a full-screen portal's edge towards the Mac crosses over, all without telling the Mac the cursor left.
- **Dragging against the viewer's top edge:** pure core tests keep a drag on the extra display when no Mac screen touches the exit point, including the parts of its bottom edge wider than the MacBook. Valid rightward and downward crossings preserve the drag, as does coming back. A Windows desktop test pushes upward repeatedly against a full-screen viewer while holding a button and checks that the pointer and forwarded motion stay at the top and the Mac receives the release. The reported Mission Control full-screen-app drag still needs confirmation on the user's Mac and PC; the synthetic Windows test does not exercise Mission Control.
- **Placement and budgets (pure core):** frame-rate limits by size; the extra display goes above the MacBook for the monitor above it, beside it for a window beside it, and on the dominant side for a diagonal monitor.
- **Video backpressure (macOS, no capture permissions):** a stalled write applies backpressure before polling, retains at most three queued frames per track, counts dropped frames, and rejects dependent pictures until a keyframe fits. Tracks recover independently; a failed write and a closed receiver cannot block capture. On real hardware, pause the network during motion, stop moving, and reconnect: confirm bounded memory and recovery of the final still picture in both Sharpest and Adaptive modes.
- **Video stream (in-process network):** frames arrive intact and in order on their track, and the stream ends cleanly.

Lab checklist:
- text is sharp at 100% zoom in full screen on a 4K monitor
- the Windows pointer lines up with where the Mac clicks, including at the picture's edges and in letterboxed windows
- latency feels close to a local display when dragging windows on it
- with adaptive quality: dragging and scrolling switch to the smaller picture (the stats say "while moving"), typing and a single tab switch stay sharp, and the picture sharpens a moment after things stop
- moving windows onto the display from the Mac's own screen, and back
- rearranging displays in System Settings while it's shown
- closing the viewer, stopping from the Mac, quitting either app, and pulling the network cable all remove the display
- the Screen Recording prompt on first use, and relaunch after granting it

---

## Manual two-machine lab

Use the real MacBook and triple-monitor Windows PC, with the same candidate on
both. A Windows VM helps iteration but cannot verify physical pointing devices or
GPU decoding; disable its mouse integration when testing capture.

The broader manual matrix below is a coverage goal, not a record of completed runs.
The focused [0.3.0 hardware gates](docs/releases/0.3.0-plan.md#hardware-gates-before-final)
are the release checklist.

**Matrix:**

| Dimension | Values |
|---|---|
| Direction | Mac→Windows, Windows→Mac |
| OS versions | macOS 26 and 27 (Apple silicon only); Windows 11 current and current−1 feature update, on x64 and ARM64 |
| Keyboard layouts | US, UK, DE, JIS, and a Mac keyboard on Windows and vice versa |
| Pointing devices | Magic Trackpad, Magic Mouse, precision touchpad, 1 kHz mouse |
| Displays | Single, dual, mixed DPI, arranged left/right/above |
| Network | Same Wi-Fi, Ethernet↔Wi-Fi, different networks (relay then direct), VPN on one side |

**Layout typing test:** type every printable key on each layout pair, with and without Shift and Option/AltGr, and compare the typed characters against an expected table. A cross-machine automated harness is not implemented.

**Mac vs Windows differences to test explicitly:**
- coordinates: points vs physical pixels, and speed parity across DPI
- natural-scrolling direction
- default Alt→Command and Win→Option remapping when driving Mac, and configured alternatives
- double-click timing
- text clipboard line endings (`\n` vs `\r\n`)
- HTML clipboard support is not implemented; test only text, images and file lists
- clipboard images (TIFF/PNG vs DIB)
- **file names in transfers:**
  - `:` is legal on Mac but not on Windows
  - Windows reserved names (`CON`, `NUL`…)
  - Windows is case-insensitive, so name collisions are possible
  - trailing dots and spaces
  - path length limits
  - Unicode normalisation (NFC vs NFD)

**Exploratory checklist** (run before each release):
- [ ] Test a focused macOS password field (Secure Input). Automatic switching prevention and a warning are not implemented; record actual behavior and loss of input as a validation gap.
- [ ] A UAC prompt appears while controlling the Windows machine
- [ ] Typing into an elevated window (Task Manager): expected failure mode and message
- [ ] Sleep and wake on each side while connected
- [ ] Lock screen on each side
- [ ] Switch Wi-Fi to Ethernet, and change networks, while connected
- [ ] Unplug or replug a monitor while controlling the other machine
- [ ] Kill the app while controlling the other machine: the cursor returns, no keys are stuck on either side
- [ ] Bounce rapidly across the edge; switch while a button is held
- [ ] A Windows PC with no physical mouse attached (cursor visibility)
- [ ] Copy a large image or file on the clipboard while moving the mouse (input still has priority)

---

## Future testing ideas (not implemented)

The following are proposals from the original testing strategy, not current CI or
application capabilities:

- Shared OS-backend conformance suite for injection/capture/scroll/drag/repeat.
  Current native suites test selected cases separately. Modifier state on `Enter`
  and HTML clipboard transport would first require implementation changes.
- Injectable clock, platform traits and a transport that simulates datagram loss,
  duplication, reordering, jitter and stalled streams.
- Recorded physical-device event streams, a `--record` flag, and replay fixtures.
- `cargo-fuzz` targets, nightly fuzzing/soaks, and previous-release compatibility
  fixtures/matrix. Current decoder proptests are not a fuzzing job.
- A debug cross-machine harness on a separate ALPN (for example `legato/test/1`)
  that reports cursor, text, clipboard, displays and held keys. No such mode ships.
- Input latency acknowledgments with inject timestamps, p50/p99 alerts, and a
  diagnostics panel for loss, hook/tap failures and Secure Input. Current viewer
  F10 timings and logs are separate diagnostics.

Manual overnight use can still look for memory growth, reconnect failures,
disabled hooks/taps and stuck input. A high-frame-rate camera can measure visible
end-to-end latency without implying an input-ack protocol exists.

## Packaging and permissions on clean machines

Test downloaded packages, not just development builds. Follow the
[signing guide](docs/macos-signing.md) and
[release plan](docs/releases/0.3.0-plan.md) for signed-install/upgrade gates.

- On a fresh Mac user or VM, test Accessibility, Input Monitoring, Screen Recording,
  Local Network access, menu-bar-only behavior, launch at login and an upgrade
  between two builds with the same signing team/bundle ID. Distinguish the first
  ad-hoc-to-Developer-ID transition from signed-upgrade retention.
- On Windows, test the extracted ZIP's app/CLI, firewall behavior on the intended
  network profile, launch at login, and cleanup after quitting. There is no
  installer or automatic firewall/uninstall workflow; Windows code signing is not
  part of the current release workflow.

## References

- [enigo integration workflow](https://github.com/enigo-rs/enigo/blob/main/.github/workflows/integration.yml)
- [Hosted Mac TCC measurements](https://github.com/Integration-Automation/AutoControlGUI/pull/482)
- [Runner Accessibility discussion](https://github.com/actions/runner-images/issues/1567)
- [Tart macOS VMs](https://github.com/cirruslabs/tart)

### Native SwiftUI candidate

The opt-in macOS front end and screenshot suite are documented in
[native/macos/README.md](native/macos/README.md). It shares the production engine;
its real two-machine parity and signed-upgrade gates remain pending. Native
screenshots use Swift Testing with offscreen AppKit hosting and do not start the
engine. Hosted CI renders fixtures and checks image content/size; strict pixel
baselines are compared on the recorded OS appearance.
