# Image controls and window icon update

Extract the entire Windows ZIP into a new folder and run TackleCast.exe. To keep your prior preferences, copy tacklecast_settings.json from the previous folder while TackleCast is closed. Existing settings files receive neutral defaults for the new controls.

Press Escape to open Settings. In VIDEO, VSR sharpness is directly below Request Super Resolution. Expand Image adjustments for the color controls. The Esc menu has slightly more vertical spacing and 24 additional points of available scroll height. Scroll the menu if needed. Adjustments preview live; Escape, clicking outside Settings, or exiting saves them. Reset image adjustments restores all six controls, including VSR sharpness.

| Control | Range | Default |
| --- | --- | --- |
| VSR sharpness | 0–100% | 50% (previous appearance, unchanged) |
| Brightness | -100 to +100 | 0 |
| Contrast | 0–200% | 100% |
| Saturation | 0–200% | 100% |
| Hue | -180 to +180 degrees | 0 degrees |
| Gamma | 0.25–3.00 | 1.00 |

VSR sharpness is a post-processing filter on the enhanced image, not an NVIDIA driver sharpness parameter or a change to VSR model strength. Below 50% blends toward a small Gaussian blur; above 50% applies an unsharp mask. Start at 30–40% if edges look too crisp. At 50%, neighborhood filtering is bypassed. The control is available when Request Super Resolution is checked and affects only frames rendered through the VSR path. If VSR falls back to original video, the sharpness filter does not run. NVIDIA may still decline the VSR request; this control does not prove the driver activated VSR.

Brightness, contrast, saturation, hue, and gamma work with VSR on or off, including original-frame fallback. They affect only video, not menus, the overlay, or letterboxing. Gamma values above 1 brighten midtones. All color controls run after VSR so they neither alter its input nor apply twice. Neutral settings bypass color adjustment and preserve the prior picture.

The controls use the existing final fragment shader, with no extra render pass, frame queue, or CPU readback. Non-neutral controls add GPU work, especially sharpness; performance and end-to-end latency have not been measured on an RTX 3070 Ti. The prior fallback, freshest-frame selection, and all three presentation modes remain in this build.

The follow-up icon fix loads assets/icon.ico through the native Windows ICO loader, using a path relative to TackleCast.exe. It assigns separate title-bar and taskbar icon sizes and sets the Windows taskbar icon resource property before displaying the window. The same embedded ICO resource is a fallback when the assets file cannot be loaded. See ICON-FIX.md.

Validation: Windows x64 release build succeeded; 20 portable tests passed, covering settings migration/persistence, invalid control values, both complete WGSL shaders and their uniform layout, icon decoding, and existing latency/VSR checks. No physical capture-card or NVIDIA runtime test was possible in the build environment.

image-adjustments.patch records this update relative to the prior low-latency Mailbox source archive. Earlier patch files retain their original baselines.
