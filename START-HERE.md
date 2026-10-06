For the new tear-free mode: **Esc > Video > Presentation mode > Mailbox (low latency, 1 frame) > Apply**.

This edition includes the capture fallback fix, latest-frame selection after display acquisition, and a Mailbox/Immediate presentation menu. See [docs/LATENCY-UPDATE.md](docs/LATENCY-UPDATE.md).

# TackleCast with an experimental RTX Super Resolution option

This package contains modified source, **not a compiled Windows application**.
The implementation has not been compiled or tested on Windows or an NVIDIA GPU.
It needs that validation before it can be considered a working release.

## What was added

- **Esc → Video → Request Super Resolution (NVIDIA RTX)**, saved in settings and off by default.
- A GPU-only bridge from TackleCast's renderer to NVIDIA's D3D11 video-processor extension.
- Normal rendering when disabled, unsupported, or when processing fails.
- A status message that distinguishes an accepted request from confirmed NVIDIA activation.

The RTX 3070 Ti is the intended GPU for testing. This does not remove compression
from the capture card's output; it asks the driver to enhance the decoded image.
Additional GPU work is required. Its latency has not been measured.

## Build on Windows

1. Extract the entire archive.
2. Install the build prerequisites listed in `BUILD.md`: Rust MSVC, Visual Studio
   C++ Build Tools and Windows SDK, LLVM/libclang, and FFmpeg 8 shared development
   files (headers, libraries and DLLs).
3. Open PowerShell in this folder and run the following, replacing both paths:

```powershell
.\Build-RTX.cmd -FFmpegDir C:\ffmpeg -RuntimeDirectory C:\Path\To\OriginalTackleCast -RunTests
```

`-RuntimeDirectory` is optional; it copies the original application's CUDA/nvJPEG
DLLs so GPU decoding can remain available. It does not change that installation.

The resulting program is `dist\TackleCast-RTX\TackleCast.exe`.
Enable RTX Video Super Resolution in NVIDIA settings, open the new program,
enable its new Video setting, and apply. Use a video viewport at least as large
as the capture resolution. NVIDIA's Active indicator is the activation check.

If compilation fails, retain the first compiler error. If processing fails,
retain the status message and application log. Neither failure should be treated
as evidence that this implementation has been validated.

See `docs/RTX-SUPER-RESOLUTION.md` for implementation details, limitations and the
Windows test procedure. `changes.patch` contains the changes against upstream
commit `3e0e198bb810b2e88574b4d0e16ce0b867f3a5ea`.
