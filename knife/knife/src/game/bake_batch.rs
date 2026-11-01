//! Only a dispatch's proven source-independent stage setters may defer baking.
//! The owner flushes before every other opcode and every actual dispatch exit.
const LAYERS: usize = 40;

#[derive(Default)]
pub(super) struct Batch {
    active: bool,
    mask: u64,
    order: Vec<u8>,
}
impl Batch {
    pub(super) fn allows(name: &str) -> bool {
        matches!(name, "STEC" | "SNX2" | "SNX3" | "STBR" | "STZ4" | "STRT" | "SNX4")
    }
    pub(super) fn set_active(&mut self, active: bool) { self.active = active; }
    pub(super) fn active(&self) -> bool { self.active }
    /// None means immediate; Some reports whether this layer was already dirty.
    pub(super) fn queue(&mut self, layer: i32) -> Option<bool> {
        if !self.active || !(0..LAYERS as i32).contains(&layer) { return None; }
        Some(self.defer(layer))
    }
    /// Async materialization also collects setters invoked outside a coalesced
    /// script run (e.g. the five clock layers), until the next real barrier.
    pub(super) fn defer(&mut self, layer: i32) -> bool {
        if !(0..LAYERS as i32).contains(&layer) { return false; }
        let bit = 1u64 << layer;
        let already = self.mask & bit != 0;
        if !already { self.mask |= bit; self.order.push(layer as u8); }
        already
    }
    /// Disable before materializing so flushes cannot requeue themselves. The
    /// first-dirty order preserves load order across distinct layer images.
    pub(super) fn take(&mut self) -> Vec<u8> {
        self.active = false;
        self.mask = 0;
        std::mem::take(&mut self.order)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_proven_setters_coalesce_and_source_reads_are_barriers() {
        for name in ["STEC", "SNX2", "SNX3", "STBR", "STZ4", "STRT", "SNX4"] { assert!(Batch::allows(name)); }
        for name in ["STCH", "SCH2", "STMA", "STCP", "FADS", "SGMA", "CLO3", "JUMP", "NOP_", "ZM00000", "ZY00000"] { assert!(!Batch::allows(name)); }
        let mut batch = Batch::default();
        assert_eq!(batch.queue(0), None);
        batch.set_active(true);
        assert_eq!(batch.queue(21), Some(false));
        assert_eq!(batch.queue(21), Some(true));
        assert_eq!(batch.queue(21), Some(true));
        assert_eq!(batch.queue(4), Some(false));
        assert_eq!(batch.queue(-1), None);
        assert_eq!(batch.queue(40), None);
        assert_eq!(batch.take(), [21, 4]);
        assert_eq!(batch.queue(21), None);
        assert!(batch.take().is_empty());
    }
    #[test]
    fn frame_or_budget_exit_and_reverting_parameters_leave_final_state() {
        let mut batch=Batch::default(); let mut actual=[0;40]; let mut wanted=actual;
        let mut builds=0;
        // STEC, SNX2, SNX3 before a use: only the final texture is observable.
        batch.set_active(true);
        for v in [2,7,9] { wanted[21]=v; batch.queue(21); }
        for li in batch.take() { actual[li as usize]=wanted[li as usize]; builds+=1; }
        assert_eq!(actual[21],9); assert_eq!(builds,1);
        // A later dispatch that restores the already materialized Bake does no work.
        batch.set_active(true);
        for v in [3,9] { wanted[21]=v; batch.queue(21); }
        for li in batch.take() { if actual[li as usize]!=wanted[li as usize] { builds+=1; } }
        assert_eq!(builds,1);
        // At a real budget/frame boundary, the pending state is materialized;
        // the next dispatch cannot overwrite it across an observable boundary.
        batch.set_active(true); wanted[2]=5; batch.queue(2);
        for li in batch.take() { actual[li as usize]=wanted[li as usize]; }
        assert_eq!(actual[2],5); assert_eq!(batch.queue(2),None);
    }
}
