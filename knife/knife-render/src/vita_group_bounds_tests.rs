use super::test_backend::{Control, Event, Gxm};
use super::*;
#[path = "../../knife/src/game/group_cache.rs"]
mod group_cache;
#[path = "../../knife/src/game/producer_reuse.rs"]
mod producer_reuse;

fn setup() -> (Renderer, Control) {
    let gxm = Gxm::new().unwrap();
    let control = gxm.control.clone();
    let renderer = Renderer::new_with_gxm(gxm).unwrap();
    control.borrow_mut().events.clear();
    (renderer, control)
}

struct OptimizationScope(u64);
impl OptimizationScope {
    fn set(mask: u64) -> Self {
        let previous = knife_perf::optimizations::mask();
        knife_perf::optimizations::set(mask);
        Self(previous)
    }
}
impl Drop for OptimizationScope {
    fn drop(&mut self) { knife_perf::optimizations::set(self.0); }
}

fn group_draw(control: &Control) -> Vec<Vertex> {
    control.borrow().events.iter().find_map(|event| match event {
        Event::Draw { shader: EffectShader::GroupComposite, vertices, .. } => Some(vertices.clone()),
        _ => None,
    }).expect("group composite draw")
}

fn initialize_full_group(renderer: &mut Renderer, encoder: &mut CommandEncoder,
    pixel: &RenderTarget, mask: &RenderTarget)
{
    for producer in [pixel, mask] {
        renderer.try_draw(encoder, producer, Some([1.0; 4]), &[]).unwrap();
    }
}

#[test]
fn group_composite_region_clips_positions_but_preserves_canvas_uv_mapping() {
    let _options = OptimizationScope::set(
        knife_perf::optimizations::GROUP_BOUNDS);
    let (mut renderer, control) = setup();
    let target = renderer.create_scene_target(CANVAS_W, CANVAS_H, "target");
    let pixel = renderer.create_scene_target(CANVAS_W, CANVAS_H, "pixel");
    let mask = renderer.create_scene_target(CANVAS_W, CANVAS_H, "mask");
    let mut encoder = renderer.encoder();
    initialize_full_group(&mut renderer, &mut encoder, &pixel, &mask);
    renderer.try_group_composite_region(
        &mut encoder, &target, &pixel, &mask, Blend::Alpha, 0.5,
        CoverageBound::Rect { x: 240, y: 135, width: 480, height: 270 },
    ).unwrap();
    renderer.try_present(encoder, &target).unwrap();
    let vertices = group_draw(&control);
    assert_eq!(vertices.len(), 6);
    assert_eq!(vertices[0].pos, [-0.5, 0.5]);
    assert_eq!(vertices[1].pos, [0.5, 0.5]);
    assert_eq!(vertices[2].pos, [0.5, -0.5]);
    assert_eq!(vertices[5].pos, [-0.5, -0.5]);
    assert_eq!(vertices[0].uv, [0.25, 0.25]);
    assert_eq!(vertices[1].uv, [0.75, 0.25]);
    assert_eq!(vertices[2].uv, [0.75, 0.75]);
    assert_eq!(vertices[5].uv, [0.25, 0.75]);
}

#[test]
fn group_composite_region_uses_full_canvas_for_disabled_unknown_invalid_or_unsafe_bounds() {
    for (enabled, coverage, blend) in [
        (false, CoverageBound::Rect { x: 240, y: 135, width: 480, height: 270 }, Blend::Alpha),
        (true, CoverageBound::Unknown, Blend::Alpha),
        (true, CoverageBound::Rect { x: 240, y: 135, width: 0, height: 270 }, Blend::Alpha),
        (true, CoverageBound::Rect { x: 240, y: 135, width: 480, height: 270 }, Blend::Ps(PsOp::Normal)),
    ] {
        let options = if enabled { knife_perf::optimizations::GROUP_BOUNDS } else { 0 };
        let _options = OptimizationScope::set(options);
        let (mut renderer, control) = setup();
        let target = renderer.create_scene_target(CANVAS_W, CANVAS_H, "target");
        let pixel = renderer.create_scene_target(CANVAS_W, CANVAS_H, "pixel");
        let mask = renderer.create_scene_target(CANVAS_W, CANVAS_H, "mask");
        let mut encoder = renderer.encoder();
        initialize_full_group(&mut renderer, &mut encoder, &pixel, &mask);
        renderer.try_group_composite_region(
            &mut encoder, &target, &pixel, &mask, blend, 0.5, coverage,
        ).unwrap();
        renderer.try_present(encoder, &target).unwrap();
        let vertices = control.borrow().events.iter().find_map(|event| match event {
            Event::Draw { vertices, .. } => Some(vertices.clone()),
            _ => None,
        }).expect("composite draw");
        assert_eq!(vertices[0].pos, [-1.0, 1.0]);
        assert_eq!(vertices[1].pos, [1.0, 1.0]);
        assert_eq!(vertices[2].pos, [1.0, -1.0]);
        assert_eq!(vertices[5].pos, [-1.0, -1.0]);
        assert_eq!(vertices[0].uv, [0.0, 0.0]);
        assert_eq!(vertices[1].uv, [1.0, 0.0]);
        assert_eq!(vertices[2].uv, [1.0, 1.0]);
        assert_eq!(vertices[5].uv, [0.0, 1.0]);
    }
}

#[test]
fn group_composite_empty_region_keeps_a_restorable_full_final_write() {
    let _options = OptimizationScope::set(
        knife_perf::optimizations::GROUP_BOUNDS);
    let (mut renderer, control) = setup();
    let target = renderer.create_scene_target(CANVAS_W, CANVAS_H, "target");
    let pixel = renderer.create_scene_target(CANVAS_W, CANVAS_H, "pixel");
    let mask = renderer.create_scene_target(CANVAS_W, CANVAS_H, "mask");
    let mut encoder = renderer.encoder();
    initialize_full_group(&mut renderer, &mut encoder, &pixel, &mask);
    renderer.try_group_composite_region(
        &mut encoder, &target, &pixel, &mask, Blend::Alpha, 0.5, CoverageBound::Empty,
    ).unwrap();
    renderer.try_present(encoder, &target).unwrap();
    let vertices = group_draw(&control);
    assert_eq!(vertices[0].pos, [-1.0, 1.0]);
    assert_eq!(vertices[2].pos, [1.0, -1.0]);
}

#[test]
fn group_bounds_keep_full_writes_for_non_neutral_transparent_sources() {
    let _options = OptimizationScope::set(knife_perf::optimizations::GROUP_BOUNDS);
    for blend in [Blend::Opaque, Blend::MaskAlpha, Blend::Ps(PsOp::SubNoAlpha)] {
        for requested in [CoverageBound::rect(2, 2, 2, 2), CoverageBound::Empty] {
            for scratch_fallback in [false, true] {
                let (mut renderer, control) = setup();
                let target = renderer.create_target(8, 8, "target");
                let pixel = renderer.create_target(8, 8, "pixel");
                let mask = renderer.create_target(8, 8, "mask");
                let scratch = renderer.create_target(8, 8, "scratch");
                let mut encoder = renderer.encoder();
                initialize_full_group(&mut renderer, &mut encoder, &pixel, &mask);
                if scratch_fallback {
                    renderer.try_group_composite_via_scratch_region(
                        &mut encoder, &target, &pixel, &mask, &scratch, blend, 0.5, requested,
                    ).unwrap();
                } else {
                    renderer.try_group_composite_region(
                        &mut encoder, &target, &pixel, &mask, blend, 0.5, requested,
                    ).unwrap();
                }
                renderer.try_present(encoder, &target).unwrap();
                let control = control.borrow();
                let vertices = control.events.iter().filter_map(|event| match event {
                    Event::Draw { vertices, .. } => Some(vertices), _ => None,
                }).last().expect("non-neutral blend must retain its final full write");
                assert_eq!(vertices[0].pos, [-1.0, 1.0], "{blend:?}, {requested:?}, scratch={scratch_fallback}");
                assert_eq!(vertices[2].pos, [1.0, -1.0]);
                assert_eq!(vertices[0].uv, [0.0, 0.0]);
                assert_eq!(vertices[2].uv, [1.0, 1.0]);
            }
        }
    }
}

#[test]
fn group_bounds_auto_uses_recorded_clipped_producer_geometry_and_preserves_tail_writes() {
    let _options = OptimizationScope::set(knife_perf::optimizations::GROUP_BOUNDS);
    for scratch_fallback in [false, true] {
        let (mut renderer, _) = setup();
        let target = renderer.create_target(16, 16, "target");
        let pixel = renderer.create_target(16, 16, "pixel");
        let mask = renderer.create_target(16, 16, "mask");
        let scratch = renderer.create_target(16, 16, "scratch");
        let mut encoder = renderer.encoder();
        let mut q = Quad::rect(renderer.white.clone(), [0.0, 0.0, 16.0, 16.0], [1.0; 4]);
        q.clip = Some([[2.0, 4.0], [10.0, 4.0], [10.0, 12.0], [2.0, 12.0]]);
        renderer.try_draw(&mut encoder, &pixel, Some([0.0; 4]), &[q]).unwrap();
        renderer.try_draw(&mut encoder, &mask, Some([0.0; 4]), &[
            Quad::rect(renderer.white.clone(), [6.0, 0.0, 8.0, 16.0], [1.0; 4]),
        ]).unwrap();
        if scratch_fallback {
            renderer.try_group_composite_via_scratch(&mut encoder, &target, &pixel, &mask,
                &scratch, Blend::Alpha, 1.0).unwrap();
        } else {
            renderer.try_group_composite(&mut encoder, &target, &pixel, &mask, Blend::Alpha, 1.0).unwrap();
        }
        let vertices = encoder.commands.iter().filter_map(|command| match command {
            Command::Draw { vertices, .. } => Some(vertices), _ => None,
        }).last().unwrap();
        assert_eq!(vertices[0].pos, [-0.25, 0.5]);
        assert_eq!(vertices[2].pos, [0.25, -0.5]);
        assert_eq!(vertices[0].uv, [0.375, 0.25]);
        assert_eq!(vertices[2].uv, [0.625, 0.75]);
        renderer.try_draw(&mut encoder, &pixel, None, &[
            Quad::rect(renderer.white.clone(), [12.0, 12.0, 2.0, 2.0], [1.0; 4]),
        ]).unwrap();
        let tail = encoder.commands.iter().filter(|command| matches!(command, Command::Draw { .. })).count();
        assert_eq!(tail, if scratch_fallback { 5 } else { 4 }, "tail producer draw must remain");
    }
}

#[test]
fn group_bounds_preserve_no_clear_history_and_copy_owner_state_across_submission() {
    let _options = OptimizationScope::set(knife_perf::optimizations::GROUP_BOUNDS);
    let (mut renderer, _) = setup();
    let target = renderer.create_target(16, 16, "target");
    let pixel = renderer.create_target(16, 16, "pixel");
    let mask = renderer.create_target(16, 16, "mask");
    let copy = renderer.create_target(16, 16, "copy");
    let q = |r: &Renderer, rect| Quad::rect(r.white.clone(), rect, [1.0; 4]);
    let mut first = renderer.encoder();
    renderer.try_draw(&mut first, &pixel, Some([0.0; 4]), &[q(&renderer, [2.0, 2.0, 2.0, 2.0])]).unwrap();
    renderer.try_draw(&mut first, &mask, Some([0.0; 4]), &[q(&renderer, [0.0, 0.0, 16.0, 16.0])]).unwrap();
    renderer.try_present(first, &target).unwrap();
    let mut next = renderer.encoder();
    renderer.try_draw(&mut next, &pixel, None, &[q(&renderer, [10.0, 8.0, 2.0, 4.0])]).unwrap();
    renderer.try_copy_target(&mut next, &pixel, &copy).unwrap();
    renderer.try_group_composite(&mut next, &target, &copy, &mask, Blend::Alpha, 1.0).unwrap();
    let vertices = next.commands.iter().filter_map(|command| match command {
        Command::Draw { shader: EffectShader::GroupComposite, vertices, .. } => Some(vertices), _ => None,
    }).last().unwrap();
    assert_eq!(vertices[0].pos, [-0.75, 0.75]);
    assert_eq!(vertices[2].pos, [0.5, -0.5]);
}

#[test]
fn group_bounds_cache_hits_and_ring_swaps_keep_the_exact_owner_and_generation() {
    let _options = OptimizationScope::set(knife_perf::optimizations::GROUP_BOUNDS
        | knife_perf::optimizations::TARGET_RING | knife_perf::optimizations::PRODUCER_REUSE);
    let (mut renderer, _) = setup();
    let target = renderer.create_scene_target(CANVAS_W, CANVAS_H, "target");
    let mut pair = ["pixel", "mask"].map(|name| renderer.create_scene_target(CANVAS_W, CANVAS_H, name));
    let q = Quad::rect(renderer.white.clone(), [240.0, 120.0, 480.0, 240.0], [1.0; 4]);
    let mut cache = group_cache::Cache::default();
    let mut first = renderer.encoder();
    cache.draw(&mut renderer, &mut first, &mut pair, 0, true, std::slice::from_ref(&q), true);
    renderer.try_present(first, &target).unwrap();
    let prior = pair[0].tex.render_identity();
    let bound = pair[0].tex.native.coverage_for(0);
    assert!(matches!(bound, CoverageBound::Rect { .. }));
    let mut second = renderer.encoder();
    cache.draw(&mut renderer, &mut second, &mut pair, 0, true, &[q], true);
    assert!(second.commands.is_empty(), "same source must hit the retained cache");
    assert_eq!(pair[0].tex.render_identity(), prior);
    assert_eq!(pair[0].tex.native.coverage_for(second.coverage_id), bound);
    let mut ring = producer_reuse::Ring::default();
    ring.rotate(&renderer, &second, &mut pair[0], &[], 1);
    assert_ne!(pair[0].tex.render_identity(), prior);
    assert!(ring.restore_if(&mut pair[0], Some(prior), &[], 1));
    assert_eq!(pair[0].tex.render_identity(), prior);
    assert_eq!(pair[0].tex.native.coverage_for(second.coverage_id), bound);
    pair[0].tex.native.changed();
    assert_eq!(pair[0].tex.native.coverage_for(second.coverage_id), CoverageBound::Unknown);
}

#[test]
fn group_bounds_cancelled_failed_or_foreign_encoder_writes_never_publish_narrow_coverage() {
    let _options = OptimizationScope::set(knife_perf::optimizations::GROUP_BOUNDS);
    for fail_submission in [false, true] {
        let (mut renderer, control) = setup();
        let target = renderer.create_target(16, 16, "target");
        let pixel = renderer.create_target(16, 16, "pixel");
        let mut pending = renderer.encoder();
        renderer.try_draw(&mut pending, &pixel, Some([0.0; 4]), &[
            Quad::rect(renderer.white.clone(), [2.0, 2.0, 2.0, 2.0], [1.0; 4]),
        ]).unwrap();
        assert_eq!(pixel.tex.native.coverage_for(pending.coverage_id), CoverageBound::rect(2, 2, 2, 2));
        assert_eq!(pixel.tex.native.coverage_for(renderer.encoder().coverage_id), CoverageBound::Unknown);
        let version = pixel.tex.render_identity();
        if fail_submission {
            control.borrow_mut().fail_next_draw = true;
            assert!(renderer.try_present(pending, &target).is_err());
        } else { drop(pending); }
        assert_eq!(pixel.tex.native.coverage_for(0), CoverageBound::Unknown);
        assert_ne!(pixel.tex.render_identity(), version, "cancelled predictions invalidate cached versions");
    }
}

#[test]
fn group_bounds_out_of_order_encoders_invalidate_superseded_predictions() {
    let _options = OptimizationScope::set(knife_perf::optimizations::GROUP_BOUNDS);
    let (mut renderer, _) = setup();
    let target = renderer.create_target(16, 16, "target");
    let pixel = renderer.create_target(16, 16, "pixel");
    let mut earlier = renderer.encoder();
    renderer.try_draw(&mut earlier, &pixel, Some([0.0; 4]), &[
        Quad::rect(renderer.white.clone(), [0.0, 0.0, 2.0, 2.0], [1.0; 4]),
    ]).unwrap();
    let mut later = renderer.encoder();
    renderer.try_draw(&mut later, &pixel, Some([0.0; 4]), &[
        Quad::rect(renderer.white.clone(), [12.0, 12.0, 2.0, 2.0], [1.0; 4]),
    ]).unwrap();
    renderer.try_present(later, &target).unwrap();
    assert_eq!(pixel.tex.native.coverage_for(0), CoverageBound::rect(12, 12, 2, 2));
    renderer.try_present(earlier, &target).unwrap();
    assert_eq!(pixel.tex.native.coverage_for(0), CoverageBound::Unknown);
}

#[test]
fn group_bounds_unknown_owner_overrides_an_explicit_rect_and_append_failure_invalidates() {
    let _options = OptimizationScope::set(knife_perf::optimizations::GROUP_BOUNDS);
    let (mut renderer, _) = setup();
    let target = renderer.create_target(16, 16, "target");
    let pixel = renderer.create_target(16, 16, "pixel");
    let mask = renderer.create_target(16, 16, "mask");
    let mut encoder = renderer.encoder();
    renderer.try_draw(&mut encoder, &mask, Some([1.0; 4]), &[]).unwrap();
    renderer.try_group_composite_region(&mut encoder, &target, &pixel, &mask,
        Blend::Alpha, 1.0, CoverageBound::rect(2, 2, 2, 2)).unwrap();
    let vertices = encoder.commands.iter().filter_map(|command| match command {
        Command::Draw { vertices, .. } => Some(vertices), _ => None,
    }).last().unwrap();
    assert_eq!(vertices[0].pos, [-1.0, 1.0]);
    assert_eq!(vertices[2].pos, [1.0, -1.0]);
    assert_eq!(mask.tex.native.coverage_for(encoder.coverage_id), CoverageBound::Full);
    encoder.vertex_bytes = MAX_FRAME_VERTEX_BYTES;
    assert!(renderer.try_draw(&mut encoder, &mask, None, &[
        Quad::rect(renderer.white.clone(), [2.0, 2.0, 2.0, 2.0], [1.0; 4]),
    ]).is_err());
    assert_eq!(mask.tex.native.coverage_for(encoder.coverage_id), CoverageBound::Unknown);
}

#[test]
fn group_bounds_empty_owner_retains_producer_clears_and_full_scratch_copy_mask() {
    let _options = OptimizationScope::set(knife_perf::optimizations::GROUP_BOUNDS);
    for scratch_fallback in [false, true] {
        let (mut renderer, _) = setup();
        let target = renderer.create_target(16, 16, "target");
        let pixel = renderer.create_target(16, 16, "pixel");
        let mask = renderer.create_target(16, 16, "mask");
        let scratch = renderer.create_target(16, 16, "scratch");
        let mut encoder = renderer.encoder();
        for producer in [&pixel, &mask] {
            renderer.try_draw(&mut encoder, producer, Some([0.0; 4]), &[]).unwrap();
        }
        if scratch_fallback {
            renderer.try_group_composite_via_scratch(&mut encoder, &target, &pixel, &mask,
                &scratch, Blend::Alpha, 1.0).unwrap();
        } else {
            renderer.try_group_composite(&mut encoder, &target, &pixel, &mask, Blend::Alpha, 1.0).unwrap();
        }
        assert_eq!(encoder.commands.iter().filter(|command| matches!(command,
            Command::Begin { clear: Some(color), .. } if *color == [0.0; 4])).count(), 2);
        assert_eq!(encoder.commands.iter().filter(|command| matches!(command, Command::Copy { .. })).count(),
            usize::from(scratch_fallback));
        let draws: Vec<_> = encoder.commands.iter().filter_map(|command| match command {
            Command::Draw { vertices, blend, .. } => Some((vertices, blend)), _ => None,
        }).collect();
        assert_eq!(draws.len(), 1 + usize::from(scratch_fallback));
        if scratch_fallback && let Some((vertices, blend)) = draws.first() {
            assert_eq!(**blend, EffectBlend::MaskAlpha);
            assert_eq!(vertices[0].pos, [-1.0, 1.0]);
            assert_eq!(vertices[2].pos, [1.0, -1.0]);
        }
    }
}

#[test]
fn group_bounds_restore_full_composite_when_inputs_change_after_crop_recording() {
    let _options = OptimizationScope::set(knife_perf::optimizations::GROUP_BOUNDS);
    for scratch_fallback in [false, true] {
        let (mut renderer, control) = setup();
        let target = renderer.create_target(16, 16, "target");
        let pixel = renderer.create_target(16, 16, "pixel");
        let mask = renderer.create_target(16, 16, "mask");
        let scratch = renderer.create_target(16, 16, "scratch");
        let mut init = renderer.encoder();
        for producer in [&pixel, &mask] {
            renderer.try_draw(&mut init, producer, Some([0.0; 4]), &[
                Quad::rect(renderer.white.clone(), [2.0, 2.0, 2.0, 2.0], [1.0; 4]),
            ]).unwrap();
        }
        renderer.try_present(init, &target).unwrap();
        control.borrow_mut().events.clear();
        let mut encoder = renderer.encoder();
        if scratch_fallback {
            renderer.try_group_composite_via_scratch(&mut encoder, &target, &pixel, &mask,
                &scratch, Blend::Alpha, 0.5).unwrap();
        } else {
            renderer.try_group_composite(&mut encoder, &target, &pixel, &mask, Blend::Alpha, 0.5).unwrap();
        }
        let vertices = encoder.commands.iter().filter_map(|command| match command {
            Command::Draw { vertices, .. } => Some(vertices), _ => None,
        }).last().unwrap();
        assert_eq!(vertices[0].pos, [-0.75, 0.75], "recording should initially use the proven crop");
        renderer.try_write_region(&pixel.tex, [12, 12, 1, 1], 4, &[255; 4]).unwrap();
        renderer.try_present(encoder, &target).unwrap();
        let control = control.borrow();
        let vertices = control.events.iter().filter_map(|event| match event {
            Event::Draw { vertices, .. } => Some(vertices), _ => None,
        }).last().unwrap();
        assert_eq!(vertices[0].pos, [-1.0, 1.0]);
        assert_eq!(vertices[2].pos, [1.0, -1.0]);
        assert_eq!(vertices[0].uv, [0.0, 0.0]);
        assert_eq!(vertices[2].uv, [1.0, 1.0]);
        assert_eq!(vertices[0].color, [1.0, 1.0, 1.0, 0.5]);
        assert_eq!(target.tex.native.coverage_for(0), CoverageBound::Unknown,
            "a widened fallback cannot publish the narrower predicted output");
    }
}
