//! Multimedia Class Scheduler Service (MMCSS) registration for the threads on
//! the capture-to-display path.
//!
//! MMCSS raises a registered thread into the realtime priority band for most
//! of each scheduling period, so a busy desktop (browser, game launcher, OBS)
//! can't delay the moment a captured frame is read, decoded or presented. A
//! plain `SetThreadPriority` boost still loses to ordinary threads that have
//! been starved long enough to receive a dynamic boost.

use tracing::{info, warn};
use windows::core::HSTRING;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
};

/// Keeps the calling thread registered with MMCSS until dropped. Must be
/// dropped on the thread that created it.
pub struct MmcssRegistration(HANDLE);

impl MmcssRegistration {
    /// Registers the calling thread under one of the task names in
    /// `HKLM\...\Multimedia\SystemProfile\Tasks` ("Capture", "Games", ...).
    pub fn register(task: &str) -> Option<Self> {
        let mut task_index = 0_u32;
        match unsafe { AvSetMmThreadCharacteristicsW(&HSTRING::from(task), &mut task_index) } {
            Ok(handle) => {
                info!("thread registered with MMCSS task '{task}'");
                Some(Self(handle))
            }
            Err(error) => {
                warn!("MMCSS registration for task '{task}' failed: {error}");
                None
            }
        }
    }
}

impl Drop for MmcssRegistration {
    fn drop(&mut self) {
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.0) };
    }
}
