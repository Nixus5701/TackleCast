# Experimental RTX Super Resolution integration

Status: source implementation; **not compiled or tested on Windows/NVIDIA hardware** in the development environment. This is not a ready-to-run Windows binary. Driver activation, image correctness, resource synchronization and end-to-end latency require the Windows checks below before calling this a working release.

Based on SaltedByte/TackleCast commit `3e0e198bb810b2e88574b4d0e16ce0b867f3a5ea` (version 2.1.0).

## Build and use

Use the prerequisites in `BUILD.md`: Rust x64 MSVC, Visual Studio C++ Build Tools with a Windows SDK, LLVM/libclang, and an FFmpeg 8 shared **development** distribution containing headers, libraries and DLLs. Existing TackleCast runtime DLLs alone are not enough to compile FFmpeg bindings.

From PowerShell in the extracted source folder:

```powershell
.\Build-RTX.cmd -FFmpegDir C:\ffmpeg -RuntimeDirectory C:\Path\To\OriginalTackleCast -RunTests
```

The runtime-directory argument is optional. It copies the original package's CUDA/nvJPEG DLLs to the new build folder. The script copies FFmpeg DLLs from the development distribution used to compile, creates `dist\TackleCast-RTX\TackleCast.exe`, and leaves the original installation untouched. Missing CUDA/nvJPEG DLLs cause upstream TackleCast to fall back to CPU decoding, which can affect latency. No NVIDIA Video Effects SDK installation is needed by this integration.

Enable Super resolution in NVIDIA Control Panel or NVIDIA App. Open the new executable and use **Esc → Video → Request Super Resolution (NVIDIA RTX)**. Apply the settings. Use fullscreen for a 1080p capture on a 1080p display; a smaller video viewport bypasses the effect. Quality is controlled in NVIDIA settings.

The setting defaults to off, including for existing settings files. Disabling it restores the normal rendering path. If the bridge fails, normal video remains available; the settings panel shows the failure. To retry after fixing the driver or configuration, apply Off, then apply On.

“Request submitted — verify in NVIDIA App” confirms that the driver's extension request was accepted and a video-processor blit was submitted. It **does not prove** that NVIDIA's AI model is active. Check the driver's Active indicator with the video visibly playing. Do not use that label, FPS, or GPU usage as proof of VSR activation.

## Implementation

- The existing capture, nvJPEG decoding, CUDA/DX12 plane buffers, audio and latest-frame delivery stay in place.
- The optional pass converts the latest YUV planes to full-range BGRA at source resolution using the existing video shader.
- A D3D12 buffer-to-texture GPU copy feeds a shared D3D11 texture on the **same adapter**.
- One D3D11 video processor converts BGRA to BT.709 studio-range NV12. A second requests NVIDIA PPE Super Resolution, then produces full-range BGRA at the video viewport size.
- The result returns through another GPU buffer copy to a normal wgpu texture. Letterboxing and UI are composited afterward, so the UI is not AI-processed.
- There is no desktop/window capture, CPU frame readback, video encoder, NVIDIA proprietary DLL bundle, or frame interpolation.
- There are additional GPU copies, color conversion and cross-API synchronization. This is GPU-only, but it is **not zero-overhead**. No latency improvement over MPC-BE, or preservation of TackleCast's original latency, has been measured.
- Source frames are limited to even dimensions, 360–1440 lines, maximum width 2560; output is at least source size and at most 8192 in each dimension. Limits are local eligibility rules, not a promise of driver support. Same-resolution cleanup is requested as well as enlargement.
- The RGB/NV12 round trip reduces chroma to 4:2:0. Check fine colored text and level ramps; the effect may not improve every image.

### Synchronization and ownership

The render thread is the only thread submitting wgpu/native render work. Native D3D12 copies use wgpu's own direct queue. Buffer resources are application-owned, wrapped with `create_buffer_from_hal`, and initialized by wgpu before native writes. D3D12 buffers decay to COMMON across ExecuteCommandLists submissions. Shared textures explicitly return to COMMON at each D3D11/D3D12 handoff.

Each frame orders operations as: wgpu input pass/copy → native D3D12 input copy and fence signal → D3D11 GPU wait and two video blits → D3D11 signal/flush → D3D12 GPU wait and output copy → wgpu output copy and composition. Each API retains its resource references. Three immutable pairs of copy command lists are recycled only after their completion fence is reached. If all are busy, that frame uses the original image instead of waiting on the CPU. Completed enhanced frames can be reused for UI-only redraws.

Only teardown/size changes wait on the CPU, with a five-second limit. If a driver stalls and completion cannot be established, the native bridge deliberately retains its resources until process exit rather than freeing GPU-in-use memory. Device removal allows normal cleanup. This is a recovery mechanism, not a latency optimization.

## Validation required on Windows

1. Run `cargo test --locked --target x86_64-pc-windows-msvc` and `cargo build --release --locked --target x86_64-pc-windows-msvc`. Also build with `--no-default-features --features gpu-decode` to check the original path without the native bridge.
2. Run `TackleCast.exe --test`. Verify orientation, aspect ratio, black/white levels and color bars before and after enabling the request. Check both equal-size output and enlargement.
3. Use the UGREEN 25854 at MJPEG 1920×1080 60 fps. Verify actual NVIDIA activation, compare static text and moving content, and compare both on/off on the same monitor.
4. Resize, minimize/restore, toggle fullscreen, move between displays, apply On/Off repeatedly, change capture size, unplug/replug the card and exit during playback. Check for stale frames, crashes, unbounded memory growth and repeated failures in `logs`.
5. For developer validation, enable Windows Graphics Tools/D3D debug validation and run with `TACKLECAST_VSR_DEBUG=1`. Resolve GPU resource/state/fence errors before shipping.
6. Measure end-to-end latency with a high-speed camera showing the source and capture preview together, or a controlled mirrored timer. Compare the original executable, this build with the option off, and this build with it on at the same source mode, monitor refresh, window size and NVIDIA quality. FPS and render-stage timing do not measure capture latency.

Local inspection covered settings wiring, feature gating, error paths and the queue/fence ordering design. This environment could not run Rust tests, compile the C++/Rust Windows code, or validate the shaders/GPU behavior. Those checks remain outstanding.

## API references

- [NVIDIA RTX Video FAQ](https://nvidia.custhelp.com/app/answers/detail/a_id/5448/~/rtx-video-faq)
- [Chromium NVIDIA PPE request ABI](https://chromium.googlesource.com/chromium/src/+/refs/tags/146.0.7676.5/ui/gl/swap_chain_presenter.cc) (`ToggleNvidiaVpSuperResolution`)
- [MPC Video Renderer request/activation distinction](https://github.com/Aleksoid1978/VideoRenderer/wiki/Super-Resolution)
- [Microsoft shared resources](https://learn.microsoft.com/en-us/windows/win32/direct3d12/shared-heaps)
- [Microsoft resource promotion and decay](https://learn.microsoft.com/en-us/windows/win32/direct3d12/using-resource-barriers-to-synchronize-resource-states-in-direct3d-12)

Implementation is under the repository's MIT license. The NVIDIA driver extension's identifier and parameter values describe an external API; no MPC Video Renderer implementation or proprietary NVIDIA runtime is bundled.
