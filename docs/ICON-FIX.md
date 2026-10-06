# Native Windows taskbar icon fix

Close the previous TackleCast instance, extract the complete ZIP into a new folder, and launch that folder's TackleCast.exe. Keep assets/icon.ico beside the executable in its assets subfolder. Copy your prior tacklecast_settings.json into the new folder while the app is closed to retain your preferences.

This update loads the actual assets/icon.ico using Windows LoadImageW via winit's native icon API, rather than decoding it to RGBA and reconstructing an icon. Paths are resolved relative to the executable, independent of the launch directory. It uses separate DPI-scaled small and large icons, retains ownership through winit, and falls back to embedded icon resource 1 if loading the file fails.

The window is created hidden; icons and window-level AppUserModel properties are assigned before it is shown. RelaunchIconResource explicitly points Windows to assets/icon.ico,0, with the embedded EXE resource as a fallback when the file is missing. The relaunch command and display name are set together. The existing application ID is preserved.

If you pinned an earlier build, unpin the old shortcut, launch the new EXE directly, and pin its running taskbar button. This package does not modify existing pinned shortcuts or clear the Windows icon cache.

Startup logs now show which native ICO loaded, whether small and large window icon handles were assigned, and the taskbar icon reference. This can distinguish loading errors from Windows shell/shortcut behavior.

Validation: Windows x64 release compilation and bundled-DLL import checks passed. Icon assets are unchanged from the prior package. The 20 portable checks from the image-controls update remain applicable; those checks do not exercise the Windows shell. Visual taskbar behavior could not be verified on a Windows desktop in this environment. No renderer or capture changes are included in this follow-up.

Windows property reference:
https://learn.microsoft.com/en-us/windows/win32/properties/props-system-appusermodel-relaunchiconresource

native-icon-fix.patch records the changes relative to the image-controls source archive.
