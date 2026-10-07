# Live latency overlay

With **Show FPS Overlay** on, the overlay adds TackleCast's latency, for example `1920x1080 | 60.0 FPS | 14.2 ms`. Turn on **Detailed Overlay** to see the breakdown:

| Stage | From → to | What changes it |
|---|---|---|
| decode | frame read from the capture card → decoded | MJPEG (GPU or software decode) vs uncompressed |
| queue | decoded → picked up by the renderer | presentation mode (swapchain and GPU-queue waits), window events |
| render | picked up → presented | artifact reduction, RTX Super Resolution, scaling filter |
| vblank ~ | presented → next display refresh | refresh rate; this one is an estimate |

The figures are averaged over the last half second and update four times a second, so you can toggle an option and watch the number settle.

**What it can't see:** controller → console → HDMI → the capture card's encoder and USB transfer happen before the first timestamp, and they are usually the larger part. Measuring them needs a camera or a photodiode. In windowed mode, DWM composition can add one more refresh after the vblank estimate; F11 fullscreen with independent flip avoids that. Use the overlay to compare TackleCast settings, not as a full input-lag figure.

## nvJPEG backend selection (decode stage)

For MJPEG, nvJPEG can do the Huffman (entropy) stage of decoding on the CPU, its default, or on the GPU (`NVJPEG_BACKEND_GPU_HYBRID`). Which is faster for one frame depends on the CPU, the GPU and the resolution. During the first ~1.5 s of capture, TackleCast therefore alternates the two frame by frame, skips 4 warm-up frames each, and keeps the backend with the lower median decode time. The log records the result, for example `nvJPEG backend selected: GpuHybrid (median decode 4.61 ms default, 2.30 ms GPU-Huffman)`. If the GPU backend is missing or rejects the stream, the default is used.
