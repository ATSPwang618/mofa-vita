use super::*;
use super::test_backend::Event;

struct Options(u64);
impl Options {
    fn set(mask: u64) -> Self {
        let old = knife_perf::optimizations::mask();
        knife_perf::optimizations::set(mask);
        Self(old)
    }
}
impl Drop for Options {
    fn drop(&mut self) { knife_perf::optimizations::set(self.0); }
}

#[test]
fn blur_weights_route_all_modes_only_with_option_capability_and_bounded_kernel() {
    for (enabled, available, sigma, expected) in [
        (true, true, 0.0, 53u32), (true, true, 5.0, 53),
        (true, true, 5.4, 10), (true, false, 3.5, 10),
        (false, true, 3.5, 10),
    ] {
        let _options = Options::set(if enabled { knife_perf::optimizations::BLUR_WEIGHTS } else { 0 });
        let gxm = Gxm::new().unwrap();
        let control = gxm.control.clone();
        control.borrow_mut().shader_capabilities = if available { 1u64 << 53 } else { 0 };
        let mut renderer = Renderer::new_with_gxm(gxm).unwrap();
        let src = renderer.create_target(8, 8, "blur source");
        let dst = renderer.create_target(8, 8, "blur destination");
        for mode in [BlurMode::Uv, BlurMode::FullAdd, BlurMode::White] {
            control.borrow_mut().events.clear();
            control.borrow_mut().blur_weight_uploads.clear();
            let mut encoder = renderer.encoder();
            renderer.try_blur(&mut encoder, &src.tex, &dst, [1.25, -0.5], sigma, mode).unwrap();
            renderer.try_present(encoder, &dst).unwrap();
            let trace = control.borrow();
            let (shader, params) = trace.events.iter().find_map(|event| match event {
                Event::Draw { shader, params, .. } => Some((*shader as u32, *params)),
                _ => None,
            }).unwrap();
            assert_eq!(shader, expected, "enabled={enabled} available={available} sigma={sigma}");
            assert_eq!(params[2], sigma);
            assert_eq!(params[3], (sigma * 3.0).ceil());
            assert_eq!(params[4], mode as u32 as f32);
            if expected == 53 {
                let n = params[3] as usize;
                let upload = &trace.blur_weight_uploads;
                assert_eq!(upload.len(), 1, "one draw-scoped weight upload");
                assert_eq!(upload[0].0.to_bits(), sigma.max(0.01).to_bits());
                assert_eq!(upload[0].1, n);
                let expected = knife_render_api::experiments::compute_blur_weights_exact(sigma.max(0.01), n).unwrap();
                assert_eq!(upload[0].2.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                    expected.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
            } else {
                assert!(trace.blur_weight_uploads.is_empty(), "fallback performs no weight upload");
            }
        }
    }
}
