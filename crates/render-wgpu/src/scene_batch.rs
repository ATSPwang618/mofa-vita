//! Non-overlapping draws can read one backdrop and share a render pass.
//! Overlaps, target changes and transitions remain ordering boundaries.
use super::scene::Pass;
use krkr_protocol::graphics::Rect;
use std::{ops::Range, sync::Arc};

// Bound the pairwise overlap checks for long character runs.
const MAX_DRAWS: usize = 64;

pub(super) struct Batch {
    pub range: Range<usize>,
    pub backdrop: Option<Rect>,
}

pub(super) fn plan(passes: &[Pass], band_height: u32) -> Vec<Batch> {
    let mut result = Vec::new();
    let mut start = 0;
    while start < passes.len() {
        let Pass::Draw {
            target,
            clip,
            parameters,
            ..
        } = &passes[start]
        else {
            result.push(Batch {
                range: start..start + 1,
                backdrop: None,
            });
            start += 1;
            continue;
        };
        let mut end = start + 1;
        let mut bounds = *clip;
        let mut area = u64::from(clip.width) * u64::from(clip.height);
        let mut reads = parameters.needs_destination();
        while end < passes.len() && end - start < MAX_DRAWS {
            let Pass::Draw {
                target: next,
                clip,
                parameters,
                ..
            } = &passes[end]
            else {
                break;
            };
            if !Arc::ptr_eq(target, next) {
                break;
            }
            if passes[start..end].iter().any(|pass| {
                matches!(pass, Pass::Draw { clip: old, .. } if old.intersection(*clip).is_some())
            }) {
                break;
            }
            let left = bounds.left.min(clip.left);
            let top = bounds.top.min(clip.top);
            let right = (i64::from(bounds.left) + i64::from(bounds.width))
                .max(i64::from(clip.left) + i64::from(clip.width));
            let bottom = (i64::from(bounds.top) + i64::from(bounds.height))
                .max(i64::from(clip.top) + i64::from(clip.height));
            let joined = Rect {
                left,
                top,
                width: (right - i64::from(left)) as u32,
                height: (bottom - i64::from(top)) as u32,
            };
            // Composition reserves one band of backdrop storage. Adjacent
            // bands may otherwise merge across their non-overlapping edges.
            if joined.height > band_height {
                break;
            }
            let next_area = area + u64::from(clip.width) * u64::from(clip.height);
            // Do not turn small, distant glyphs into a fullscreen copy.
            if u64::from(joined.width) * u64::from(joined.height) > next_area * 2 {
                break;
            }
            bounds = joined;
            area = next_area;
            reads |= parameters.needs_destination();
            end += 1;
        }
        result.push(Batch {
            range: start..end,
            backdrop: reads.then_some(bounds),
        });
        start = end;
    }
    result
}
