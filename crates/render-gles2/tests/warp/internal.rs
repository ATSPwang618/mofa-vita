#[test]
fn lens_scale_iteration_cap_preserves_every_cycle() {
    let next = |x: i32| (x >> 1).wrapping_mul(x >> 1) >> 14;
    for initial in -131072i32..=131071 {
        let mut at = initial;
        for _ in 0..37 {
            at = next(at);
        }
        let entered = at;
        for _ in 0..8 {
            at = next(at);
        }
        assert_eq!(at, entered, "cycle after maximum tail, initial {initial}");
    }
    assert_eq!(super::lens_iterations(u32::MAX), 38);
}
