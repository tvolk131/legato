# Testing strategy

Target platforms are **macOS and Windows**, in both directions (Mac→Windows and Windows→Mac).

The approach:
- Make most behaviour testable without a real OS.
- Test the thin OS layers against one shared contract.
- Keep a small two-machine lab for what only real hardware shows.

| Layer | What it catches | Runs | Where |
|---|---|---|---|
| 1. Pure core | Logic, state-machine, geometry, protocol and keymap bugs | Every PR | Any OS (fast) |
| 2. In-process network | End-to-end event flow, loss/reorder, disconnects, version skew | Every PR | Any OS |
| 3. OS conformance | Regressions in each OS backend (injection, capture, swallowing) | Every PR | GitHub-hosted `macos-latest` + `windows-latest` |
| 4. Cross-OS lab | Mac↔Windows interop, keyboard layouts, devices, displays, feel | Per milestone + pre-release | Real MacBook + triple-monitor Windows PC (manual, and test-harness mode) |

---

## 0. Design for testability (prerequisite)

- **Thin OS layers behind traits:** `Capture`, `Emulate`, `Clipboard`, `Displays`. They exchange platform-neutral types:
  - keys as USB HID usage codes
  - positions in the receiving machine's coordinates, with an explicit scale factor
  - scroll in units of 120 plus a pixel variant
  - buttons as an enum
- **A pure, deterministic core** that never touches the OS. It holds:
  - the edge and switch state machine, and the virtual cursor
  - layout geometry and DPI scaling
  - held-key and held-button tracking, release-all
  - modifier remapping (Cmd↔Ctrl)
  - key-repeat policy
  - clipboard sync and echo suppression
- **An injectable clock** (`trait Clock`), so dwell times, heartbeat timeouts and repeat timers can be tested without sleeping.
- **Tag every injected event:** `kCGEventSourceUserData` on macOS, `dwExtraInfo` on Windows. Capture uses the tag to ignore its own injected events, and tests can turn that filter off so a test can inject and then capture.
- **An injectable transport:** the session talks to a `Transport` trait. Production uses iroh; tests use an in-memory implementation with fault injection.

---

## 1. Pure-core tests

**Property tests (`proptest`)** over random sequences of input events, edge crossings, disconnects and timeouts. These invariants must always hold:
- After `Leave`, disconnect or heartbeat timeout, the other machine holds **zero** keys or buttons.
- Every key-up and button-up goes to the **same machine** as its matching down, even if a switch happened in between.
- The virtual cursor always stays inside the other machine's screen rectangles.
- Locally, events are either swallowed or passed through, never both, and never forwarded while the mode is `Local`.
- OS auto-repeat events are never forwarded. Only the receiving side repeats.

**Geometry fixtures.** Save `display-info` dumps from real setups as JSON fixtures:
- a laptop alone
- a laptop plus an external monitor at mixed DPI
- stacked monitors
- offset monitors with gaps
- a rotated monitor
- Windows at 150% scaling next to a Retina Mac

Table-driven tests on these fixtures check:
- which edges are outer edges versus boundaries between local monitors
- how an entry point maps onto the other machine's screen
- speed scaling across DPI
- clamping inside gaps between monitors

**Keymap table.**
- Every macOS virtual keycode and Windows set-1 scancode (including E0-extended keys) must round-trip through the HID code.
- Keys that exist on only one OS (Fn, Menu/App, Insert, PrtSc, ISO/JIS extras) go in an explicit, reviewed allowlist.
- Snapshot the whole table with `insta` so any change is visible in review.
- Test the Cmd↔Ctrl and Option↔Alt remap tables separately.

**Wire protocol.**
- Round-trip every message type with serde/postcard.
- Fuzz the decoder with `cargo-fuzz`: every frame decoder and the datagram parser. They parse input from the network.
- Version compatibility: commit the encoded bytes of each message type for every release to `tests/fixtures/proto/vN/`. Version N must decode version N−1 fixtures, or reject them with a clear version error. Never let an old fixture crash the decoder.

**Record and replay.**
- Add a debug `--record` flag that writes the raw captured event stream, with timestamps, to a file.
- Record real devices: Magic Trackpad (continuous scroll plus momentum), Magic Mouse, a Windows precision touchpad, a 1 kHz gaming mouse, and fast typing including chords.
- Replay those recordings through the core and snapshot what goes out on the wire. That's how you catch "scrolling feels wrong" bugs.

---

## 2. In-process network tests

- Run two real iroh endpoints in one test, as in `spikes/iroh-probe`: a scripted fake `Capture` on one side and a recording fake `Emulate` on the other. Assert that the emulator received the exact expected sequence.
- **Fault injection** through an in-memory `Transport`:
  - drop, duplicate, reorder and delay datagrams
  - add jitter
  - stall the control stream
  - Assert that motion stays correct because newer positions supersede older ones, and that keys and buttons are never lost.
- **Disconnect cases:**
  - Kill the connection while a key is held: the other machine releases it.
  - Hit the heartbeat timeout: the controlling machine returns to `Local`.
  - Reconnect: the session resumes cleanly, with no duplicated `Enter`.
- **Version skew:** CI builds the previous release tag and runs the suite with the controlling machine at N and the other at N−1, and the reverse. This covers ALPN negotiation and graceful refusal.
- **Pairing:**
  - Both sides derive the same 6-digit code.
  - An unpaired `EndpointId` is rejected on the session ALPN.
  - The pairing ALPN is rate-limited.
  - A revoked peer is rejected on its next connection.

---

## 3. OS conformance suite (CI on hosted runners)

GitHub-hosted `macos-latest` and `windows-latest` runners can inject input:
- enigo runs its input integration tests on them.
- Someone measured that `macos-14` runners grant Accessibility, and that `CGEventPost` moves the cursor there.

Write **one conformance suite, run against each backend**. Every OS backend must pass the same tests:
- Inject key down/up for every allowlisted key; capture sees the same HID code and direction.
- Inject an absolute move to (x, y); the cursor ends up within 1 px, per display.
- A scroll of +120 produces one notch, and continuous pixel scroll arrives as pixels.
- Button down, move, button up produces a drag (macOS: the `*MouseDragged` event types); two quick clicks produce a double-click.
- While capture is in `Remote` mode, an injected event does **not** reach a focused test window.
- A held key repeats at the local OS rate, from the receiving side's own repeat timer.
- Modifier sync: after `Enter` with Shift set, typing `a` gives `A`.
- Clipboard round-trips text (including CRLF/LF), HTML, a PNG and a file list.

Setup notes:
- Verify with a tiny iced/winit test window that logs key codes, characters, mouse positions and scroll deltas.
- Run with `--test-threads=1`, because all tests share the global cursor.
- Gate the suite behind a `desktop-tests` feature (or `#[ignore]`) so plain `cargo test` stays headless.

Limits: runners are single-display VMs with no real input devices, and their permission state (TCC) differs from users' machines. This is a regression net, not proof the app feels right.

### UI snapshots

The app's UI tests drive the real views headlessly with `iced_test` and compare each screen, pixel for pixel, with a golden image in `crates/legato-app/src/snapshots/`. These screens are covered: devices, pairing dialog, arrangement, settings, and the display viewer while it waits.
- They're rendered with tiny-skia on the CPU. `.cargo/config.toml` sets `ICED_TEST_BACKEND=tiny-skia` so local runs match CI.
- Device ids in the sample data are fixed, so nothing on screen changes between runs.
- On a mismatch, the test writes the new rendering and a diff (changed pixels in red) to `target/snapshots/`. CI uploads them as an artifact.
- After a deliberate UI change, run `LEGATO_UPDATE_SNAPSHOTS=1 cargo test -p legato-app` and review the new images in the pull request.
- The viewer's video picture is drawn by a GPU shader, which tiny-skia can't draw, so it isn't in a golden.

### Virtual monitor mode

The Mac encodes and Windows decodes, so the two ends are tested against each other through a committed fixture:
- **Encoder (macOS, every PR):** VideoToolbox encodes a known test pattern (`legato_screen::test_pattern`). The test checks the stream starts with SPS, PPS and an IDR slice and that later frames are P-slices.
- **Decoder (Windows, every PR):** Media Foundation decodes `crates/legato-screen/tests/fixtures/pattern.frames`, which the encoder test wrote, and checks the colours of every frame. It also checks that low-latency mode gives one picture per frame. Regenerate the fixture on a Mac with `LEGATO_WRITE_FIXTURES=1 cargo test -p legato-screen --test mac_hardware encoder`.
- **Virtual display (macOS, ignored):** the display appears with the requested point and pixel size and Legato's vendor and product ids, and goes away when dropped.
- **Capture (macOS, ignored; needs Screen Recording):** a virtual display streams a keyframe first, and a keyframe on request even while nothing on it changes.
- **Portal (pure core):** motion over the picture places the Mac cursor absolutely, input over it goes to the Mac, and leaving, closing, yielding and losing the Mac each end it.
- **Video stream (in-process network):** frames arrive intact and in order, and the stream ends cleanly.

Lab checklist:
- text is sharp at 100% zoom in full screen on a 4K monitor
- the Windows pointer lines up with where the Mac clicks, including at the picture's edges and in letterboxed windows
- latency feels close to a local display when dragging windows on it
- moving windows onto the display from the Mac's own screen, and back
- rearranging displays in System Settings while it's shown
- closing the viewer, stopping from the Mac, quitting either app, and pulling the network cable all remove the display
- the Screen Recording prompt on first use, and relaunch after granting it

---

## 4. Cross-OS lab

**Hardware:**
- The real MacBook and the triple-monitor Windows PC. There's deliberately **no self-hosted CI runner**. Windows builds come from hosted CI artifacts and are run by hand; later, the test-harness mode below reports results back over iroh.
- A Windows VM on the Mac is fine for the day-to-day loop. Turn off the hypervisor's mouse integration, because it fights capture.

**Test-harness mode** (debug builds only, on a separate ALPN such as `legato/test/1`). It reports:
- cursor position and current screen
- text typed into the harness window
- clipboard contents
- display configuration
- held keys
- diagnostics counters

A driver on the controlling machine runs scripted scenarios in both directions and asserts on the state the other machine reports.

**Matrix:**

| Dimension | Values |
|---|---|
| Direction | Mac→Windows, Windows→Mac |
| OS versions | macOS 26 and 27 (Apple silicon only); Windows 11 current and current−1 feature update, on x64 and ARM64 |
| Keyboard layouts | US, UK, DE, JIS, and a Mac keyboard on Windows and vice versa |
| Pointing devices | Magic Trackpad, Magic Mouse, precision touchpad, 1 kHz mouse |
| Displays | Single, dual, mixed DPI, arranged left/right/above |
| Network | Same Wi-Fi, Ethernet↔Wi-Fi, different networks (relay then direct), VPN on one side |

**Layout typing test:** type every printable key on each layout pair, with and without Shift and Option/AltGr, and compare the harness's recorded characters against an expected table.

**Mac vs Windows differences to test explicitly:**
- coordinates: points vs physical pixels, and speed parity across DPI
- natural-scrolling direction
- Cmd↔Ctrl remapping
- double-click timing
- text clipboard line endings (`\n` vs `\r\n`)
- clipboard HTML (Windows CF_HTML has header offsets)
- clipboard images (TIFF/PNG vs DIB)
- **file names in transfers:**
  - `:` is legal on Mac but not on Windows
  - Windows reserved names (`CON`, `NUL`…)
  - Windows is case-insensitive, so name collisions are possible
  - trailing dots and spaces
  - path length limits
  - Unicode normalisation (NFC vs NFD)

**Exploratory checklist** (run before each release):
- [ ] A focused macOS password field (Secure Input): switching is blocked and a warning shows
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

## 5. Latency, soak and field diagnostics

**Latency.** The other machine echoes each `Motion.seq` back with its inject timestamp. The controlling machine then computes capture→inject→ack round trips without needing synchronised clocks.
- Record p50/p99/max in each lab session, with and without a concurrent file transfer.
- Alert if p99 regresses by more than 20%.
- For a final feel check, film both screens with a 240 fps phone camera to get end-to-end latency.

**Soak.** Run the lab overnight with randomised scripted input and switches. Check for:
- no memory growth (iroh issues #4509/#4390 are relevant)
- no dropped hooks or taps
- no stuck keys
- steady reconnect behaviour

**Built-in diagnostics counters,** shown in a debug panel and saved with logs. These are the silent failures users will report as "it randomly stopped working":
- macOS event tap disabled (by timeout or by user input)
- Windows low-level hook callback took over N ms, or the hook was re-registered
- datagram loss %
- reconnects
- time spent on the relay vs a direct path
- Secure Input blocks

---

## 6. Packaging and permissions (clean machines)

Permission behaviour depends on the signature and the install, so development builds won't reveal it.

- **macOS:** use Tart VM snapshots (Apple silicon) with a fresh user, and test the signed and notarized build:
  - the Accessibility prompt and flow, and relaunch after granting
  - the Local Network prompt (bundled apps only; binaries run from Terminal are exempt)
  - `LSUIElement` (no dock icon)
  - launch at login
  - an upgrade keeps the permission grant (same signing identity and bundle id)
- **Windows:** use Hyper-V or UTM snapshots:
  - the installer adds its firewall rule
  - discovery on a "Public" network profile, where mDNS is blocked, shows the expected fallback message
  - SmartScreen with a signed build
  - launch at login
  - uninstall removes the hooks, the autostart entry and the firewall rule

---

## Suggested CI layout

| Trigger | Jobs |
|---|---|
| Every PR | Layers 1 and 2 on Linux/macOS/Windows; layer 3 on `macos-latest` + `windows-latest`; clippy, fmt; a short fuzz smoke run (~60 s per target) |
| Nightly (hosted) | Long fuzz runs; the full conformance suite on every hosted runner (macOS, Windows x64, Windows ARM64) |
| Pre-release | Full lab matrix, the exploratory checklist, clean-machine packaging tests, the version-skew suite against the last two releases |

## References

- [enigo integration workflow](https://github.com/enigo-rs/enigo/blob/main/.github/workflows/integration.yml)
- [AutoControlGUI #482: macOS runner TCC measurements](https://github.com/Integration-Automation/AutoControlGUI/pull/482)
- [runner-images #1567: Accessibility on macOS runners](https://github.com/actions/runner-images/issues/1567)
- [Tart (macOS VMs)](https://github.com/cirruslabs/tart)
