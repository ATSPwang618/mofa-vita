use knife_render_api::experiments::{
    compute_blur_weights_exact, probe_tiled_target, select_scene_capacity,
    BlurWeightCache, BlurWeightResult, ExperimentFeatures, HalfColorDecision,
    HalfColorSample, ReadbackPlan, SceneCapacity, SceneCapacityError, TargetFormat,
    TargetStorageKind, TiledTargetProbe,
    validate_half_color_candidate,
};

#[test]
fn half_color_candidate_preserves_inputs_and_rejects_rgba8_byte_diff() {
    let sample = HalfColorSample {
        uv: [0.125, 0.875],
        wpos: [13.25, 17.5],
        flags: 37.0,
        denominator: 2.5,
        native_uchar4: [1, 2, 3, 4],
        candidate_rgba8: [1, 2, 3, 5],
    };

    let report = validate_half_color_candidate(sample);
    assert_eq!(report.decision, HalfColorDecision::RejectedRgba8ByteDiff { index: 3 });
    assert_eq!(report.uv, sample.uv);
    assert_eq!(report.wpos, sample.wpos);
    assert_eq!(report.flags, sample.flags);
    assert_eq!(report.denominator, sample.denominator);
    assert_eq!(report.native_uchar4, sample.native_uchar4);
    assert!(!report.default_promotion_applied);
}

#[test]
fn half_color_candidate_accepts_exact_rgba8_bytes_without_promotion() {
    let sample = HalfColorSample {
        uv: [0.0, 1.0],
        wpos: [4.0, 8.0],
        flags: 0.0,
        denominator: 1.0,
        native_uchar4: [255, 128, 64, 32],
        candidate_rgba8: [255, 128, 64, 32],
    };
    let report = validate_half_color_candidate(sample);
    assert_eq!(report.decision, HalfColorDecision::Accepted);
    assert!(!report.default_promotion_applied);
}

#[test]
fn tiled_probe_only_admits_data_null_rgba8_render_targets_and_accounts_padding() {
    let input = knife_render_api::experiments::TiledTargetProbeInput {
        data_is_null: true,
        format: TargetFormat::Rgba8,
        render_target: true,
        width: 5,
        height: 3,
        tile_width: 4,
        tile_height: 2,
        linear_readback_available: true,
    };
    let TiledTargetProbe::Candidate(candidate) = probe_tiled_target(input) else {
        panic!("expected tiled RGBA8 candidate");
    };
    assert_eq!(candidate.storage, TargetStorageKind::TiledRgba8Candidate);
    assert_eq!(candidate.layout.logical_bytes, 5 * 3 * 4);
    assert_eq!(candidate.layout.padded_bytes, 8 * 4 * 4);
    assert_eq!(candidate.layout.padding_bytes, candidate.layout.padded_bytes - candidate.layout.logical_bytes);
    assert_eq!(candidate.readback, ReadbackPlan::LinearStaging { bytes: 5 * 3 * 4, row_bytes: 5 * 4 });

    for (format, data_is_null, render_target) in [
        (TargetFormat::Rgba8, false, true),
        (TargetFormat::R8, true, true),
        (TargetFormat::Compressed, true, true),
        (TargetFormat::Rgba8, true, false),
    ] {
        let result = probe_tiled_target(knife_render_api::experiments::TiledTargetProbeInput {
            data_is_null,
            format,
            render_target,
            ..input
        });
        assert!(matches!(result, TiledTargetProbe::Fallback { .. }));
    }
}

#[test]
fn tiled_probe_falls_back_when_linear_readback_staging_is_unavailable() {
    let result = probe_tiled_target(knife_render_api::experiments::TiledTargetProbeInput {
        data_is_null: true,
        format: TargetFormat::Rgba8,
        render_target: true,
        width: 16,
        height: 16,
        tile_width: 8,
        tile_height: 8,
        linear_readback_available: false,
    });
    assert!(matches!(result, TiledTargetProbe::Fallback { .. }));
}

#[test]
fn blur_cache_preserves_exact_k_order_formula_and_bounded_storage() {
    let sigma = 2.25;
    let expected = compute_blur_weights_exact(sigma, 2).unwrap();
    assert_eq!(expected.len(), 5);
    assert_eq!(expected[0].to_bits(), expected[4].to_bits());
    assert_eq!(expected[1].to_bits(), expected[3].to_bits());
    let expected_center = {
        let norm = (2.0_f32 * sigma * sigma * 3.1416_f32).sqrt();
        (-(0.0_f32) / (2.0 * sigma * sigma)).exp() / norm
    };
    assert_eq!(expected[2].to_bits(), expected_center.to_bits());

    let mut cache = BlurWeightCache::default();
    let first = cache.get_or_compute(sigma, 2);
    assert!(matches!(first, BlurWeightResult::Cached(_)));
    let second = cache.get_or_compute(sigma, 2);
    assert!(matches!(second, BlurWeightResult::Cached(_)));
    assert_eq!(cache.group_count(), 1);
    assert!(cache.bytes_used() <= BlurWeightCache::MAX_BYTES);

    assert!(matches!(cache.get_or_compute(1.0, 17), BlurWeightResult::Fallback { .. }));
    for i in 0..32 {
        let _ = cache.get_or_compute(0.5 + i as f32 * 0.125, 16);
    }
    assert!(cache.group_count() <= BlurWeightCache::MAX_GROUPS);
    assert!(cache.bytes_used() <= BlurWeightCache::MAX_BYTES);
}

#[test]
fn scene_capacity_selector_cleans_up_failed_candidate_and_falls_back_to_one() {
    let mut cleanup_calls = 0;
    let selected = select_scene_capacity(
        SceneCapacity::Four,
        |capacity| {
            if capacity == SceneCapacity::Four {
                Err("driver memory")
            } else {
                Ok(())
            }
        },
        |_capacity| Ok::<_, &str>(()),
        || cleanup_calls += 1,
    )
    .unwrap();
    assert_eq!(selected.selected, SceneCapacity::One);
    assert!(selected.used_fallback);
    assert_eq!(cleanup_calls, 1);
}

#[test]
fn scene_capacity_selector_cleans_up_partial_allocation_and_reports_fallback_failure() {
    let mut cleanup_calls = 0;
    let error = select_scene_capacity(
        SceneCapacity::Two,
        |_capacity| Ok::<_, &str>(()),
        |capacity| {
            if capacity == SceneCapacity::Two { Err("partial allocation") } else { Err("one failed") }
        },
        || cleanup_calls += 1,
    )
    .unwrap_err();
    assert!(matches!(error, SceneCapacityError::FallbackAllocationFailed { .. }));
    assert_eq!(cleanup_calls, 2);
}

#[test]
fn all_experiment_gates_are_off_by_default() {
    assert_eq!(ExperimentFeatures::default(), ExperimentFeatures::disabled());
}
