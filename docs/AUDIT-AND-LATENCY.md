# Source audit and tear-free latency reduction

This update fixes defects found in an audit of the RTX latency source package. It also reduces capture-to-display latency without allowing tearing.

## Use it

Esc → Video → Presentation mode → **Mailbox (low latency)**, then press Escape to apply. For the lowest display latency, also use **F11 fullscreen**. A flip-model swapchain covering the whole monitor can use *independent flip*, which skips one DWM composition step. The overlay and menu don't prevent this. To confirm it, PresentMon should report `Hardware: Independent Flip`.

On a G-Sync/FreeSync monitor that is in VRR mode, the display refreshes when each frame arrives. Below the monitor's maximum refresh rate, FIFO-style presentation is then both tear-free and immediate. Mailbox (low latency) behaves well there too.

## Latency changes

| Change | Where the time went before | Effect |
|---|---|---|
| **Newest-packet capture** (`capture.rs`) | FFmpeg's MJPEG decoder is single-threaded in FFmpeg 8, so `decode_threads = 4` had no effect. When decode was slower than the capture rate (software MJPEG at 1440p120 manages about 40 fps), packets piled up in DirectShow's 16 MB real-time buffer and were shown in order. That added hundreds of milliseconds, and the delay stayed until the buffer overflowed. Packets buffered by `avformat_find_stream_info` at startup and any later stall added a backlog the same way. | After each blocking read, packets already queued are drained without waiting, and only the newest is decoded. This applies only to MJPEG and raw video, whose frames are self-contained. Skipped packets are logged as `stale_packets_skipped` in the decode summary. |
| **No frame-threaded decode** | Frame threading holds `threads − 1` frames inside the decoder. This mattered for non-MJPEG codecs such as webcam H.264 at more than 60 fps. | Slice threading plus `AV_CODEC_FLAG_LOW_DELAY`. |
| **Low-latency Mailbox redesigned** (`presentation.rs`, `render.rs`) | The swapchain had 2 buffers (frame latency 1). With one buffer on screen and one queued, rendering a newer capture frame had to wait for the next vblank. This happened always in fullscreen independent flip, and under DWM whenever two frames arrived within one refresh. | The swapchain now has 3 buffers, so a newer frame replaces the queued one at once. A **GPU-queue gate** waits for the previous frame's GPU work before starting the next. This prevents GPU queueing, the job the 1-frame limit used to do, without the vblank wait. When the GPU is the bottleneck, every frame renders the newest capture available when the GPU frees up. |
| **No CPU repacking on upload** (`render.rs`) | The NV12 path, the default at 60 fps and below, copied every plane into padded scratch buffers. `write_texture` doesn't need padding. It also split interleaved UV on the CPU. All of this ran on the render thread *after* the swapchain was acquired. | Planes upload directly. NV12 chroma becomes a single `Rg8Unorm` texture, and the shader reads U and V from one set of taps, which also halves the chroma texture fetches. The output is bit-identical to the old shader for every filter and scale. This was checked on a Vulkan software rasterizer. |
| **No per-frame allocation** (`capture.rs`) | Software decode allocated three new plane `Vec`s per frame. | Planes are written into the reused triple-buffer slot. |
| **MMCSS** (`mmcss.rs`) | The capture and render threads had at most `ABOVE_NORMAL` priority. | The capture thread is registered as MMCSS "Capture" and the render/event thread as "Games", which reduces scheduling delay on a busy desktop. |
| **Audio drift cap** (`audio.rs`) | The capture card and the speakers run on separate clocks. Any drift, plus the input starting before the output, built up in the 1-second ring buffer as permanent audio delay. | More than 40 ms of buffered audio is trimmed to 20 ms, in whole frames. |

All Mailbox options are tear-free. "Mailbox (default)" previously fell back to **Immediate**, which tears, when Mailbox was unavailable. Now both Mailbox options fall back to FIFO VSync. Only the explicit Immediate option can tear.

## Bugs fixed

1. **Hang and busy-spin when a device is unplugged.** `input.packets()` retries every error except EOF forever. DirectShow returns EIO on every read after removal, so the capture thread spun at 100% CPU and never checked its stop flag again. Changing settings or exiting then froze the app in `CaptureThread::stop`. Reads are now explicit. Repeated failures end the session with a "device lost" message on the overlay, and the app no longer cycles through fallback resolutions, which would have overwritten the saved settings.
2. **Hang on exit or settings change while the source sends no frames** (console off). DirectShow reads block until a frame arrives. `stop()` now waits up to 2.5 s and then detaches the thread instead of freezing the UI.
3. **GPU memory overrun in nvJPEG decode.** Chroma subsampling was logged but never checked, and the dimensions were read only from the first frame. A 4:4:4 JPEG, or one larger than the allocation, made nvJPEG write past the end of the plane buffers. This includes the shared DX12 buffers. A 4:2:0 JPEG rendered garbage. Every frame's header is now validated. Anything that isn't 3-component 4:2:2 within the allocation switches to software decode.
4. **Double `cuMemFree`** when owned-mode reallocation failed partway through. Also fixed: a CUDA context, stream and nvJPEG handle leaked if the first device allocation failed.
5. **Menu and error overlay didn't appear without video.** Only incoming frames triggered redraws. With no device, a capture error or a console that was off, pressing Escape opened an invisible menu, and capture errors never reached the overlay. Escape, egui input while the menu is open or video is idle, and new capture errors now request a redraw.
6. **The Exit button restarted capture and audio** with the menu's settings before quitting. The settings are now saved without being applied.

## Known issues not changed

- **Colour matrix:** both formats use BT.601. HD sources sent as NV12 are usually BT.709, so some colours are slightly shifted. Changing this needs checking against each capture card's actual output.
- **VSR busy frames:** when the RTX bridge reports all three slots busy, the RGB pre-pass for that frame has already been submitted. That GPU work is wasted, and it is rare with the GPU-queue gate.
- **No automatic reconnect** after a device is lost. Reopen the device from the menu.
- **Single NVIDIA GPU assumed:** zero-copy uses CUDA device 0 and doesn't check that it is the adapter wgpu chose. This matters on systems with several NVIDIA GPUs.
- **Negotiated fallback resolutions** other than 720p, 1440p and 4K are saved as "1080p".
- **Shared GPU buffers are always allocated,** including for NV12 capture (60 fps and below), which never uses them. This costs memory only.

## Validation performed

- `cargo check --target x86_64-pc-windows-msvc` passes for the default feature set, `--no-default-features`, and `rtx-vsr` only. It was run on Linux with FFmpeg headers. The native C++ bridge was not compiled because it is unchanged.
- The portable tests pass (22), including new tests for the no-tearing fallback and the frame-latency/gate selection, plus naga validation of both WGSL shaders.
- GPU equivalence test on lavapipe: the old and new video shaders give **bit-identical** output for NV12 and 4:2:2 at scales of 0.5×, 1×, 2× and 3× with all three filters. A neutral grey frame matches the BT.601 limited-range reference.
- The audio ring trim was unit-tested.
- **Not done:** no run on Windows, a capture card or an NVIDIA GPU, and no end-to-end latency measurement. To compare builds, use a high-speed camera filming the source and the preview together. Keep format, resolution, refresh rate, window or fullscreen mode, and VSR identical between runs.
