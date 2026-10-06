use krkr_protocol::{
    budget::Budget,
    transform::{Filter, Sampling},
};
use krkr_render::resample::{Axis, Cache};
use std::sync::Arc;

#[test]
fn cache_uses_exact_geometry_coefficients_and_budget_ownership() {
    let budget = Budget::new(4 * 1024 * 1024);
    let other = Budget::new(4 * 1024 * 1024);
    let mut cache = Cache::default();
    for filter in [
        Filter::Linear,
        Filter::Cubic,
        Filter::Lanczos2,
        Filter::Lanczos3,
        Filter::Spline16,
        Filter::Spline36,
        Filter::Gaussian,
        Filter::Blackman,
        Filter::Area,
    ] {
        for (src, dst, visible) in [(0, 64, 0), (3, -64, -60), (5, 63, 7)] {
            let sampling = Sampling {
                filter,
                sharpness: -0.75,
                no_clip: false,
            };
            let axis = cache
                .get(src, 127, 0, dst, visible, 48, sampling, &budget)
                .unwrap();
            let expected = Axis::new(src, 127, 0, dst, visible, 48, sampling, &budget).unwrap();
            assert_eq!(axis.range, expected.range);
            assert!(
                axis.data
                    .iter()
                    .zip(&expected.data)
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
            assert_eq!(axis.data.len(), expected.data.len());
            let same = cache
                .get(src, 127, 0, dst, visible, 48, sampling, &budget)
                .unwrap();
            assert!(Arc::ptr_eq(&axis, &same));
            let different = cache
                .get(src, 127, 0, dst, visible, 48, sampling, &other)
                .unwrap();
            assert!(!Arc::ptr_eq(&axis, &different));
            let sharp = cache
                .get(
                    src,
                    127,
                    0,
                    dst,
                    visible,
                    48,
                    Sampling {
                        sharpness: -0.5,
                        ..sampling
                    },
                    &budget,
                )
                .unwrap();
            assert!(!Arc::ptr_eq(&axis, &sharp));
        }
    }
    cache.clear();
    assert_eq!(budget.used(), 0);
    assert_eq!(other.used(), 0);
}

#[test]
fn tiny_pool_does_not_retain_axes_and_cached_reservations_can_be_reclaimed() {
    let sampling = Sampling {
        filter: Filter::Cubic,
        sharpness: -1.,
        no_clip: false,
    };
    let budget = Budget::new(4096);
    let mut cache = Cache::default();
    drop(cache.get(0, 16, 0, 8, 0, 8, sampling, &budget).unwrap());
    assert_eq!(budget.used(), 0);
    let budget = Budget::new(1024 * 1024);
    drop(cache.get(0, 16, 0, 8, 0, 8, sampling, &budget).unwrap());
    assert!(budget.used() > 0);
    let blocker = budget.reserve(budget.available()).unwrap();
    drop(cache.get(1, 16, 0, 8, 0, 8, sampling, &budget).unwrap());
    cache.clear();
    drop(blocker);
    assert_eq!(budget.used(), 0);
}
