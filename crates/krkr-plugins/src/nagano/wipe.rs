use super::*;
pub(super) fn prepare(
    effect: &Effect,
    t: u64,
    d: u64,
    p: &mut [u32; 16],
    budget: &Budget,
) -> Result<Arc<Bytes>, String> {
    let rule = effect.rule.as_ref().ok_or("missing imagewipe rule")?;
    let rw = rule.size.width as usize;
    let rh = rule.size.height as usize;
    let h = effect.size.height as usize;
    let reverse = effect.values[0] != 0.;
    let time = if reverse { d - t } else { t };
    p[3] = ((u128::from(effect.size.width + rule.size.width) * u128::from(time) / u128::from(d))
        as i64
        - i64::from(rule.size.width)) as u32;
    p[4] = u32::from(reverse);
    p[5] = rule.size.width;
    p[6] = rule.size.height;
    // Rule pixels and per-row edges are uploaded once, shared by all frames.
    effect
        .table
        .get_or_init(|| {
            let source = rule
                .main
                .as_ref()
                .ok_or("missing imagewipe RGBA data")?
                .as_slice();
            if source.len() != rw * rh * 4 {
                return Err("invalid imagewipe rule storage".into());
            }
            table(h + rw * rh, budget, |bytes| {
                bytes[h * 4..].copy_from_slice(source);
                for y in 0..h {
                    let row = &source[y.min(rh - 1) * rw * 4..][..rw * 4];
                    let edge = row
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .rposition(|p| p[3] > 240)
                        .unwrap_or(rw / 2);
                    put(bytes, y, edge as i32);
                }
            })
        })
        .clone()
}
