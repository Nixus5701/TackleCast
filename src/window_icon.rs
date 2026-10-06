//! Native Windows ICO loading and taskbar identity. All paths are EXE-relative.
use std::path::{Path, PathBuf};
use windows::core::{HSTRING, PROPVARIANT};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IPropertyStore, PROPERTYKEY, PSCoerceToCanonicalValue, PSGetPropertyKeyFromName,
    SHGetPropertyStoreForWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, ICON_BIG, ICON_SMALL, WM_GETICON};
use winit::dpi::PhysicalSize;
use winit::platform::windows::{IconExtWindows, WindowExtWindows};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Icon, Window};

fn asset_path(exe: &Path) -> PathBuf {
    exe.parent().unwrap_or_else(|| Path::new(".")).join("assets").join("icon.ico")
}

fn native_icon(path: &Path, side: u32) -> Option<Icon> {
    let size = Some(PhysicalSize::new(side, side));
    // LoadImageW reads the actual ICO and selects its native resolution.
    match Icon::from_path(path, size) {
        Ok(icon) => {
            tracing::info!("Native ICO loaded: {} ({side}px)", path.display());
            Some(icon)
        }
        Err(error) => {
            tracing::warn!("Native ICO load failed: {error}; using embedded icon resource 1");
            Icon::from_resource(1, size)
                .map_err(|e| tracing::error!("Embedded icon load failed: {e}"))
                .ok()
        }
    }
}

pub fn apply(window: &Window, app_id: &str) {
    let Ok(exe) = std::env::current_exe() else {
        tracing::error!("Cannot resolve executable path for window icons");
        return;
    };
    let path = asset_path(&exe);
    let scale = window.scale_factor();
    // winit retains the owned HICONs for the window's lifetime.
    window.set_window_icon(native_icon(&path, (16.0 * scale).round().max(16.0) as u32));
    window.set_taskbar_icon(native_icon(&path, (32.0 * scale).round().max(32.0) as u32));

    let Ok(handle) = window.window_handle() else { return; };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else { return; };
    let hwnd = HWND(handle.hwnd.get() as *mut _);
    // Configure the shell's group/pinned icon as well as the live window icons.
    if let Err(error) = unsafe { set_taskbar_properties(hwnd, &exe, &path, app_id) } {
        tracing::warn!("Taskbar icon properties could not be set: {error}");
    }
    unsafe {
        let small = SendMessageW(hwnd, WM_GETICON, WPARAM(ICON_SMALL as usize), LPARAM(0));
        let big = SendMessageW(hwnd, WM_GETICON, WPARAM(ICON_BIG as usize), LPARAM(0));
        tracing::info!("Native icon fix: window icon assigned={}, taskbar icon assigned={}", small.0 != 0, big.0 != 0);
    }
}

unsafe fn set_taskbar_properties(hwnd: HWND, exe: &Path, icon: &Path, app_id: &str) -> windows::core::Result<()> {
    let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd)?;
    let icon_reference = if icon.is_file() {
        format!("{},0", icon.display())
    } else {
        // Resource ID 1 is the exact same assets/icon.ico embedded by build.rs.
        format!("{},-1", exe.display())
    };
    let command = format!("\"{}\"", exe.display());
    // Set relaunch properties before the explicit window AppUserModelID.
    for (name, value) in [
        ("System.AppUserModel.RelaunchCommand", command.as_str()),
        ("System.AppUserModel.RelaunchDisplayNameResource", "TackleCast"),
        ("System.AppUserModel.RelaunchIconResource", icon_reference.as_str()),
        ("System.AppUserModel.ID", app_id),
    ] {
        let mut key = PROPERTYKEY::default();
        PSGetPropertyKeyFromName(&HSTRING::from(name), &mut key)?;
        let mut value = PROPVARIANT::from(value);
        PSCoerceToCanonicalValue(&key, &mut value)?;
        store.SetValue(&key, &value)?;
    }
    store.Commit()?;
    tracing::info!("Taskbar icon reference: {icon_reference}");
    Ok(())
}
