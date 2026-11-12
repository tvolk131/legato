# Current architecture

This describes the implementation, checked against main `38d4f25` in November
2026. [RESEARCH.md](../RESEARCH.md) records the original feasibility work and
investigations; proposals there do not imply a feature is implemented. Hardware
validation and release readiness are tracked in the
[0.3.0 plan](releases/0.3.0-plan.md).

Legato supports macOS 26+ on Apple silicon and Windows 11 on x64 and ARM64.
Each machine captures and injects input. The control policy either follows local
input or designates one controller. A Mac can host an extra display streamed to
Windows; the opposite direction and Linux are outside the supported scope.

## Workspace and runtime

| Crate | Responsibility |
| --- | --- |
| `legato-proto` | Postcard wire types, framing, protocol version, pairing code derivation |
| `legato-core` | Pure layout geometry, controller/receiver state machines, keymaps, Caps Lock reconciliation, display placement |
| `legato-net` | iroh identity and peers, discovery, pairing, sessions, stream transport |
| `legato-macos` | macOS display enumeration, event-tap capture, injection, permissions, Caps Lock |
| `legato-windows` | Windows display enumeration, hooks, injection, drag capture, Caps Lock |
| `legato-screen` | Mac virtual display/capture/H.264 encoding; Windows decoding; cursor classification |
| `legato-engine` | Shared application engine, settings, input routing, clipboard, files, streaming lifecycle |
| `legato-app` | Iced/iced-m3 tray UI, arrangement editor, Windows viewer and crash recovery |
| `legato-cli` | Pairing, layout, diagnostics, and sharing from the terminal |

The app runs Iced/AppKit on its main thread and bridges to the engine's Tokio
runtime. Native capture has its own event-tap or Windows hook/message-loop thread.
Capture runs the pure controller synchronously so it can immediately swallow or
pass each OS event. Injection and clipboard work are kept off the UI loop.
The app and CLI use the same engine and state-directory lock; quit the tray app
before invoking a CLI command that starts the engine, including `doctor`.

Physical displays are re-enumerated once a second outside the input loop, even
when sharing is stopped. The Mac virtual display is excluded from the physical
shared desk. Changes update the UI, peers, and capture geometry.

## Pointer and held input

There is one shared pointer position. The computer hosting the physical screen
shows its own native cursor. The Windows viewer therefore shows the Windows
cursor and omits the Mac cursor from captured video.

Input positions use the target machine's native global coordinates: macOS points
or Windows physical pixels. Layout uses desk units for placement and speed
scaling. `Screens::native_per_desk` relates these units. Pointer reports describe
the sender's native position; placement and virtual-display messages each specify
their own coordinate spaces in `legato-proto`.

The controller tracks a remote position while local input drives another machine.
On Mac, parking/return warps contaminate the next event's delta; only that first
motion uses location correction. Real trackpads can move the hidden cursor even
when their events are dropped, so later motion must use per-event deltas.
Windows capture derives motion relative to its parked cursor. Pointer reports
keep a machine that yielded control aligned for the next takeover and also align
the viewer when the Mac's own trackpad moves over its extra display.

Keys use physical USB HID usages. The default Windows-to-Mac map sends Alt as
Command and Win as Option. The receiver repeats held keys at its own configured
rate. `Leave`, local takeover, or disconnect releases remote keys/buttons. The
controller then drops their old routes and swallows subsequent physical releases,
so they do not become unmatched releases on another machine. Drag transitions
that remain within one Mac visit retain the held button.

`Enter` carries a position and sequence baseline, without held modifiers or lock
state. Caps Lock is synchronized separately using change clocks and an endpoint-ID
tie-breaker. Num Lock and Scroll Lock are not shared. Automatic Secure Input
switching prevention and a warning are not implemented; password-field behavior
remains a manual validation gap. Windows elevated windows and the secure desktop
also have injection restrictions; no SYSTEM service is provided.

## Connections and protocol

Unpaired devices are discovered through LAN mDNS. Both users compare and accept a
six-digit code derived from the live TLS session using the label
`legato pairing code v1`. Pairing requires listening mode, allows one concurrent
attempt, and has timeouts; it has no attempt-rate limiter. Paired devices reconnect
by endpoint identity using mDNS, DNS/pkarr, and iroh relays, with direct-path
upgrades where possible. Traffic is end-to-end encrypted.

The session ALPN is `legato/1`; pairing uses `legato/pair/1`. Incompatible changes
bump `PROTOCOL_VERSION` (currently 13), and the handshake rejects mismatches. The
ALPN suffix does not change with each protocol bump. Use the same application
build on both machines for hardware validation.

| Transport | Contents |
| --- | --- |
| One reliable bidirectional control stream | Hello, input except motion, placement/screens, control policy, Caps Lock, extra-display lifecycle and cursor shapes |
| Unreliable datagrams | Absolute `Motion` and shared-pointer `Pointer` reports with sequence numbers |
| Unidirectional streams | Clipboard text/PNG blobs and one ordered H.264 stream per video track |
| Bidirectional streams per file/directory | Header, file bytes if present, and acknowledgment after writing/flushing or creating a directory |

Control stream priority is 0, moving video −1, sharp video −2, and bulk transfers
−3. QUIC keep-alive is one second and the idle timeout is five seconds. There is
no application-level motion-acknowledgment or ping/pong protocol, transfer resume,
or file-content hash field. See `legato-net/src/session.rs` for framing and
`legato-proto/src/lib.rs` for the wire contract.

## Clipboard and files

The engine polls native clipboard change counters and reads supported content
with arboard. Copied text and PNG images are pushed to connected peers while
sharing, with echo suppression. It does not wait for a pointer switch, send
clipboard offers, or transmit HTML. Copied files use the file-transfer path and
become pasteable only after the complete batch arrives.

Explicit sends and Windows edge drags save into `Downloads/Legato`; clipboard
files use a temporary cache. Folder scans preserve empty directories and internal
links but reject dangling links, ancestor cycles, and nested links outside the
selection. An explicit link selection selects its target. Name reservation avoids
truncating existing destinations. Failed/cancelled batches remove partial files
while preserving completed files. Finder drag-out and native drops at the remote
pointer are not implemented.

See [configuration and diagnostics](configuration.md) for paths and limits, and
[TESTING.md](../TESTING.md) for regression and hardware checks.

## Extra Mac display

The Mac creates a virtual display through an isolated private CoreGraphics API,
captures with ScreenCaptureKit, and encodes H.264 using VideoToolbox. A single slot
retains only the newest picture waiting for the encoder. Per-track bounded queues
apply network backpressure and recover with a keyframe when needed.

Adaptive quality uses independent sharp and moving video tracks. The Windows
viewer decodes and displays the newest timestamp across both tracks. Media
Foundation first tests D3D11 hardware decoding with a committed Mac color fixture;
setup failure uses software, and a live failure requests a keyframe and keeps
that track in software for the session. NV12 GPU readback and CPU copying are
included in decode timings; frames are uploaded to the viewer's shader afterward.

The display refresh rate and stream frame budget are separate. The former follows
the user's choice; the host limits encoding by stream size. Resolution changes
validate logical points, backing pixels, and refresh, selecting a matching mode
when necessary. Historical user recovery does not establish the underlying cause;
repeat the Windows-options sequence on a release candidate.

The pointer over an uncovered viewer picture forms a portal into the Mac. A
viewer-generation token rejects stale state after reopen. Cursor shape is sampled
on AppKit's main thread at 20 Hz while streaming, sent reliably when it changes,
and mapped to stock Windows cursors. Custom/unavailable shapes fall back to arrow.
No cursor bitmap is transmitted. The complete two-machine cursor interaction and
physical-GPU performance still need hardware checks.
