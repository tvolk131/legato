# Working on Legato

This is the active checkout. Development moved from the old `synergy-rs` checkout
on October 9, 2026; old copies are reference material.

Start with the [current architecture](docs/architecture.md),
[testing guide](TESTING.md), and [0.3.0 release plan](docs/releases/0.3.0-plan.md).
[RESEARCH.md](RESEARCH.md) contains dated research and later investigation notes;
the [handoff](docs/handoff/README.md) preserves historical hardware evidence.
Check current Git/GitHub state before acting on any dated status or checklist.

## Project decisions

- Legato shares a keyboard, mouse, clipboard, and files between macOS and Windows.
  It also hosts an extra Mac display in a Windows viewer.
- Support macOS 26+ on Apple silicon and Windows 11 on x64 and ARM64. Linux,
  Intel Macs, Windows 10, and Windows-display-on-Mac streaming are out of scope.
- Keep the MIT license. Do not copy GPL/AGPL code or add GPL/AGPL dependencies.
- Pair through automatic LAN discovery; paired devices must also work across
  networks using iroh. Keep both follow-input and designated-controller policies.
- Preserve one shared pointer position. The visible pointer belongs to the
  computer physically hosting the screen, including the Windows viewer. Either
  machine's input can take over without jumping; keys and clicks follow it.
- The primary setup is three landscape 4K Windows monitors side by side, with a
  MacBook below the middle monitor. Both keyboards are US English; Alt maps to
  Command and Win to Option by default when driving the Mac.
- Preserve smooth animations. The user declined a frame-rate cap to fix idle CPU;
  moving continuous discovery animation into the Add a device dialog solved it.
- Keep useful diagnostics when simplifying code.

## Workflow and verification

Use branches and a PR per meaningful change. The user previously authorized
merging after all required CI checks pass and publishing alpha/beta GitHub releases
for testing. Verify the current PR head and its checks; never merge failing CI.
A documentation, migration, or context-reading task alone does not request a new
release. The release checklist itself does not authorize a final release.

Use these checks as appropriate for the change:

```sh
cargo fmt --all --check
RUSTFLAGS='-D warnings' cargo clippy --locked --workspace --all-targets
RUSTFLAGS='-D warnings' cargo test --locked --workspace
```

CI runs Clippy and tests on macOS, Windows x64, and Windows ARM64 with warnings
denied. Its Rust 1.99 job type-checks the workspace on macOS. Documentation PRs
also run the full CI matrix. See [TESTING.md](TESTING.md) for opt-in tests and
Windows cross-checks; normal tests need desktop enumeration/AppKit access on Mac.
Ignored tests can move the cursor, type, change Caps Lock or the clipboard, and
create displays. Check their side effects and permissions before running them on
the user's desktop.

Do not claim a reported bug is fixed until the exact reported sequence is
reproduced and passes with the change, or the user confirms it on their setup.
Distinguish a plausible fix from a verified result. Synthetic input and real
trackpads can behave differently. Screen capture and GPU decoding need real
hardware checks that hosted CI cannot replace. There is no self-hosted Windows
runner; the user declined one. Use hosted CI and manual testing on their PC.

## Implementation and release invariants

- Bump `PROTOCOL_VERSION` in `legato-proto` on incompatible wire changes. Postcard
  cannot decode unknown enum variants; mismatched peers refuse to connect. Test
  both machines with the same candidate when verifying a change.
- Preserve native pointer visibility, first-motion-after-warp handling, Windows
  raw-keyboard registration, and viewer redraw behavior when changing input.
- The app's Clippy configuration bans `std::process::Command::new`; prefer native
  APIs to avoid console flashes and repeated process startup on Windows.
- Release tags must match every workspace package version in `Cargo.toml` and
  `Cargo.lock`. Release bodies come from `docs/releases/<tag>.md`. Preserve
  historical release notes and the bundled third-party notices under `licenses/`.
- The current macOS release workflow requires Developer ID signing and
  notarization; see [the signing guide](docs/macos-signing.md). Older alphas were
  ad-hoc signed. Permission retention across signed upgrades remains a hardware
  gate. Permissions granted to a different responsible process do not imply this
  app has access. Check before relying on Accessibility, Input Monitoring, or
  Screen Recording; do not reset permissions during ordinary source work.
