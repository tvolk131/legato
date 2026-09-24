# Spikes

Throwaway projects from the feasibility research (see `../RESEARCH.md`). They were checked on macOS arm64 only.

- **`iroh-probe/`** targets iroh 1.2.0. It was compiled and run.
  - `cargo run --release`: LAN-only endpoint, allow-list in the `ProtocolHandler`, datagram vs stream round-trip times, and rejection of an unpaired peer. Add `--mdns` to dial by id only, resolved via mDNS.
  - `cargo run --bin pair`: 6-digit pairing code derived from TLS keying material, plus the ticket string.
  - `cargo run --bin hook`: central allow-list enforced in an `EndpointHooks::after_handshake` hook.
  - `cargo run --bin thread`: `send_datagram` called from a plain OS thread, standing in for a capture-hook thread.
  - `cargo run --bin upgrade`: relay → direct path upgrade timing. It uses n0's public relays, so it needs internet.
  - `cargo run --bin hybrid`: `presets::N0` plus mDNS. A peer with mDNS dials by id and gets a direct LAN path. A peer without mDNS dials by id via pkarr and the relay, then upgrades to a direct path. Needs internet.
- **`ui-compile-check/`** only proves that iced 0.14.0, iced-m3 0.1.0-beta.2, tray-icon, arboard, drag and display-info resolve and type-check together. It was never run, and some of it is deliberately naive: the tray is created in `boot`, which is wrong on macOS, and monitor rectangles aren't scaled into canvas space.
