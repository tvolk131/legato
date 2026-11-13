# Legato

Use one keyboard and mouse across your computers over the network. Your cursor moves smoothly from one machine's screens to the next, and the clipboard and files come along with it.

**Status: alpha.** Either machine's keyboard and mouse can drive the other, with a tray app for pairing, arranging screens and settings. Copy and paste works across machines (text, images and files), and files can be sent or dragged across the edge from Windows. A Mac can also appear as an extra display in a window on a Windows PC. Downloads are on the [releases page](https://github.com/tvolk131/legato/releases).

- **Platforms:** macOS 26+ (Apple silicon) and Windows 11 (x64 and ARM64).
- **Written in Rust:** [iroh](https://github.com/n0-computer/iroh) for encrypted, peer-to-peer networking. The Mac app is SwiftUI; the Windows app uses [iced](https://github.com/iced-rs/iced) + [iced-m3](https://github.com/tvolk131/iced-m3).
- **Pairing:** nearby devices are discovered automatically on your local network and confirmed with a 6-digit code. After pairing, devices can connect across networks.
- **Virtual monitor mode:** “Show as display” next to a Mac in the Windows app adds a display to the Mac and shows it in a window, or full screen with F11. Move the Windows pointer over it to use it; F10 toggles stream statistics.

## Quick start

1. Download matching releases for both machines; their protocol versions must agree. Choose `macos-arm64` for Apple silicon and `windows-x64` or `windows-arm64` for the PC.
2. Extract the Mac archive, copy `Legato.app` to Applications, and open it. Extract the Windows ZIP and run `Legato.exe`. These are portable archives; there is no Windows installer.
3. On Mac, grant **Accessibility** and **Input Monitoring** to the app. Hosting the extra display also needs **Screen Recording**. Allow **Local Network** access when prompted and allow Windows firewall access for the network where you pair. Quit/reopen after changing permissions if sharing remains stopped.
4. Open **Legato** on both machines. On one machine, choose **Add a device…** and pick the other machine, then compare and accept the same 6-digit code on both. Drag the other machine's screens to where they sit in the arrangement editor and start sharing.

Push the cursor across the shared edge, and keep pushing a little past it. In the default follow-input mode, using the other machine's own trackpad or keyboard takes control there.

Published alpha packages through alpha.30 used ad-hoc Mac signatures and may need fresh permission grants after an upgrade. The current release workflow requires Developer ID signing and notarization for new packages; signed-upgrade permission retention still needs hardware validation. See the [signing guide](https://github.com/tvolk131/legato/blob/main/docs/macos-signing.md).

Finder drag-out, custom cursor images, and Windows secure-desktop control are outside the current feature set. Automatic prevention of switching during macOS Secure Input is not implemented; password-field behavior remains a validation gap.

## Command line and source builds

The Mac archive includes `legato`; the Windows archive keeps the CLI at `cli/legato.exe` so its name does not conflict with the app. Add that directory to PATH or invoke its executable by path.

Quit the tray app on both machines before using the CLI, including `doctor`. Closing the app window leaves it running; the app and CLI share a state-directory lock.

On both machines:

```sh
legato pair      # compare and accept the code on both machines
legato doctor   # inspect permissions, devices, and display numbers
```

Then, on Windows, place the Mac below the appropriate Windows display. Replace the device name and display number with the values from `doctor`:

```sh
legato layout "MacBook" --side below --display 2
```

Finally, on both machines:

```sh
legato run      # start sharing; Ctrl+C to stop
```

To build the app and CLI from source on a supported OS, install Rust 1.99+ and native build tools (Xcode command-line tools on Mac; MSVC C++ tools and Windows SDK on Windows), then run:

```sh
cargo build --locked -p legato-cli
cargo build --locked -p legato-app   # the Windows app
native/macos/build-app.sh            # the Mac app; needs Swift 6.2+ and the macOS 26 SDK
```

The executables are under `target/debug/`; the Mac app is `target/swiftui/Legato.app` (see [native/macos](https://github.com/tvolk131/legato/blob/main/native/macos/README.md)). Development builds have their own permission context. See [TESTING.md](https://github.com/tvolk131/legato/blob/main/TESTING.md) for checks and opt-in hardware tests.

## Diagnostics and documentation

App logs are `legato-app.log` and `legato-app.previous.log` in the per-user state directory. `legato doctor` prints diagnostics; `LEGATO_LOG` controls verbosity. Paths, configuration, file destinations, and limits are in the [configuration guide](https://github.com/tvolk131/legato/blob/main/docs/configuration.md).

These online links also work from the README included in downloaded packages:

- [Current architecture](https://github.com/tvolk131/legato/blob/main/docs/architecture.md): crate map, input routing, protocol and streaming
- [Testing guide](https://github.com/tvolk131/legato/blob/main/TESTING.md): current CI, local checks and hardware verification
- [Research and investigations](https://github.com/tvolk131/legato/blob/main/RESEARCH.md): historical proposals and later findings
- [macOS release signing](https://github.com/tvolk131/legato/blob/main/docs/macos-signing.md): credentials, notarization and test builds
- [0.3.0 release plan](https://github.com/tvolk131/legato/blob/main/docs/releases/0.3.0-plan.md): release candidates and remaining validation

## License

[MIT](LICENSE)
