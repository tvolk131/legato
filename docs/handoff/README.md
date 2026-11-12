# Historical development handoff

This preserves observations from the October 9, 2026 checkout migration and
subsequent cursor investigation. It is historical evidence, not an installed
version inventory or a work queue. Temporary package paths, transcript identifiers,
and superseded next-step commands have been omitted. The original checkout was
named `synergy-rs`; development moved to `legato` and no unpublished source changes
were found in the copied checkout during migration.

Use the [current architecture](../architecture.md),
[testing guide](../../TESTING.md), [signing guide](../macos-signing.md), and
[0.3.0 plan](../releases/0.3.0-plan.md) for ongoing work. In particular, hardware
decoding and cursor shapes have shipped, Developer ID signing is implemented,
and today's protocol differs from the October 9 builds.

## Real hardware observations

These came from user-supplied screenshots/logs or the user's confirmation, except
the idle-CPU measurements recorded by the earlier development session.

| Observation | Evidence and limit |
| --- | --- |
| Alpha.26, first full-4K session on October 9 | F10 showed 3840×2160, 40 fps, 0.3 Mbit/s and 42 ms total: Mac 22, network 3, decode 13, display 4. The earlier screenshot showed about 107 ms total, including Mac 84. These were not controlled identical-motion benchmarks. |
| Mac capture/encode after alpha.26 | Five-second windows averaged 1–4 ms capture delay, 0–6 ms encoder wait and 17–18 ms encode time. The earlier sustained five-frame delay was absent from these samples. Some superseded captures were skipped. Sustained maximum throughput was not established. |
| Stuck-4K report after alpha.28 | The user said “Fixed it!” after installation and restoring permissions. At 20:41 UTC the Mac log showed full 3840×2160 streaming configured for 60 fps. The cause remains unproven: that successful session logged no explicit mode selection. |
| RTX 3070 hardware decoding | The same session's Windows log passed its hardware self-check and selected `D3D11 hardware on NVIDIA GeForce RTX 3070 (NV12 readback)`, without fallback through the supplied log's end at 20:50:44 UTC. Hardware decoding is confirmed for that session. |
| Alpha.28 F10 comparison | 43 ms total: Mac 21, network 3, decode 14, display 5, at 40 fps and 0.2 Mbit/s. The 14 ms decode figure showed no speedup over the earlier 13 ms software sample. Decode includes transform processing and NV12 readback/copy. |
| Busy Mac capture windows | 283–285 pictures per five seconds, with roughly 17–18 ms encode time and no old five-frame backlog. Zero skipped in one F10 screenshot described only that sampled viewer window. |
| Idle CPU after discovery moved into Add a device | The earlier session measured 0.1–1.8% on alpha.24 versus 14–25% with the continuous discovery spinner on alpha.20. The user declined a UI frame-rate cap; preserve smooth animations. |
| Cursor probe, October 13 | A background probe on macOS 26.6.2 observed all 12 controlled cursor shapes on the virtual display. This supports the Mac reader, without verifying the complete Windows interaction. |

The stuck-mode recovery was user-confirmed; it was not a controlled reproduction
of the reported Windows-options sequence. A forced-low-DPI CLI experiment failed
before explicit selection, but both old and new code recovered with an AppKit
event loop. Keep the repeated resolution/reopen checks on the current candidate.
Alpha.29 added per-stage Windows timing logs; no real RTX 3070 per-stage results
were recorded here. Profile those stages before attributing decode time to GPU
execution versus readback/synchronization.

## Remaining reports and lessons

- A Windows wake crash contained DX12 `ResizeBuffers` / `E_OUTOFMEMORY` followed
  by a wgpu panic. Restart resilience and logging do not establish its cause.
- A phone-hotspot connectivity report had no established cause or reproducible
  sequence. Retain it as historical context; it is not a diagnosed current bug.
- Finder drag-out and custom cursor images remain outside the current feature set.
- Real trackpads can move a hidden cursor even when event taps drop their events.
  Synthetic input cannot prove that behavior is correct on physical devices.
- Ad-hoc signed Mac upgrades needed fresh permission grants. Permissions belonging
  to the earlier assistant's responsible process do not imply another process
  has Accessibility, Input Monitoring, or Screen Recording access.
- Hosted CI exercises Windows input APIs and software GPU rendering. Physical
  capture, GPU decoding, cursor feel, and signed-upgrade permission retention
  require the real Mac/Windows pair. No self-hosted Windows runner is configured.

The recovered standalone cursor-hover scratch probe was removed during the
November documentation cleanup. Production cursor classification and maintained
coverage now live in `legato-screen/src/mac/cursor.rs` and
`legato-screen/tests/mac_cursor_shapes.rs`. Preserve the probe's observations
above without treating private/deprecated API success as a future OS guarantee.

Release chronology and implementation changes remain in the
[versioned release notes](../releases/) and Git history. Current code, exact PR
heads/checks, and new hardware observations take precedence over this snapshot.
