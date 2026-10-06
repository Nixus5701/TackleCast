# Capture latency update

Extract the full Windows ZIP into a new folder and run TackleCast.exe. Open Esc > Video and choose a Presentation mode, then press Escape to close Settings and save. The setting is saved across restarts.

- Mailbox (default) keeps the frame-latency limit of 2. It now falls back to FIFO VSync, never to Immediate, when Mailbox is unavailable.
- Mailbox (low latency) — formerly "Mailbox (low latency, 1 frame)" — uses Mailbox with three swapchain buffers plus a GPU-queue gate (see [AUDIT-AND-LATENCY.md](AUDIT-AND-LATENCY.md); the earlier two-buffer version could wait for vblank before rendering a newer frame). It remains tear-free. If Mailbox is unsupported, this option falls back to FIFO VSync, never Immediate. It can be used with VSR enabled or disabled. The improvement has not been measured on the capture setup.
- Immediate requests presentation with tearing permitted and a frame-latency limit of 1. If unsupported, the renderer uses Mailbox, then AutoVsync. Startup and mode-change logs show both requested and active modes.

All modes choose the latest available capture frame after acquiring the display buffer. This prevents a frame chosen before a display wait from being unnecessarily stale when rendering starts. VSR processes the newly selected frame as before.

Capture format negotiation now retains shared CUDA/DX12 handles through failed opening attempts. NV12-to-MJPEG fallback can use shared GPU storage when the actual capture dimensions match. Resolution fallback with incompatible storage uses the owned decode path. The shared decoder validates JPEG dimensions against its actual allocation, replacing the old hardcoded size assumption.

Shared capture storage is retained until both CPU frame readers and the GPU upload copy finish. Four reusable storage slots accommodate ownership without making a four-frame FIFO. When no slot is available, the decoder skips that packet rather than overwriting an in-use buffer or adding a CPU wait. The existing CUDA completion wait remains necessary.

## Validation

Windows x64 release compilation passed. Seventeen portable tests passed, including settings persistence/defaults, presentation-mode fallback, GPU storage ownership, existing triple-buffer newest-frame behavior, VSR geometry, and WGSL shader validation. The prior Windows unit-test executable compiled. This update was checked with the portable tests and the Windows release build. No physical capture-card, NVIDIA runtime, tearing, or end-to-end latency measurements were performed. Improvements are implementation changes, not a measured latency guarantee.

For an A/B comparison, keep capture format/resolution/FPS, window size, monitor refresh and VSR settings identical. Compare Mailbox, Mailbox (low latency, 1 frame), and Immediate. The log entry `gpu_decode=true, zero_copy=true` identifies active shared capture storage; `presentation requested=... active=... maximum_frame_latency=...` identifies the actual display mode.

This build is based on the source ZIP supplied for the latency audit. The latency-changes.patch file contains changes against that supplied baseline; changes.patch retains the prior VSR changes against upstream. See BUILD.md for the Windows toolchain and scripts/build_rtx.ps1 for packaging. Portable checks run with `cargo test --manifest-path tests/portable/Cargo.toml`.
