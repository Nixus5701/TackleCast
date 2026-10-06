use crate::settings::PresentationMode;
use wgpu::PresentMode;

/// Picks the swapchain present mode. Only `Immediate` may tear: both Mailbox
/// options fall back to FIFO VSync, never to Immediate.
pub fn choose_present_mode(requested: PresentationMode, available: &[PresentMode]) -> PresentMode {
    let order: &[PresentMode] = match requested {
        PresentationMode::Mailbox | PresentationMode::MailboxLowLatency => &[PresentMode::Mailbox],
        PresentationMode::Immediate => &[PresentMode::Immediate, PresentMode::Mailbox],
    };
    order.iter().copied().find(|mode| available.contains(mode)).unwrap_or(
        if requested == PresentationMode::Immediate { PresentMode::AutoVsync }
        else { PresentMode::Fifo }
    )
}

/// The swapchain's maximum frame latency (DXGI keeps one more buffer than
/// this).
///
/// Low-latency Mailbox keeps a latency of 2, i.e. three buffers. With only two,
/// one buffer is on screen and the other is queued for the next vblank, so
/// rendering a newer capture frame has to wait for that vblank (always in
/// fullscreen independent flip, and whenever two frames land in one refresh
/// under DWM). The third buffer lets a newer frame replace the queued one
/// immediately. GPU queueing, which a latency of 1 also prevented, is instead
/// prevented by the renderer's GPU-queue gate (`gates_gpu_queue`), so the
/// extra buffer adds no latency. FIFO and Immediate have no queued frame to
/// replace and use a latency of 1.
pub fn swapchain_frame_latency(requested: PresentationMode, active: PresentMode) -> u32 {
    match (requested, active) {
        (PresentationMode::Mailbox, _) => 2,
        (PresentationMode::MailboxLowLatency, PresentMode::Mailbox) => 2,
        (PresentationMode::MailboxLowLatency | PresentationMode::Immediate, _) => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modes_and_unsupported_fallbacks() {
        let modes = [PresentMode::Fifo, PresentMode::Mailbox, PresentMode::Immediate];
        assert_eq!(choose_present_mode(PresentationMode::Mailbox, &modes), PresentMode::Mailbox);
        assert_eq!(choose_present_mode(PresentationMode::Immediate, &modes), PresentMode::Immediate);
        assert_eq!(choose_present_mode(PresentationMode::Immediate, &[PresentMode::Mailbox]), PresentMode::Mailbox);
        assert_eq!(choose_present_mode(PresentationMode::Immediate, &[PresentMode::Fifo]), PresentMode::AutoVsync);
        assert_eq!(choose_present_mode(PresentationMode::MailboxLowLatency, &modes), PresentMode::Mailbox);
        assert_eq!(choose_present_mode(PresentationMode::MailboxLowLatency, &[]), PresentMode::Fifo);
    }
    #[test]
    fn mailbox_modes_never_fall_back_to_tearing() {
        let tearing_only = [PresentMode::Immediate, PresentMode::Fifo];
        for mode in [PresentationMode::Mailbox, PresentationMode::MailboxLowLatency] {
            assert_eq!(choose_present_mode(mode, &tearing_only), PresentMode::Fifo);
            assert_eq!(choose_present_mode(mode, &[PresentMode::Immediate]), PresentMode::Fifo);
        }
    }
    #[test]
    fn frame_latency_and_gpu_gate() {
        use PresentationMode as P;
        // Low-latency Mailbox keeps a spare buffer so presents never wait for vblank,
        // and relies on the GPU gate instead of a shallow swapchain.
        assert_eq!(swapchain_frame_latency(P::MailboxLowLatency, PresentMode::Mailbox), 2);
        assert_eq!(swapchain_frame_latency(P::MailboxLowLatency, PresentMode::Fifo), 1);
        assert_eq!(swapchain_frame_latency(P::Immediate, PresentMode::Immediate), 1);
        assert_eq!(swapchain_frame_latency(P::Mailbox, PresentMode::Mailbox), 2);
        assert!(P::MailboxLowLatency.gates_gpu_queue());
        assert!(P::Immediate.gates_gpu_queue());
        assert!(!P::Mailbox.gates_gpu_queue());
    }
    #[test]
    fn old_settings_and_saved_selection() {
        let old: crate::settings::Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(old.presentation_mode, PresentationMode::Mailbox);
        let mut updated = old;
        for mode in PresentationMode::ALL {
            updated.presentation_mode = mode;
            let saved = serde_json::to_string(&updated).unwrap();
            let restored: crate::settings::Settings = serde_json::from_str(&saved).unwrap();
            assert_eq!(restored, updated);
        }
    }
}
