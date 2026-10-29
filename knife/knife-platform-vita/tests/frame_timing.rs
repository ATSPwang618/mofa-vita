#[path = "../src/frame_timing.rs"]
mod frame_timing;
use frame_timing::FrameWindow;

#[test]
fn records_work_separately_from_pacing_and_resets_each_window() {
    let mut window = FrameWindow::default();
    window.record(4_000, 1_000, 2_000, 16_667);
    window.record(40_000, 30_000, 8_000, 45_000);
    let sample = window.take();
    assert_eq!(sample.frames, 2);
    assert_eq!(sample.work_us, 44_000);
    assert_eq!(sample.update_us, 31_000);
    assert_eq!(sample.render_us, 10_000);
    assert_eq!(sample.wall_us, 61_667);
    assert_eq!(sample.over_16ms, 1);
    assert_eq!(sample.over_33ms, 1);
    assert_eq!(sample.max_work_us, 40_000);
    assert_eq!(sample.p95_work_upper_us, 50_000);
    assert_eq!(window.take().frames, 0);
}

#[test]
fn p95_is_a_bucket_bound_and_long_outliers_are_not_discarded() {
    let mut window = FrameWindow::default();
    for _ in 0..95 { window.record(2_000, 0, 0, 16_667); }
    for _ in 0..4 { window.record(30_000, 0, 0, 33_333); }
    window.record(2_000_000, 0, 0, 2_000_000);
    let sample = window.take();
    assert_eq!(sample.p95_work_upper_us, 2_000);
    assert_eq!(sample.max_work_us, 2_000_000);
    assert_eq!(sample.over_16ms, 5);
    let mut single = FrameWindow::default();
    single.record(2_000_000, 0, 0, 2_000_000);
    assert_eq!(single.take().p95_work_upper_us, 2_000_000);
}
