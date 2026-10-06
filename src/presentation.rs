use crate::settings::PresentationMode;
use wgpu::PresentMode;

pub fn choose_present_mode(requested: PresentationMode, available: &[PresentMode]) -> PresentMode {
    let order = match requested {
        PresentationMode::Mailbox => [PresentMode::Mailbox, PresentMode::Immediate],
        PresentationMode::MailboxLowLatency => [PresentMode::Mailbox, PresentMode::Fifo],
        PresentationMode::Immediate => [PresentMode::Immediate, PresentMode::Mailbox],
    };
    order.into_iter().find(|mode| available.contains(mode)).unwrap_or(
        if requested == PresentationMode::MailboxLowLatency { PresentMode::Fifo }
        else { PresentMode::AutoVsync }
    )
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
        assert_eq!(choose_present_mode(PresentationMode::Mailbox, &[PresentMode::Immediate]), PresentMode::Immediate);
        assert_eq!(choose_present_mode(PresentationMode::Immediate, &[PresentMode::Fifo]), PresentMode::AutoVsync);
        assert_eq!(choose_present_mode(PresentationMode::MailboxLowLatency, &modes), PresentMode::Mailbox);
        assert_eq!(choose_present_mode(PresentationMode::MailboxLowLatency, &[PresentMode::Immediate, PresentMode::Fifo]), PresentMode::Fifo);
        assert_eq!(choose_present_mode(PresentationMode::MailboxLowLatency, &[]), PresentMode::Fifo);
        assert_eq!(PresentationMode::MailboxLowLatency.frame_latency(), 1);
        assert_eq!(PresentationMode::Immediate.frame_latency(), 1);
        assert_eq!(PresentationMode::Mailbox.frame_latency(), 2);
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
