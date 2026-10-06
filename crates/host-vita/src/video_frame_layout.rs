//! Physical decoder blocks use the kernel's native 1 MiB granularity.
//! Stronger callback alignment is satisfied inside the block, without passing
//! optional alignment attributes to the PHYCONT allocator.
pub const GRANULARITY: u32 = 1024 * 1024;

pub struct FrameLayout {
    pub bytes: u32,
    alignment: u32,
    payload: u32,
}

impl FrameLayout {
    pub fn new(alignment: u32, payload: u32) -> Result<Self, &'static str> {
        let alignment = alignment.max(1);
        if !alignment.is_power_of_two() || payload == 0 {
            return Err("invalid frame size or alignment");
        }
        // A naturally aligned block needs at most alignment - GRANULARITY
        // bytes of padding, including for alignments larger than 1 MiB.
        let bytes = payload
            .checked_add(alignment.saturating_sub(GRANULARITY))
            .and_then(|n| n.checked_add(GRANULARITY - 1))
            .map(|n| n & !(GRANULARITY - 1))
            .filter(|&n| n <= i32::MAX as u32)
            .ok_or("frame allocation overflow")?;
        Ok(Self {
            bytes,
            alignment,
            payload,
        })
    }

    pub fn offset(&self, base: usize) -> Option<usize> {
        if base == 0 || !base.is_multiple_of(GRANULARITY as usize) {
            return None;
        }
        let mask = self.alignment as usize - 1;
        let aligned = base.checked_add(mask)? & !mask;
        let offset = aligned.checked_sub(base)?;
        let end = offset.checked_add(self.payload as usize)?;
        base.checked_add(self.bytes as usize)?;
        (end <= self.bytes as usize).then_some(offset)
    }
}
