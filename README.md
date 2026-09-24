# Legato

Use one keyboard and mouse across your computers over the network. Your cursor moves smoothly from one machine's screens to the next, and the clipboard and files come along with it.

**Status: alpha.** Either machine's keyboard and mouse can drive the other, with a tray app for pairing, arranging screens and settings. Copy and paste works across machines (text, images and files), and files can be sent or dragged across the edge from Windows. A Mac can also appear as an extra display in a window on a Windows PC. Downloads are on the [releases page](https://github.com/tvolk131/legato/releases).

- **Platforms:** macOS 26+ (Apple silicon) and Windows 11 (x64 and ARM64).
- **Written in Rust:** [iced](https://github.com/iced-rs/iced) + [iced-m3](https://github.com/tvolk131/iced-m3) for the UI, [iroh](https://github.com/n0-computer/iroh) for encrypted, peer-to-peer networking.
- **Zero-config pairing:** nearby devices are discovered automatically on your local network and confirmed with a 6-digit code. After pairing, devices connect whether they're on the same network or not.
- **Virtual monitor mode:** "Show as display" next to a Mac (in the Windows app) adds a display to the Mac and shows it in a window, or full screen with F11. Move the Windows pointer over it to use it.

## Quick start

Open **Legato** on both machines. Each lists the other under "Nearby"; pick it on either side and check that both show the same 6-digit code. Then drag the other machine's screens to where they sit in the arrangement editor.

Or from the command line, on both machines unless noted:

```sh
legato pair      # pick the other machine; both show the same 6-digit code
legato doctor    # (on the machine with the keyboard) displays are numbered left to right
legato layout "Tommy's MacBook Pro" --side below --display 2   # where the other machine sits
legato run       # start sharing; Ctrl+C to stop
```

Push the cursor across the shared edge, and keep pushing a little past it. Touching the other machine's own trackpad or keyboard gives it back.

## Docs

- [RESEARCH.md](RESEARCH.md): feasibility research, architecture and decisions
- [TESTING.md](TESTING.md): testing strategy
- [spikes/](spikes/README.md): throwaway prototypes that verified the iroh and iced APIs

## License

[MIT](LICENSE)
