use std::sync::Arc;

// Only the decode thread claims free slots. Other threads can only clone a
// lease they already own, so a count of one cannot acquire a concurrent reader.
pub fn free_slot(leases: &[Arc<()>]) -> Option<usize> {
    leases.iter().position(|lease| Arc::strong_count(lease) == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn storage_is_held_until_both_frame_and_gpu_readers_finish() {
        let slots = vec![Arc::new(()), Arc::new(())];
        let frame = slots[0].clone();
        let gpu_copy = frame.clone();
        let other_frame = slots[1].clone();
        assert_eq!(free_slot(&slots), None);
        drop(frame);
        assert_eq!(free_slot(&slots), None);
        drop(gpu_copy);
        assert_eq!(free_slot(&slots), Some(0));
        drop(other_frame);
        assert_eq!(free_slot(&slots), Some(0));
    }
}
