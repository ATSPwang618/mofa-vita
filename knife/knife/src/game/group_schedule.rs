//! Reorder only independent group production ahead of queued stage writes.
//! Surface contents/clear commands and the order within either destination
//! remain unchanged. Texture identity includes aliases through logical views.

/// Reads carry whether their draw writes the stage. A producer that reads the
/// stage, or a stage draw that reads the persistent group pair, forbids moving
/// the stage draw after the producer. Rule-mask samplers are reads as well.
pub(super) fn can_defer(
    stage: usize,
    pair: [Option<usize>; 2],
    reads: impl IntoIterator<Item = (bool, usize)>,
) -> bool {
    reads.into_iter().all(|(on_stage, source)| {
        if on_stage { !pair.contains(&Some(source)) } else { source != stage }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_directions_and_rule_reads_are_barriers() {
        assert!(can_defer(10, [Some(20), Some(21)], [(true, 1), (false, 2)]));
        assert!(can_defer(10, [None; 2], [(true, 1), (false, 2)]));
        for (on_stage, source) in [(false, 10), (true, 20), (true, 21)] {
            // The second read models a rule sampler, as passed by production.
            assert!(!can_defer(10, [Some(20), Some(21)], [(true, 1), (on_stage, source)]));
        }
        assert!(can_defer(10, [Some(20), Some(21)], [(false, 20), (false, 21)]));
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Image { color: i64, alpha: i64 }
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct State { stage: Image, pixel: Image, mask: Image }
    fn update(target: &mut Image, value: i64, clear: bool) {
        if clear { *target = Image { color: 0, alpha: 0 }; }
        // Order-sensitive integer operations expose lost clears/reordering.
        target.color = target.color * 3 + value;
        target.alpha = (target.alpha * 7 + value).rem_euclid(251);
    }
    fn stage_run(state: &mut State, clear: &mut bool, value: i64) {
        update(&mut state.stage, value, *clear); *clear = false;
    }
    fn composite(state: &mut State, mode: i64) {
        state.stage.color = state.stage.color * mode + state.pixel.color * state.mask.alpha;
        state.stage.alpha = (state.stage.alpha + state.pixel.alpha * state.mask.alpha).rem_euclid(251);
    }
    fn frame(mut state: State, deferred: bool, flags: u8) -> State {
        let mut clear = true;
        for group in 0..3 {
            let run = 10 + group;
            if !deferred { stage_run(&mut state, &mut clear, run); }
            // Explicitly include a side that inherits the prior group's/frame's
            // pixels: the scheduling optimisation must not assume both clear.
            update(&mut state.pixel, 17 + group, flags & (1 << (group * 2)) != 0);
            update(&mut state.mask, 31 + group, flags & (2 << (group * 2)) != 0);
            if deferred { stage_run(&mut state, &mut clear, run); }
            composite(&mut state, 2 + group);
        }
        // Tail writes are observable persistent state for the next frame.
        update(&mut state.pixel, 7, false);
        state
    }

    #[test]
    fn preserves_clear_blend_order_and_cross_frame_pair_for_all_clear_patterns() {
        let initial = State {
            stage: Image { color: 3, alpha: 5 },
            pixel: Image { color: 11, alpha: 13 },
            mask: Image { color: 17, alpha: 19 },
        };
        for flags in 0..64 {
            let old = frame(initial.clone(), false, flags);
            let new = frame(initial.clone(), true, flags);
            assert_eq!(old, new, "first frame clear flags {flags}");
            assert_eq!(frame(old, false, flags ^ 63), frame(new, true, flags ^ 63),
                "second frame inherits pair, clear flags {flags}");
        }
    }
}
