#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
use krkr_protocol::{
    budget::Budget,
    graphics::{DrawFace, Fill, Rect, Size},
    pixels::Bytes,
    transition::{
        Effect, Frame,
        custom::{self, Instance, Payload},
    },
};
use krkr_render_gles2::{Config, Gpu, Image};
use std::sync::Arc;

#[derive(Debug)]
struct Fixture {
    parameters: [u32; 16],
    table: Arc<Bytes>,
}
impl Instance for Fixture {
    fn kernel(&self) -> &'static str {
        "krkr.extrans.v1"
    }
    fn prepare(&self, _: Size, _: u64, _: u64, _: &Budget) -> Result<Payload, String> {
        Ok(Payload {
            parameters: self.parameters,
            table: self.table.clone(),
        })
    }
}
fn effect(parameters: [i32; 16], words: &[i32]) -> custom::Frame {
    let mut table = Bytes::zeroed(words.len() * 4, &Budget::new(2 * 1024 * 1024)).unwrap();
    for (out, value) in table
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(words)
    {
        out.copy_from_slice(&value.to_le_bytes());
    }
    custom::Frame {
        instance: Arc::new(Fixture {
            parameters: parameters.map(|p| p as u32),
            table: Arc::new(table),
        }),
        elapsed: 500,
        duration: 1000,
        lifetime: Arc::new(()),
    }
}
fn image(gpu: &Gpu, size: Size, colors: &[u32]) -> Image {
    let mut image = gpu.create_image(size, 0).unwrap();
    let fills: Vec<_> = colors
        .iter()
        .enumerate()
        .map(|(i, &color)| Fill {
            rectangle: Rect {
                left: (i as u32 % size.width) as i32,
                top: (i as u32 / size.width) as i32,
                width: 1,
                height: 1,
            },
            color,
            face: DrawFace::Alpha,
            hold_alpha: false,
        })
        .collect();
    gpu.fill(&mut image, &fills).unwrap();
    image
}
fn read(gpu: &Gpu, image: &Image) -> Vec<u32> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| u32::from_be_bytes([p[3], p[0], p[1], p[2]]))
        .collect()
}

#[test]
fn registered_kernel_renders_wave_mosaic_turn_rotate_and_ripple_across_tiles() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 4,
        height: 2,
    };
    let a: Vec<_> = (0..8).map(|i| 0x80200000 + i * 0x10000).collect();
    let b: Vec<_> = (0..8).map(|i| 0xc0006000 + i * 0x100).collect();
    let first = image(&gpu, size, &a);
    let second = image(&gpu, size, &b);
    let mut target = gpu.create_image(size, 0).unwrap();
    let frame = Frame {
        effect: Effect::Custom,
        face: DrawFace::Opaque,
        size,
        phase: 1,
    };
    let mut params = [0; 16];
    params[1] = 128;
    params[2] = 0xff0a0b0cu32 as i32;
    let wave = effect(params, &[1, -1]);
    gpu.transition_with_custom(&mut target, &first, &second, None, frame, Some(&wave))
        .unwrap();
    assert_eq!(
        read(&gpu, &target),
        [
            0xff0a0b0c, 0x00103000, 0x00103000, 0x00113100, 0x00123200, 0x00133300, 0x00133300,
            0xff0a0b0c
        ]
    );
    params[0] = 1;
    params[3] = 2;
    let mosaic = effect(params, &[0]);
    gpu.transition_with_custom(&mut target, &first, &second, None, frame, Some(&mosaic))
        .unwrap();
    assert_eq!(
        read(&gpu, &target),
        [0xa0123200, 0xa0123200, 0xa0133300, 0xa0133300].repeat(2)
    );
    params[0] = 2;
    params[3] = 8;
    let mut turn = vec![0; 9 * 64 * 6];
    for y in 0..2 {
        let at = (8 * 64 + y) * 6;
        turn[at..at + 6].copy_from_slice(&[0, 4, 0, (y as i32) * 65536, 65536, 0]);
    }
    let turn = effect(params, &turn);
    gpu.transition_with_custom(&mut target, &first, &second, None, frame, Some(&turn))
        .unwrap();
    assert_eq!(
        read(&gpu, &target),
        [
            0x20c7bfbf, 0x20c7bfbf, 0x20c7bfbf, 0x20c8bfbf, 0x20c8bfbf, 0x20c8bfbf, 0x20c8bfbf,
            0x20c9bfbf
        ]
    );
    params[0] = 3;
    params[3] = 2;
    let mut rotate = vec![0; 2 * 16];
    for y in 0..2 {
        rotate[y * 16..y * 16 + 8].copy_from_slice(&[
            0,
            4,
            3 * 65536 - 1,
            y as i32 * 65536,
            -65536,
            0,
            0,
            0,
        ]);
        rotate[y * 16 + 8..y * 16 + 16].copy_from_slice(&[
            2,
            4,
            0,
            y as i32 * 65536,
            65536,
            0,
            0,
            0,
        ]);
    }
    let rotate = effect(params, &rotate);
    gpu.transition_with_custom(&mut target, &first, &second, None, frame, Some(&rotate))
        .unwrap();
    assert_eq!(
        read(&gpu, &target),
        [a[2], a[1], b[0], b[1], a[6], a[5], b[4], b[5]]
    );
    params[0] = 4;
    params[3] = 2;
    params[4] = 1;
    params[5] = 16;
    params[6] = 2;
    params[7] = 2;
    params[8] = 0;
    params[9] = 4;
    let mut ripple = vec![0; 2 + 16 + 64];
    ripple[2..2 + 16 + 32].fill(2048); // wave=2048, cos=2048, sin=0 -> one horizontal pixel
    let ripple = effect(params, &ripple);
    gpu.transition_with_custom(&mut target, &first, &second, None, frame, Some(&ripple))
        .unwrap();
    assert_eq!(
        read(&gpu, &target),
        [
            0x00103000, 0x00113100, 0x00103000, 0x00113100, 0x00123200, 0x00133300, 0x00123200,
            0x00133300
        ]
    );
    for custom in [&wave, &mosaic, &turn, &rotate, &ripple] {
        use krkr_protocol::{
            graphics::{Blend, ImageRef, Node, Scene},
            transition::SceneTransition,
        };
        let mut ids = slotmap::SlotMap::with_key();
        let a = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let b = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let images =
            std::collections::HashMap::from([(a.id, first.shared()), (b.id, second.shared())]);
        let node = |image, visible| Node {
            cache: None,
            visible,
            parent: None,
            image: Some(image),
            neutral_color: 0,
            rectangle: size.rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
            opacity: 255,
        };
        let scene = Scene {
            nodes: vec![node(a, true), node(b, false)],
            transitions: vec![SceneTransition {
                destination: 0,
                source: 1,
                with_children: false,
                frame,
                rule: None,
                custom: Some(custom.clone()),
            }],
            ..Default::default()
        };
        let full = gpu.scene_surface(size, &scene, &images, (0, 0)).unwrap();
        let expected = read(&gpu, &full);
        let scaled = gpu
            .scene_surface_scaled(
                size,
                Size {
                    width: 3,
                    height: 1,
                },
                &scene,
                &images,
            )
            .unwrap();
        assert_eq!(read(&gpu, &scaled), [expected[4], expected[6], expected[7]]);
    }
    let lease = Arc::downgrade(&ripple.lifetime);
    drop(ripple);
    assert!(
        lease.upgrade().is_none(),
        "uploaded transition tables do not retain their CPU provider"
    );
    gpu.collect().unwrap();
    assert!(lease.upgrade().is_none());
}

#[test]
fn custom_frames_release_cpu_data_without_a_per_frame_finish() {
    use std::{cell::Cell, ffi::c_void};
    type Finish = unsafe extern "system" fn();
    thread_local! {
        static FINISH: Cell<Option<Finish>> = const { Cell::new(None) };
        static WAITS: Cell<usize> = const { Cell::new(0) };
    }
    unsafe extern "system" fn finish() {
        WAITS.set(WAITS.get() + 1);
        unsafe { FINISH.get().unwrap()() };
    }
    let context = support::Context::new();
    let gl = context.gl_with(|name, pointer| {
        if name == "glFinish" {
            FINISH.set(Some(unsafe {
                std::mem::transmute::<*const c_void, Finish>(pointer)
            }));
            finish as *const c_void
        } else {
            pointer
        }
    });
    let gpu = unsafe {
        Gpu::new(
            gl,
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 32,
        height: 4,
    };
    let first = gpu.create_image(size, 0xff842033).unwrap();
    let second = gpu.create_image(size, 0xff112299).unwrap();
    let mut target = gpu.create_image(size, 0).unwrap();
    let custom = effect([0; 16], &[0; 4]);
    let lifetime = Arc::downgrade(&custom.lifetime);
    let provider = Arc::downgrade(&custom.instance);
    WAITS.set(0);
    for _ in 0..8 {
        gpu.transition_with_custom(
            &mut target,
            &first,
            &second,
            None,
            Frame {
                effect: Effect::Custom,
                face: DrawFace::Alpha,
                size,
                phase: 1,
            },
            Some(&custom),
        )
        .unwrap();
        gpu.maintain().unwrap();
    }
    assert_eq!(WAITS.get(), 0, "transition CPU data forced a GPU finish");
    drop(custom);
    assert!(lifetime.upgrade().is_none());
    assert!(provider.upgrade().is_none());
    assert_eq!(read(&gpu, &target), vec![0xff842033; 128]);
}

#[test]
fn rotation_keeps_low_fixed_point_bits_at_large_scanline_deltas() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 127,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 258,
        height: 1,
    };
    let mut colors = vec![0xff000000; 258];
    colors[256] = 0xff00ff00;
    colors[257] = 0xffff0000;
    let source = image(&gpu, size, &colors);
    let background = gpu.create_image(size, 0xff0000ff).unwrap();
    let mut output = gpu.create_image(size, 0).unwrap();
    let mut parameters = [0; 16];
    parameters[0] = 3;
    parameters[3] = 1;
    let custom = effect(
        parameters,
        &[0, 258, 256, 0, 65535, 0, 0, 0, 0, 258, 0, 0, 0, 0, 0, 0],
    );
    gpu.transition_with_custom(
        &mut output,
        &source,
        &background,
        None,
        Frame {
            effect: Effect::Custom,
            face: DrawFace::Opaque,
            size,
            phase: 1,
        },
        Some(&custom),
    )
    .unwrap();
    let pixels = read(&gpu, &output);
    // 256 + 257*65535 is exactly one fixed-point unit below pixel 257.
    assert_eq!(&pixels[256..], [0xff00ff00, 0xff00ff00]);
}

#[test]
fn wave_alpha_rounding_and_background_survive_tiled_selection() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 4,
        height: 3,
    };
    let colors: Vec<_> = (0..12).map(|i| 0x11223344 + i * 0x11030201).collect();
    let reversed: Vec<_> = colors.iter().rev().copied().collect();
    let first = image(&gpu, size, &colors);
    let second = image(&gpu, size, &reversed);
    let mut output = gpu.create_image(size, 0).unwrap();
    let lookup = krkr_render::blend::lookup_table();
    let shifts = [-1, 0, 5];
    for face in [DrawFace::Opaque, DrawFace::Alpha, DrawFace::AddAlpha] {
        for opacity in [1, 127, 128, 254] {
            let mut parameters = [0; 16];
            parameters[1] = opacity;
            parameters[2] = 0xdeadbeefu32 as i32;
            let wave = effect(parameters, &shifts);
            let frame = Frame {
                effect: Effect::Custom,
                face,
                size,
                phase: 1,
            };
            gpu.transition_with_custom(&mut output, &first, &second, None, frame, Some(&wave))
                .unwrap();
            let expected: Vec<_> = (0..12)
                .map(|i| {
                    let x = i % 4 - shifts[(i / 4) as usize];
                    if !(0..4).contains(&x) {
                        return 0xdeadbeef;
                    }
                    let at = (i / 4 * 4 + x) as usize;
                    krkr_render::transition::blend(
                        Frame {
                            effect: Effect::CrossFade,
                            phase: opacity as u32,
                            ..frame
                        },
                        colors[at],
                        reversed[at],
                        0,
                        &lookup,
                    )
                })
                .collect();
            assert_eq!(read(&gpu, &output), expected, "{face:?} opacity={opacity}");
        }
    }
}

#[test]
fn turn_endpoints_bypass_empty_table_rows_and_preserve_background() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 4,
        height: 1,
    };
    let a = [0x11223344, 0x55667788, 0x99aabbcc, 0xddeeff00];
    let b = [0x12345678, 0x9abcdef0, 0x98765432, 0xabcdef01];
    let first = image(&gpu, size, &a);
    let second = image(&gpu, size, &b);
    let mut output = gpu.create_image(size, 0).unwrap();
    for (phase, expected) in [(0, a), (63, b), (8, [0xdeadbeef; 4])] {
        let mut parameters = [0; 16];
        parameters[0] = 2;
        parameters[2] = 0xdeadbeefu32 as i32;
        parameters[3] = phase;
        let turn = effect(parameters, &[0]);
        let frame = Frame {
            effect: Effect::Custom,
            face: DrawFace::Alpha,
            size,
            phase: 1,
        };
        gpu.transition_with_custom(&mut output, &first, &second, None, frame, Some(&turn))
            .unwrap();
        assert_eq!(read(&gpu, &output), expected, "phase={phase}");
    }
}
