# Hotkeys and colour matrix

## Hotkeys

| Default | Action |
|---|---|
| F6 | Toggle RTX Super Resolution |
| F7 | Toggle MJPEG artifact reduction (keeps the chosen strength) |
| F11 | Fullscreen (fixed) |
| Esc | Settings menu (fixed) |

Hotkeys work while the menu is closed, and each press shows a brief on-screen confirmation. To rebind, open **Esc → Hotkeys**, click a binding and press the new key; Esc cancels. Binding a key that the other action already uses swaps the two. Bindings use the physical key position, so they don't change with the keyboard layout. They are saved in `tacklecast_settings.json`.

## Colour matrix

**Esc → Video → Colour matrix**, with live preview:

- **Auto** (default): BT.709 for 720p and above, BT.601 below. This is the convention HDMI sources and video players follow when no matrix is signalled.
- **BT.709 (HD):** what HD consoles send.
- **BT.601 (SD):** what every previous build used for all sources.

Decoding BT.709 video with BT.601 makes greens brighter and yellower and reds slightly duller. For example, a grass green of RGB (64, 153, 51) displayed as (72, 169, 55). This was checked on the GPU shader: BT.709 content decoded with BT.709 reproduces the source colour within 1 level. Capture cards don't always follow the convention, so if colours look wrong with Auto, compare the explicit options. The setting also applies to the image fed into RTX Super Resolution.
