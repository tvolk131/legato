# Legato

Use one keyboard and mouse across your computers over the network. Your cursor moves smoothly from one machine's screens to the next, and the clipboard and files come along with it.

**Status: pre-alpha, research and spikes only.** Nothing usable yet.

- **Platforms:** macOS 26+ (Apple silicon) and Windows 11 (x64 and ARM64).
- **Written in Rust:** [iced](https://github.com/iced-rs/iced) + [iced-m3](https://github.com/tvolk131/iced-m3) for the UI, [iroh](https://github.com/n0-computer/iroh) for encrypted, peer-to-peer networking.
- **Zero-config pairing:** nearby devices are discovered automatically on your local network and confirmed with a 6-digit code. After pairing, devices connect whether they're on the same network or not.
- **Planned:** a virtual-monitor mode that extends a Mac onto a Windows PC as an extra display.

## Docs

- [RESEARCH.md](RESEARCH.md): feasibility research, architecture and decisions
- [TESTING.md](TESTING.md): testing strategy
- [spikes/](spikes/README.md): throwaway prototypes that verified the iroh and iced APIs

## License

[MIT](LICENSE)
