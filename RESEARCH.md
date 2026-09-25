# Legato: feasibility research

Researched 2026-09-24. Versions are the latest on crates.io on that date. The code in `spikes/` was compiled and run on macOS arm64 only. Nothing has been built or run on Windows or Linux yet.

---

## Decisions so far (2026-09-24)

| Topic | Decision |
|---|---|
| Platforms | macOS and Windows only. Linux/Wayland is out of scope. |
| Minimum OS | macOS 26+, Apple silicon only; Windows 11 only, x64 + ARM64. |
| License | MIT. We can't depend on lan-mouse (GPL-3.0) crates; it's a clean-room reference only. |
| Control model | A setting, switchable at runtime: **follow input** (whichever machine's keyboard or mouse you touch takes over) or **designated controller**. That means a symmetric core: every machine runs capture *and* emulation, and the layout is replicated to all peers. |
| Discovery and connectivity | Automatic LAN discovery via mDNS, with no copy/paste or QR. Paired peers connect both locally and remotely (mDNS + pkarr/DNS + relay). |
| Identity key storage | OS keychain (macOS Keychain / Windows Credential Manager) via the `keyring` crate. |
| macOS signing | No Apple Developer ID yet. The CLI phase runs from Terminal; bundled dev builds use a self-signed certificate. |
| Name | **Legato**, repo `tvolk131/legato`. Renamed from the working name "synergy-rs" because Synergy is a Symless trademark. |
| Primary use case | A triple-monitor **Windows PC drives a MacBook**, using the Windows keyboard and mouse. Build Windows capture + macOS injection first. macOS capture only needs to detect local input so it can hand control back; Windows injection comes later. |
| Future: virtual monitor | **macOS shown on Windows** (§14). The inverse and all Linux support are skipped. |

---

## 1. Verdict

**Feasible.** Every building block exists in Rust today. Most of the effort goes into writing a separate input backend for each operating system and into the many small edge cases, not into anything unsolved. Wayland and native file drag-and-drop are the two areas that need to be scoped down.

| Area | Confidence | Why |
|---|---|---|
| Mouse and keyboard on macOS, Windows, X11 | **High** | Synergy has used the same techniques for 15+ years. All the OS APIs are reachable through `objc2-core-graphics`, `windows` and `x11rb`. |
| Mouse and keyboard on Wayland | **Medium on GNOME/KDE, low elsewhere** | Wayland blocks global capture by design. You need portals + libei (GNOME 46+, KDE 6.1+), wlroots-specific protocols, or evdev/uinput with elevated privileges. |
| Networking with iroh | **High** | 1.0 shipped 2026-06-15 and 1.2.0 is current. It gives authenticated, end-to-end encrypted QUIC, mDNS on the LAN, and datagrams. |
| UI with iced + iced-m3 | **Medium-high** | They resolve and compile together (iced 0.14.0 + iced-m3 0.1.0-beta.2). iced-m3 is a very new beta. |
| Clipboard (text, images) | **High, except GNOME Wayland** | `arboard` and `clipboard-rs` cover it. GNOME needs the portal Clipboard API. |
| File drag-and-drop | **Low for fully native, high for simplified** | No open-source KVM has shipped reliable cross-machine drag-and-drop. Do it in phases (§8). |

---

## 2. Stack check

| Crate | Version | Notes |
|---|---|---|
| `iroh` | 1.2.0 | semver-stable 1.x, MSRV 1.91. Uses n0's Quinn fork `noq` (multipath QUIC). |
| `iroh-mdns-address-lookup` | 0.5.0 | mDNS was moved out of core iroh in 1.0. Still 0.x, and it already had a breaking change in August. |
| `iroh-tickets` | 1.0.0 | Compact, QR-friendly strings that encode an endpoint's address. |
| `iced` | 0.14.0 | Still the latest release. 0.14.x subcrate patches are still landing. Uses winit 0.30.13. |
| `iced-m3` | 0.1.0-beta.2 | Pins `iced = "=0.14.0"`, so the app is locked to that exact iced version. |
| `tray-icon` / `muda` | 0.25.1 / 0.20 | 0.25 adds a GTK-free Linux backend (`ksni`). |
| `objc2-core-graphics` / `objc2-app-kit` | 0.3.2 | macOS event taps, event posting, cursor warping, pasteboard. |
| `windows` | 0.62.2 | Windows low-level hooks, Raw Input, `SendInput`, OLE. |
| `x11rb` | 0.14.0 | X11 input extensions (xinput, xtest, xfixes). |
| `ashpd` / `reis` | 0.13.13 / 0.7.1 | Wayland InputCapture and RemoteDesktop portals, and a pure-Rust libei. reis says its API is unstable. |
| `evdev` | 0.13.2 | Linux fallback: exclusive device grab plus a uinput virtual device. |
| `keycode` | 1.0.0 | Converts between USB HID, evdev, Windows scancodes and macOS keycodes (Chromium's table). |
| `arboard` / `clipboard-rs` | 3.6.1 / 0.3.5 | Clipboard. clipboard-rs adds change watching and raw formats. |
| `drag` | 2.1.1 | Starts a native drag from the app on macOS and Windows. Its Linux support needs GTK, so it can't be used with iced. |
| `display-info` | 0.5.9 | Monitor rectangles and scale factors. iced can't list monitors itself. |

**Licensing matters here.** The most reusable prior art is GPL: lan-mouse's `input-capture`, `input-emulation`, `input-event` and `lan-mouse-proto` 0.4 are GPL-3.0-or-later, and Deskflow is GPL-2.0. Legato is MIT-licensed, so lan-mouse is a clean-room reference only: we read it to understand techniques and never copy its code (§11).

---

## 3. Architecture

```
┌──────────────────────────────── one process ────────────────────────────────┐
│  main thread: iced::daemon (tray icon, settings window, iced-m3 widgets)    │
│        ▲ Subscription::run(stream::channel)       │ commands (mpsc)          │
│        │ events: peers, status, layout            ▼                          │
│  ┌─────────────────────── engine (own tokio runtime) ───────────────────┐   │
│  │  session state machine · layout model · clipboard sync · file xfer   │   │
│  │  iroh Endpoint + Router (ALPN "legato/1", "legato/pair/1")           │   │
│  └──────▲───────────────────────────────────────────────┬──────────────┘   │
│         │ lock-free channel (+ shared atomic mode flag)  │                  │
│  ┌──────┴──────────────┐                     ┌───────────▼─────────────┐    │
│  │ capture thread      │                     │ emulation               │    │
│  │ CGEventTap run loop │                     │ CGEventPost / SendInput │    │
│  │ / LL-hook msg loop  │                     │ / XTest / libei / uinput│    │
│  │ / XI2 / libei recv  │                     └─────────────────────────┘    │
│  └─────────────────────┘                                                    │
└─────────────────────────────────────────────────────────────────────────────┘
```

- **Hooks get their own OS threads.** macOS event taps need a CFRunLoop, and Windows low-level hooks need a message loop. Windows silently removes a hook whose callback takes longer than about 1 s, and macOS disables taps on timeout. So the hook callbacks do constant-time work and push events to a channel.
- **The "swallow or pass through" decision is synchronous.** The hook has to return its verdict immediately and can't `await` the engine. The current mode (`Local`, or `Remote(peer)`) must therefore live in an atomic that the hook thread can read.
- **Engine and UI are separate.** Keep a clean boundary between the engine (crate `legato-core`) and the UI so they can later be split into a headless daemon plus a UI client, as lan-mouse does. Start with a single process: on macOS, permission grants are tied to one signed binary, and one process is simpler.
- **Sending from the hook thread is possible.** iroh's `Connection::send_datagram` is synchronous and works from a plain OS thread with no tokio context (verified in `spikes/iroh-probe/src/bin/thread.rs`). Motion could go straight from the capture thread if that channel hop ever matters.

Suggested workspace:
- `proto`: wire types, using serde + postcard.
- `input`: `Capture` / `Emulate` traits with one module per OS.
- `net`: iroh endpoint, pairing, session.
- `core`: layout, edge state machine, clipboard.
- `app`: iced + iced-m3 UI and tray.

---

## 4. Mouse movement

### 4.1 The mechanism every Synergy-style tool uses

```
            Local                                         Remote(peer)
 ┌──────────────────────────┐   cursor reaches an   ┌──────────────────────────────┐
 │ hook passes events thru  │ ── outer edge that ──►│ hook SWALLOWS every event     │
 │ watch cursor position    │   has a neighbour     │ hide local cursor, pin it     │
 └──────────────────────────┘   (+dwell/double-tap) │ read delta per event          │
            ▲                                       │ virtual_pos += delta·scale    │
            │ virtual cursor exits the peer's       │ clamp to peer's screen rects  │
            │ edge back toward us → warp real       │ send Motion datagram          │
            │ cursor to the mapped point, unhide    └──────────────────────────────┘
            │ (or exits toward a 3rd machine → Remote(other))
            └── also: heartbeat timeout / disconnect / panic hotkey → Local + release all keys
```

1. **Detect the edge.** Watch cursor positions in the hook. An edge only counts if it's an *outer* edge of this machine's desktop (not a boundary between two local monitors) and the layout has a neighbour there. Treat each machine's desktop as a union of rectangles, not a bounding box. Optional guards: a dwell time, a double-tap, dead corners, or "don't switch while a button is held".
2. **Capture.** Once you switch to `Remote`, the hook swallows every mouse and keyboard event, so nothing happens locally. You then read *deltas* instead of positions:
   - **macOS:** read `kCGMouseEventDeltaX/Y` from each event. Warp the hidden cursor back with `CGWarpMouseCursorPosition`, and lower `CGEventSourceSetLocalEventsSuppressionInterval` from its 0.25 s default (lan-mouse uses 0.05 s) or motion stutters.
   - **Windows:** a swallowed move never actually moves the cursor, so `MSLLHOOKSTRUCT.pt − pinned point` is the delta, with acceleration already applied. Raw Input (`RIDEV_INPUTSINK`) gives unaccelerated device deltas if you want them.
   - **X11:** `XGrabPointer`/`XGrabKeyboard` on your own window, then either warp to the center or read raw valuators from `XI_RawMotion`.
3. **Track a virtual cursor.** Add the deltas to a virtual position in the *client's* coordinate space. Scale them by the ratio of the two machines' DPI so speed feels the same. Clamp to the rectangles the client reported. When the virtual cursor crosses one of the client's outer edges, look up that side's neighbour and switch: back to `Local` (warp the real cursor to the matching point and unhide it) or to another client.
4. **Hide the server's cursor.** On macOS, `CGDisplayHideCursor` only works while the app is frontmost. Barrier and lan-mouse use the private `CGSSetConnectionProperty(…, "SetsCursorInBackground", true)`. Windows has no clean API for a background process; the options are a blank `SetSystemCursor` (you must restore it after a crash) or leaving the cursor pinned and visible. Plan to prototype this.

### 4.2 Injecting on the client

| OS | API | Gotchas |
|---|---|---|
| macOS | `CGEventPost(kCGHIDEventTap, …)` | Needs Accessibility permission. While a button is held, send the `*MouseDragged` event types instead of plain moves. Double-clicks need `kCGMouseEventClickState`, which you compute yourself. Set modifier flags on every event. |
| Windows | `SendInput` | `MOUSEEVENTF_MOVE` (relative) goes through the user's pointer-acceleration settings, up to about 4×. Use `ABSOLUTE \| VIRTUALDESK` (coordinates normalised to 0–65535) or `SetCursorPos`. Injection silently fails into elevated windows (UIPI) unless you run elevated or with `uiAccess`. A PC with no physical mouse may hide the cursor entirely (a known lan-mouse issue). |
| X11 | XTest `FakeMotion` / `FakeButton` | Scrolling is buttons 4–7, which may mean no smooth scrolling. |
| Wayland (GNOME/KDE) | RemoteDesktop portal → libei sender (`ashpd` + `reis`) | Shows a user consent dialog; persist the restore token so it isn't asked every time. |
| Wayland (wlroots/Sway/Hyprland) | `wlr-virtual-pointer`, `zwp_virtual_keyboard_v1` | The compositor must support these protocols. |
| Linux (any) | uinput virtual device via `evdev` | Needs root or `input` group membership. |

### 4.3 Absolute or relative on the wire

Recommendation: **send absolute positions by default, with a relative mode as an option.**

- **Absolute:** the server tracks the virtual cursor and sends `Motion { seq, x, y }` in client coordinates. Each message is idempotent: if a datagram is lost, the next one supersedes it with no accumulated drift. It also avoids Windows applying acceleration a second time.
- **Relative:** needed when an app on the client has locked the pointer (games, 3D viewports). Offer it per screen, or switch to it when the client reports pointer lock.
- **Client's own mouse:** if the client's local mouse moves the cursor, the server's virtual position goes stale. The client should report local cursor movement so the server can resync or step back.

### 4.4 Scroll and buttons

- **Scroll:** send high-resolution units. Windows and Linux wheels use `value120` (fractions of a 120-unit notch). Continuous macOS trackpads report pixel deltas (`kCGScrollWheelEventIsContinuous`). Momentum and gesture phases are a later refinement.
- **Buttons** go on the *reliable* stream and carry the cursor position (`Button { b, down, x, y }`), so a click still lands in the right place if the latest motion datagram was lost or arrived out of order.

---

## 5. Keyboard

**Send physical key positions, not characters.** Use the USB HID usage code on the wire and convert to and from native codes with the `keycode` crate:
- evdev on Linux (the xkb code is evdev + 8)
- Windows set-1 scancodes (extended keys are E0-prefixed)
- macOS virtual keycodes

This behaves like plugging the keyboard into the client. The client's own layout, IME, dead keys and shortcuts all work. Synergy instead sends keysym-like "KeyIDs" and tries to reproduce the server's *character* on the client, and that has been its longest-running bug source (Deskflow issue #4280 has 143 comments). Optionally add a per-client "Unicode text" mode later, using `KEYEVENTF_UNICODE` on Windows and `CGEventKeyboardSetUnicodeString` on macOS.

Rules for correctness:
- **Modifier and lock sync.** The `Enter` message carries the server's modifier state and Caps/Num Lock, and the client reconciles its own state.
- **Auto-repeat has one owner.** The server drops OS-generated repeat events. Clients repeat held keys themselves:
  - macOS and Windows: injected held keys don't auto-repeat, so run a timer using the client's own repeat delay and rate (lan-mouse uses 500 ms / 32 ms).
  - X11 and Wayland: the display server or compositor already handles repeat.
- **No stuck keys.**
  - Both sides track which keys and buttons are held.
  - On `Leave`, disconnect or heartbeat timeout, the client releases everything it pressed.
  - The server remembers where each key-*down* went and sends the matching key-*up* to the same place, even if you switched screens in between.
  - Key and button events travel on the reliable stream, never as datagrams.
- **Special combos.**
  - Cmd+Tab, the Win key and similar can be captured with active taps and hooks.
  - Ctrl+Alt+Del can't be captured on Windows. Injecting it needs `SendSAS` from a SYSTEM service.
  - The UAC prompt, lock screen and login screen run on the Windows secure desktop, which is only reachable from a SYSTEM service (Deskflow runs one, and it's fragile). Defer this.
- **macOS Secure Event Input.** When a password field (or 1Password, etc.) is focused, *keyboard* events stop reaching every event tap. A macOS server would then type the "remote" keystrokes locally. Check `IsSecureEventInputEnabled()`, and warn or refuse to switch while it's on.

---

## 6. Networking with iroh

### 6.1 Why iroh fits

- **Identity is an ed25519 key.** The handshake is TLS 1.3 with raw public keys, so `conn.remote_id()` is cryptographically authenticated. Traffic is end-to-end encrypted even through a relay; the relay sees only IPs, timing and volume.
- **LAN-only mode.** `presets::Minimal` + `MdnsAddressLookup` works with zero config and no internet: dial by `EndpointId` and mDNS resolves the address (verified).
- **Measured round-trip times (spike, 2000 round trips):** datagram p50 **118 µs** / p99 ~500 µs; bi-stream p50 104 µs; LAN connect ~3 ms. Both endpoints were on the *same host*, so this measures stack overhead, not Wi-Fi. For context, a 1 kHz mouse reports every 1 ms, and added latency becomes noticeable at around 16 ms.
- **Optional "over the internet" mode.** `presets::N0` or a self-hosted `iroh-relay` handles NAT hole-punching and upgrades from relay to direct (about 76 ms in the spike, a best case). n0 reports that about 90% of network setups end up with a direct connection.
- **Decision (2026-09-24): discover on the LAN, connect from anywhere.** Unpaired devices are found only via mDNS on the local network. Paired devices dial each other by `EndpointId`, using mDNS, pkarr/DNS and relays together (`address_lookup` adds to the list each time it's called). Verified in `spikes/iroh-probe/src/bin/hybrid.rs`:
  - The mDNS peer connected in 735 ms over a direct LAN path.
  - The peer without mDNS connected in 240 ms via the relay, and switched to a direct path 80 ms later. Both peers were on the same host, so that upgrade is a best case.

### 6.2 Protocol layout (one connection per peer pair)

| Channel | Carries | Why |
|---|---|---|
| ALPN `legato/1` | The session. Only paired peers are allowed, enforced centrally in an `EndpointHooks::after_handshake` hook (`spikes/iroh-probe/src/bin/hook.rs`). | Bump the ALPN version on incompatible changes. |
| ALPN `legato/pair/1` | Pairing. Open to anyone, rate-limited. | See §6.3. |
| **Control bi-stream** | `Hello {proto, name, screens}`, layout, `Enter {x, y, mods, locks}`, `Leave`, `Key`, `Button`, `Scroll`, clipboard offers, `ReleaseAll`. Length-prefixed postcard frames. | Reliable and ordered, so key and button events are never lost. |
| **Datagrams** | `Motion {seq, x, y}` (or `{seq, dx, dy}` in relative mode), coalesced to ≤1 kHz. Datagrams from a previous visit to the screen are ignored because `Enter` carries a baseline `seq`. Also ping/pong every 250 ms for liveness. | Unreliable is fine for motion because only the newest position matters, and it avoids head-of-line blocking during Wi-Fi loss bursts. |
| **Uni-streams, one per transfer** | Clipboard payloads and files. Header `{kind, name, size, blake3}` plus a resume offset. Use `set_priority` below the control stream and an app-level rate cap. | Keeps bulk data from queuing ahead of input. Datagrams and streams share one congestion window. |

```rust
// Verified against iroh 1.2.0 (see spikes/iroh-probe)
let ep = Endpoint::builder(presets::N0)                 // relays + pkarr/DNS (swap in own relay later)
    .secret_key(sk)                                     // persist sk.to_bytes()
    .portmapper_config(PortmapperConfig::Disabled)      // avoids macOS firewall prompt
    .transport_config(QuicTransportConfig::builder()
        .keep_alive_interval(Duration::from_secs(1)).build())
    .address_lookup(MdnsAddressLookup::builder().service_name("legato"))
    .hooks(PairedOnly(allow_list.clone()))
    .alpns(vec![KVM_ALPN.to_vec(), PAIR_ALPN.to_vec()])
    .bind().await?;
let router = Router::builder(ep.clone()).accept(KVM_ALPN, KvmHandler { .. }).spawn();
let conn = ep.connect(peer_id, KVM_ALPN).await?;       // mDNS resolves the id
conn.send_datagram(motion_bytes)?;                      // sync; drops oldest if buffer is full
```

Set `max_idle_timeout` to about 3–5 s so a dead client is detected quickly and control snaps back to the server. If `max_datagram_size()` returns `None`, fall back to a stream. The spike measured 1162 bytes, far more than a motion message needs.

### 6.3 Pairing

This is verified in `spikes/iroh-probe/src/bin/pair.rs`.

1. The new machine connects on `legato/pair/1`, found via mDNS or a 77-character ticket string / QR code.
2. Both sides compute a short code from the TLS session:
   ```rust
   conn.export_keying_material(&mut [0u8;4], b"legato pairing SAS v1", b"")
   ```
   Each takes the value mod 10⁶ and shows it as a 6-digit code. The two sides produced the identical code in the test.
3. The user confirms the codes match on both machines, and each adds the other's `EndpointId` to its allow-list.

Because the code is derived from the live TLS session, a man-in-the-middle can't make the two codes match. lan-mouse only verifies the peer in one direction, and Barrier had CVE-2021-42072; this design avoids both of those mistakes.

The mDNS `user_data` field (≤245 bytes, unauthenticated) can advertise a display name for the "nearby devices" list.

### 6.4 iroh gotchas

- **Keep one connection per peer.** iroh issues #4509 and #4390 report unbounded memory growth with two or more connections to the same peer; the fixes aren't merged as of 1.2.0.
- **Disable the portmapper on the LAN** (`PortmapperConfig::Disabled`). Its SSDP probing triggers macOS firewall dialogs (#4349).
- **mDNS is deliberately slow and paced**, and multiple network interfaces aren't supported. Offer "add by ticket" or "add by IP" as a fallback.
- **iroh needs a tokio runtime.** `MdnsAddressLookup::build` panics outside one. Give the engine its own multi-threaded runtime rather than relying on iced's executor.
- **Skip `iroh-blobs` for now.** Its current version (0.103) calls itself "not production quality", and plain uni-streams are enough for this app. `iroh-gossip` and `iroh-docs` aren't needed either, because the server is the single source of truth for the layout.
- **Binary size:** 7.7 MB stripped with LTO on macOS arm64.

---

## 7. Clipboard

Model: **eagerly announce, push small payloads on switch, and handle large ones explicitly.**

- **Watching for changes:** `clipboard-rs`'s watcher polls `changeCount` on macOS, uses `AddClipboardFormatListener` on Windows, and uses XFixes selection events on X11.
  - wlroots and KDE Wayland: the `ext-`/`wlr-data-control` protocols (via `wl-clipboard-rs`).
  - **GNOME Wayland** implements neither. Use the portal Clipboard interface attached to the RemoteDesktop session, which a Wayland client needs for input anyway.
- **Sync:** when the machine with focus gets a new clipboard, it sends a `ClipboardOffer {formats, size}`. When the cursor enters another screen, text, HTML and images below a size cap are pushed to it, as Synergy does. Real paste-time lazy fetching needs native delayed rendering on each OS (`WM_RENDERFORMAT`, `NSPasteboardItemDataProvider`, X11 INCR). That's a later optimisation.
- **Echo loops:** remember the hash of content you set from a remote peer, so your own watcher doesn't send it straight back.
- **Files on the clipboard (MVP):** transfer the files into a cache folder, then set a file list on the destination clipboard with arboard `set().file_list()` (CF_HDROP on Windows, NSURL on macOS, `text/uri-list` on Linux). Mouse Without Borders does this. arboard doesn't write `x-special/gnome-copied-files`, so pasting in Nautilus may need clipboard-rs.
- **macOS 15.4+ pasteboard privacy:** reading the pasteboard without user interaction can show an alert. Reading `changeCount` reportedly doesn't. Test this with a bundled, signed build.

---

## 8. File drag-and-drop

History:
- Synergy 1.x shipped drag-and-drop. Only one file was sent. On Windows targets the file was just dropped on the Desktop; on macOS targets it used a file-promise drag. Deskflow removed it in 2025 as "broken on all platforms".
- Mouse Without Borders supports one file up to 100 MB, saved to a folder on the Desktop.
- Apple Universal Control is the only polished implementation. It uses file promises over private frameworks.

### 8.1 Mechanics

**Source side: detecting a drag at the moment the cursor hits the edge with the left button held**

- **macOS:** read `NSPasteboard(name: .drag)`. If its `changeCount` changed since mouse-down and it holds file URLs, a file drag is in progress, and those URLs are its files.
- **Windows:** there's no API for "what's being dragged right now". Synergy and Mouse Without Borders both place a tiny topmost drop-target window under the cursor. The OLE drag then fires `DragEnter` with CF_HDROP (the file paths), after which they fake Esc to cancel the local drag. A small undecorated iced window moved under the cursor should get winit's `FileHovered` event for the same effect on Windows, macOS and X11, possibly after nudging the cursor 1 px (unverified).
- **X11:** the same window trick, or read `XdndSelection` as `text/uri-list`.
- **Wayland:** effectively impossible. Drag events only go to the surface under the pointer, and apps can't position windows.

**Transfer:** as soon as a drag is detected, start streaming the files on low-priority uni-streams while the user is still moving across.

**Target side: completing the drop**

- **MVP:** on the remote mouse-up, save the files to `~/Downloads/legato/` and notify, the Mouse Without Borders approach. A "drop icon" overlay can follow the cursor while dragging.
- **Native drop (macOS and Windows):** once the files have arrived:
  1. Move a tiny transparent iced window under the injected cursor.
  2. Inject a mouse-down.
  3. Call `drag::start_drag(window, DragItem::Files(paths), …)` so a real OS drag session follows the user's cursor and they can drop into any app.
  - On macOS this call goes on the main thread via `iced::window::run`.
  - On Windows `DoDragDrop` blocks and runs its own message loop, so use a dedicated STA thread.
- **Lazy "full fidelity":** file promises start the drag before the data has arrived. That means `NSFilePromiseProvider` on macOS and `CFSTR_FILEDESCRIPTORW` + `CFSTR_FILECONTENTS` virtual files on Windows (how RDP does it). Both are hand-written native code.
- **Blockers:**
  - `drag` needs GTK on Linux, so it's unusable here.
  - winit 0.30 only accepts drops *into* windows. winit 0.31 (beta) adds `start_drag` for AppKit, Win32 and Wayland, but iced won't pick it up before its next major version.

### 8.2 Recommended phases

1. **"Send files to…"** Drop files onto the app window (iced `FileDropped`) or use a tray menu entry. They land in the other machine's Downloads folder. Also sync copied files through the clipboard, with a size cap.
2. **Drag across the edge, then drop anywhere, files saved to Downloads.** macOS reads the drag pasteboard; Windows and X11 use the catcher window.
3. **Native drop on macOS and Windows targets** via `drag::start_drag` after the transfer completes.
4. **Lazy file promises** and an X11 drag source. Wayland only through a future iced on winit 0.31, and probably never on GNOME.

---

## 9. UI (iced 0.14 + iced-m3)

- **App shape:** use `iced::daemon(boot, update, view)` for a tray app. It starts with no window and keeps running after the last window closes. `view` takes `(&State, window::Id)`, and windows are opened with `window::open(settings)`.
- **Engine bridge:** `Subscription::run(fn() -> impl Stream)` with `iced::stream::channel`. The stream's first item is an `mpsc::Sender` for sending commands back to the engine. In 0.14, `Subscription::map` requires a non-capturing closure; use `.with(value)` to pass context.
- **Tray:** `tray-icon` + `muda`, with events polled or bridged from their channels. On macOS, create the tray *after* the event loop starts (on the first `update`, not in `boot`).
- **No dock icon (macOS):** `LSUIElement=true` in Info.plist for bundled builds. For `cargo run`, call `NSApplication::setActivationPolicy(Accessory)` via objc2-app-kit.
- **Autostart and single instance:** `auto-launch` 0.6. Use `interprocess` local sockets for single-instance plus a "show window" IPC, rather than the stale `single-instance` crate.
- **Screen-arrangement editor:** implement `canvas::Program<Message, iced_m3::Theme>` with hit-testing, a drag offset, `Action::publish(MoveScreen)` + `capture()`, and `mouse::Interaction::Grab`/`Grabbing`. The compile spike's version is about 80 lines; snapping and a "touching but not overlapping" rule are extra.
- **Monitor geometry:** use `display-info` (`DisplayInfo::all()` gives x, y, w, h, scale and primary). iced only exposes `monitor_size(window_id)`. Re-read on hot-plug, either with native callbacks (`CGDisplayRegisterReconfigurationCallback`, `WM_DISPLAYCHANGE`, RandR) or by polling.
- **iced-m3 specifics:**
  - Your app uses `iced_m3::Theme`, and the crate provides `Element<'a, M> = iced::Element<'a, M, Theme>`.
  - Plain iced widgets and all of `iced_aw` are styled for `iced::Theme`, so wrap them in `widget::themer(Some(theme.iced()), …)`. `canvas` needs no wrapping.
  - Screen readers aren't supported.
  - Windows and Linux interaction, IME, and mixed-DPI scaling are listed as untested.
- **Notes on iced-m3's `Cargo.toml` (as its maintainer would care):** the exact pins `material-colors = "=0.4.2"` and `unicode-segmentation = "=1.13.3"` will cause resolution conflicts downstream as soon as another dependency needs a newer version. Caret requirements avoid this. The `iced = "=0.14.0"` pin is defensible, but it means apps move in lockstep with iced-m3 releases.

---

## 10. Permissions and packaging

| OS | What's needed |
|---|---|
| macOS | **Accessibility** permission, needed for both the active event tap and event posting. It includes Input Monitoring. **A stable Developer ID signature:** macOS ties the grant to the signature, and with ad-hoc signing every rebuild loses it. `NSLocalNetworkUsageDescription`, because LAN UDP and multicast need the Local Network permission on macOS 15+ (binaries run from Terminal are exempt, so this won't show up during `cargo run`). `LSUIElement`. Notarization for distribution. |
| Windows | Nothing for the basics. Add a firewall rule in the installer; mDNS is blocked on "Public" network profiles. Declare PerMonitorV2 DPI awareness in the manifest. Controlling elevated windows needs elevation or `uiAccess` (signed + installed under Program Files). UAC prompts and the lock screen need a SYSTEM service. |
| Linux X11 | None. |
| Linux Wayland | Portal consent dialogs; store restore tokens. The evdev/uinput fallback needs the `input` group, which effectively lets you read every input device, so make it opt-in. |

---

## 11. Risks (highest first) and decisions to make early

**Risks**
1. **Wayland fragmentation.**
   - InputCapture works on GNOME 46+ and Plasma 6.1+.
   - Hyprland has it but with bugs (EIS fd leak, missing modifiers).
   - xdg-desktop-portal-wlr has only an open PR, and COSMIC and niri have nothing.
   - Deskflow's portal backend still can't hide the server's cursor or sync modifiers.
   - Plan three backends (portal, wlroots protocols, evdev) or scope Wayland down to GNOME and KDE.
2. **macOS permissions and hidden behaviour:** TCC grants tied to the signature, Secure Input, taps silently disabled, the private cursor-hiding API, pasteboard privacy alerts.
3. **Windows integrity levels:** UIPI blocks injection into elevated windows, and there's no access to the secure desktop without a SYSTEM service.
4. **Keyboard edge cases:** AltGr, dead keys, lock-key sync. Sending physical keys shrinks this considerably.
5. **Drag-and-drop:** keep it phased (§8).
6. **Dependency maturity:** iced-m3 is a beta pinned to iced 0.14.0. `reis` has an unstable API. `iroh-mdns-address-lookup` is 0.x.

**Decisions**
1. **License.** GPL-3.0 lets you depend on lan-mouse's `input-capture` and `input-emulation`, which already cover macOS, Windows, wlroots, libei/portal and X11 emulation. That saves most of the backend work. A permissive license means writing it clean-room.
2. **Topology.** Either a single primary (the Synergy "server" owns the layout and the physical keyboard and mouse) or symmetric (any machine can drive, as in lan-mouse). A single primary is much simpler; start there.
3. **Deskflow wire compatibility.** Probably not worth it: it's a different transport, and the only Rust implementation (`schengen`) is GPL-3.0.
4. **Platform order.** macOS ↔ Windows first, since those backends are the most tractable.

---

## 12. Milestones

Revised once the scope settled on macOS and Windows, with a Mac next to a Windows PC as the first use case.

1. **M0, spikes (done).** iroh LAN + mDNS + pairing + allow-list; iced + iced-m3 + canvas + daemon compile check (`spikes/`).
2. **M1, CLI prototype, Windows driving a Mac (done).** Edge switching with push-through, motion datagrams, keys and buttons on the control stream, release-all-on-disconnect, stuck-key prevention.
3. **M2, UI (done).** Tray, nearby devices, pairing dialog with the 6-digit code, arrangement editor, settings, start at login.
4. **M3, both directions (done).** The Mac drives Windows too; "whichever machine you touch" vs "one machine controls" setting, synced between machines; text, image and file clipboard in both directions; arrangements mirrored so either side can edit them.
5. **M4, files (done).** "Send files…" to a device, and dragging files across the edge from Windows (released on the other machine, they're saved to `Downloads/Legato`). Dragging out of Finder is still to come: macOS offers no way to see another app's drag without taking part in it.
6. **M5, virtual monitor mode (done).** The Mac gets an extra display shown in a window on Windows (§14): "Show as display" next to a connected Mac, F11 for full screen.
7. **Later.** Native drop at the pointer on the receiving side, Finder drags, internet mode with a self-hosted relay, Windows secure-desktop service.

## 13. Open questions to prototype

- Windows: does Raw Input still get `WM_INPUT` for moves that a low-level hook swallowed? Sources conflict.
- Windows: do low-level hooks in a non-elevated process still see input while an elevated window has focus?
- Windows: the best way to hide the cursor from a background process.
- macOS: does the `SetsCursorInBackground` private API still work on current macOS? Do pasteboard privacy alerts fire for a background agent reading the drag pasteboard?
- Wayland: do compositors apply pointer acceleration to libei or uinput relative motion?
- iroh: how long mDNS takes to find a peer in practice, and the datagram round-trip time across real Wi-Fi rather than one host.
- iced: does an undecorated always-on-top iced window receive `FileHovered` during an OS drag it didn't start?

---

## 14. Virtual monitor mode (macOS shown on Windows)

Built in `legato-screen` as planned below, with these choices:
- **Display size:** 3840×2160 pixels in HiDPI by default, so it looks like 1920×1080 and is sharp full screen on a 4K monitor. Change it under `[extend]` in `legato.toml`.
- **Encoding:** H.264 Constrained High, low-latency rate control, 60 fps, 40 Mbit/s. Keyframes come on request, plus one a minute. When the screen is still, the last picture is re-sent as a keyframe.
- **Transport:** one ordered stream per video track, below input and above file transfers. Ordered delivery keeps P-frames decodable. When a track's frames back up, the Mac skips its new pictures before encoding them, and re-sends the latest sharp one once the backlog clears so a still screen isn't left stale.
- **Adaptive quality** (the default): two tracks, each its own H.264 sequence. The sharp track is the full stream size, limited to the frame rate the Mac can encode at that size. The moving track is at most 1920×1080 (or 2560×1440), scaled on the GPU with `VTPixelTransferSession`, at the display's frame rate. ScreenCaptureKit's dirty rectangles say how much of each picture changed. Two pictures within 100 ms that each change at least 3% switch to the moving track, so a lone big change like switching tabs stays sharp instead of flashing soft. The sharp track takes over again after 150 ms of changes under 1%, re-encoding the latest picture, with one touch-up encode 300 ms later if the screen is still. Each track's next frame follows on from its own last one, so switching needs no keyframes. Frames carry a timestamp from one clock, and the viewer shows whichever track's picture is newest; the moving track's stream has higher priority, so a big sharpening frame never holds up the next moving one. Scaling costs about 3 ms per picture whatever the sizes (mostly the GPU round trip), which is counted towards the moving track's frame budget.
- **Viewer:** a wgpu shader widget draws NV12 textures directly (BT.709 limited range, converted on the GPU).
- **Input:** a *portal*. While the Windows pointer is over the picture, and the viewer isn't covered there, moves go to the Mac as absolute positions on the display, and so do clicks, keys and scrolling. A click or key over the picture goes in even without motion first. That covers the Mac having just taken its cursor back (its own trackpad or keyboard was used) and the viewer appearing under a resting pointer. The Windows pointer stays visible and ScreenCaptureKit leaves the Mac's cursor out of the picture, so there is no laggy second cursor.
- **Arrangement:** the virtual display is left out of the Mac's shared desk, so pushing past an edge never lands on it. It's reached through the viewer. A pointer on it therefore isn't on any of the Mac's shared screens, so the Mac treats it as at no edge. Windows' hook reports positions past the edge it's pushing against, so the Windows backend clamps them onto its screens first. Before this, a touch of the trackpad with the cursor on the virtual display read as pushing past the MacBook's top edge, and the Mac took over the PC every frame.
- **Options:** "Show as display" chooses full screen on a monitor or a window; a size that matches it (changing after resizes) or stays fixed; and a frame rate within what the Mac can encode at that size (about 2 ns per pixel: 60 fps at 4K, 120 at 1440p, 144 at 1080p).
- **Desk matching:** the Mac moves its extra display, with `CGConfigureDisplayOrigin`, to the side of the MacBook where the viewer sits on the shared desk.
- **Dragging between the Mac's screens:** moving between the viewer and the Mac's own screens is one continuous visit to the Mac, with held buttons kept held. Leaving the picture while dragging continues on the MacBook's screen, moving onto the extra display from the MacBook brings the Windows pointer out in the viewer, and a full-screen viewer can be pushed past its screen's edge towards the MacBook.
- **Stats:** each frame carries the Mac's time on it, from appearing on screen to being sent. The viewer adds network, decode and display time and shows the total.

The original plan:

The Mac gets an extra display in its own arrangement. That display's picture is streamed to Windows and shown in one viewer window, or full screen on one of the monitors. Any windows, menus and popups dragged onto it just work, because macOS treats it as a real display.

**Pipeline**
1. Create the display with `CGVirtualDisplay`. It's a private API with no public alternative; Apple's official route needs an entitlement Apple must grant. Keep it isolated in its own small crate.
2. Capture it with ScreenCaptureKit (IOSurface frames). Don't use `CGDisplayStream`, which is broken for virtual displays on macOS 15+.
3. Encode with VideoToolbox, hardware H.264 in low-latency mode.
4. Send over iroh, one uni-stream per frame so stale frames can be dropped.
5. Decode on Windows with Media Foundation's H.264 decoder. Avoid HEVC: many PCs lack the paid HEVC extension.
6. Show it with a wgpu texture in an iced `shader` widget.

**Input:** the Windows mouse inside the viewer sends absolute positions within the virtual display, and the Mac injects them (the existing KVM path, limited to a rectangle). Draw the cursor locally in the viewer from its position and shape, so the cursor feels instant while the content trails.

**Precedent:** [rustscreen](https://github.com/PeterXMR/rustscreen) uses Rust + objc2 + `CGVirtualDisplay` + ScreenCaptureKit + VideoToolbox and measures ~34 ms p50 glass-to-glass at 2400×1080@60. Its license isn't finalized, so it's a reference only.

**Risks**
- Apple can break the private API in any release.
- Screen Recording permission, with periodic re-confirmation.
- Small coloured text blurs under H.264; use a high bitrate or HiDPI, and later a lossless path for regions that haven't changed.
- It's only usable on direct connections, not through a relay.
- Zero-hack fallback: an HDMI or USB-C dummy display plug.

Per-window streaming (dragging individual app windows across) was also considered and rejected for now. It needs window tracking, capturing menus and popups, and hiding the real window somewhere, which is all much hackier.

---

## Sources (selected)

- **Prior art:**
  - Deskflow [protocol reference](https://github.com/deskflow/deskflow/blob/master/docs/dev/protocol_reference.md), [ProtocolTypes.h](https://github.com/deskflow/deskflow/blob/master/src/lib/deskflow/ProtocolTypes.h), [Server.cpp](https://github.com/deskflow/deskflow/blob/master/src/lib/server/Server.cpp), [Wayland status #7499](https://github.com/deskflow/deskflow/discussions/7499), [drag-and-drop removal #8459](https://github.com/deskflow/deskflow/issues/8459)
  - [Barrier platform sources](https://github.com/debauchee/barrier/tree/master/src/lib/platform)
  - [lan-mouse](https://github.com/feschber/lan-mouse) (`input-capture`, `input-emulation`, `lan-mouse-proto`)
  - [rkvm](https://github.com/htrefil/rkvm)
  - [Mouse Without Borders source](https://github.com/microsoft/PowerToys/tree/main/src/modules/MouseWithoutBorders/App/Core)
  - [Inside Universal Control](https://eclecticlight.co/2022/06/06/inside-universal-control/)
- **macOS:**
  - [Active tap → Accessibility](https://developer.apple.com/forums/thread/122492)
  - [TN2150 Secure Event Input](https://developer.apple.com/library/archive/technotes/tn2150/_index.html)
  - [TN3179 Local Network privacy](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy)
  - [CGEventSourceSetLocalEventsSuppressionInterval](https://developer.apple.com/documentation/coregraphics/1408783-cgeventsourcesetlocaleventssuppr)
- **Windows:**
  - [LowLevelMouseProc](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelmouseproc)
  - [MOUSEINPUT](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-mouseinput)
  - [SendInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput)
  - [uiAccess](https://learn.microsoft.com/en-us/previous-versions/windows/it-pro/windows-10/security/threat-protection/security-policy-settings/user-account-control-only-elevate-uiaccess-applications-that-are-installed-in-secure-locations)
- **Linux:**
  - [InputCapture portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html)
  - [RemoteDesktop portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)
  - [Portal Clipboard](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Clipboard.html)
  - [XI2 protocol](https://xorg.freedesktop.org/archive/current/doc/inputproto/XI2proto.txt)
- **iroh:**
  - [1.0 announcement](https://www.iroh.computer/blog/v1)
  - [docs](https://docs.iroh.computer/)
  - [CHANGELOG](https://github.com/n0-computer/iroh/blob/main/CHANGELOG.md)
  - issues [#4509](https://github.com/n0-computer/iroh/issues/4509), [#4349](https://github.com/n0-computer/iroh/issues/4349)
  - [mDNS pacing](https://github.com/n0-computer/iroh-address-lookups/issues/13)
- **iced:**
  - [iced::daemon](https://docs.rs/iced/0.14.0/iced/fn.daemon.html)
  - [Subscription](https://docs.rs/iced/0.14.0/iced/struct.Subscription.html)
  - [iced-m3](https://github.com/tvolk131/iced-m3)
  - [tray-icon](https://github.com/tauri-apps/tray-icon)
  - [iced tray PR #3021](https://github.com/iced-rs/iced/pull/3021)
- **Latency:** [Latency thresholds in mouse interaction](https://link.springer.com/chapter/10.1007/978-3-319-58475-1_4)
