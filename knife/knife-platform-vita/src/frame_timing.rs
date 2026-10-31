//! Fixed-size, allocation-free frame timing windows. Work excludes the frame pacer.
// 12,000..16,667 used to be one 4.7 ms bucket, which cannot resolve a 16.67 ms
// budget; the 1 ms steps below it narrow the report to +-0.5 ms.
const BOUNDS: [u64; 25] = [1_000, 2_000, 4_000, 8_000, 12_000, 13_000, 14_000, 15_000,
    16_000, 16_667, 25_000, 33_333, 50_000, 100_000, 125_000, 150_000, 200_000, 250_000, 333_333, 500_000,
    750_000, 1_000_000, 1_500_000, 2_000_000, u64::MAX];

#[derive(Default, Clone, Copy)]
pub struct FrameSample {
    pub frames: u64,
    pub work_us: u64,
    pub update_us: u64,
    pub render_us: u64,
    pub wall_us: u64,
    pub max_work_us: u64,
    pub over_16ms: u64,
    pub over_33ms: u64,
    /// Histogram upper bound, rather than a falsely precise percentile.
    pub p50_work_upper_us: u64,
    pub p95_work_upper_us: u64,
    pub p99_work_upper_us: u64,
}

#[derive(Default)]
pub struct FrameWindow {
    sample: FrameSample,
    buckets: [u64; BOUNDS.len()],
}
impl FrameWindow {
    pub fn record(&mut self, work: u64, update: u64, render: u64, wall: u64) {
        let s = &mut self.sample;
        s.frames += 1;
        s.work_us = s.work_us.saturating_add(work);
        s.update_us = s.update_us.saturating_add(update);
        s.render_us = s.render_us.saturating_add(render);
        s.wall_us = s.wall_us.saturating_add(wall);
        s.max_work_us = s.max_work_us.max(work);
        s.over_16ms += u64::from(work > 16_667);
        s.over_33ms += u64::from(work > 33_333);
        let index = BOUNDS.iter().position(|&upper| work <= upper).unwrap();
        self.buckets[index] += 1;
    }

    pub fn take(&mut self) -> FrameSample {
        let mut result = self.sample;
        if result.frames > 0 {
            let percentile = |percent: u64| {
                let rank = (result.frames * percent).div_ceil(100);
                let mut count = 0;
                for (index, &bucket) in self.buckets.iter().enumerate() {
                    count += bucket;
                    if count >= rank {
                        return if BOUNDS[index] == u64::MAX { result.max_work_us } else { BOUNDS[index] };
                    }
                }
                result.max_work_us
            };
            result.p50_work_upper_us = percentile(50);
            result.p95_work_upper_us = percentile(95);
            result.p99_work_upper_us = percentile(99);
        }
        *self = Self::default();
        result
    }
}
