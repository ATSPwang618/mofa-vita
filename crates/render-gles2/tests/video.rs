#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    graphics::{DrawFace, Fill, Size},
    pixels::{Bytes, Yuv420, Yuv420Layout},
};
use krkr_render_gles2::{Config, Gpu};

#[test]
fn nv12_pairs_match_planar_chroma_across_tiles_and_layout_switches() {
    let size = Size {
        width: 18,
        height: 10,
    };
    let luma = (size.width * size.height) as usize;
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    tile_edge: 8,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let mut frames = Vec::new();
        for layout in [Yuv420Layout::Nv12, Yuv420Layout::Yv12, Yuv420Layout::Nv12] {
            let mut data = Bytes::zeroed(Yuv420::byte_len(size).unwrap(), &gpu.staging).unwrap();
            for (i, y) in data.as_mut_slice()[..luma].iter_mut().enumerate() {
                *y = 16 + (i * 23 % 220) as u8;
            }
            for i in 0..luma / 4 {
                let u = 16 + (i * 53 % 225) as u8;
                let v = 16 + (i * 97 % 225) as u8;
                match layout {
                    Yuv420Layout::Nv12 => {
                        data.as_mut_slice()[luma + 2 * i..luma + 2 * i + 2]
                            .copy_from_slice(&[u, v]);
                    }
                    Yuv420Layout::Yv12 => {
                        data.as_mut_slice()[luma + i] = v;
                        data.as_mut_slice()[luma + luma / 4 + i] = u;
                    }
                }
            }
            frames.push(
                gpu.upload_yuv(&Yuv420 { size, layout, data }, size)
                    .unwrap(),
            );
        }
        for frame in &frames {
            let rgba = gpu.readback(frame, size.rect(), false).unwrap();
            for (i, pixel) in rgba.data.as_slice().as_chunks::<4>().0.iter().enumerate() {
                let x = i % size.width as usize;
                let y = i / size.width as usize;
                let chroma = y / 2 * (size.width as usize / 2) + x / 2;
                let luminance = 1.164383 * (i * 23 % 220) as f64;
                let u = f64::from(16 + (chroma * 53 % 225) as u8) - 128.;
                let v = f64::from(16 + (chroma * 97 % 225) as u8) - 128.;
                let expected = [
                    luminance + 1.596027 * v,
                    luminance - 0.391762 * u - 0.812968 * v,
                    luminance + 2.017232 * u,
                    255.,
                ];
                for (actual, expected) in pixel.iter().zip(expected) {
                    let expected = expected.round().clamp(0., 255.) as u8;
                    assert!(
                        actual.abs_diff(expected) <= 1,
                        "work={work} pixel={i}: {pixel:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn both_yuv_layouts_preserve_color_orientation_compact_storage_and_split_alpha() {
    for work in [false, true] {
        for layout in [Yuv420Layout::Yv12, Yuv420Layout::Nv12] {
            check_layout(work, layout);
        }
    }
}

fn check_layout(work: bool, layout: Yuv420Layout) {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                work_framebuffer: work,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 4,
        height: 4,
    };
    // Red/white above blue/black exercises plane order and vertical orientation.
    let mut data = Bytes::zeroed(24, &gpu.staging).unwrap();
    data.as_mut_slice().copy_from_slice(&[
        81, 81, 235, 235, 81, 81, 235, 235, 41, 41, 16, 16, 41, 41, 16, 16, 240, 128, 110, 128, 90,
        128, 240, 128,
    ]);
    if layout == Yuv420Layout::Nv12 {
        data.as_mut_slice()[16..].copy_from_slice(&[90, 240, 128, 128, 240, 110, 128, 128]);
    }
    let pixels = Yuv420 { size, layout, data };
    let logical = Size {
        width: 8,
        height: 8,
    };
    let image = gpu.upload_yuv(&pixels, logical).unwrap();
    assert_eq!(image.stored_size(), Some(size));
    let rgba = gpu.readback(&image, logical.rect(), false).unwrap();
    for (i, p) in rgba.data.as_slice().as_chunks::<4>().0.iter().enumerate() {
        let expected: [u8; 4] = if i / 8 >= 4 {
            if i % 8 < 4 {
                [0, 0, 255, 255]
            } else {
                [0, 0, 0, 255]
            }
        } else if i % 8 < 4 {
            [254, 0, 0, 255]
        } else {
            [255, 255, 255, 255]
        };
        assert!(
            p.iter().zip(expected).all(|(&a, b)| a.abs_diff(b) <= 1),
            "{i}: {p:?}"
        );
    }
    let mut target = gpu.create_image(logical, 0x37102030).unwrap();
    gpu.copy_yuv(&mut target, &pixels, logical, true, logical)
        .unwrap();
    let rgba = gpu.readback(&target, logical.rect(), false).unwrap();
    for (i, p) in rgba.data.as_slice().as_chunks::<4>().0.iter().enumerate() {
        if i % 8 < 4 {
            if i / 8 < 4 {
                assert!(p[0] >= 253 && p[1] <= 1 && p[2] <= 1 && p[3] == 255);
            } else {
                assert!(p[0] <= 1 && p[1] <= 1 && p[2] >= 253 && p[3] == 0);
            }
        } else {
            assert_eq!(p, &[16, 32, 48, 55]);
        }
    }
    let malformed = Yuv420 {
        size,
        layout,
        data: Bytes::zeroed(11, &gpu.staging).unwrap(),
    };
    assert!(gpu.upload_yuv(&malformed, logical).is_err());
}

#[test]
fn switching_video_plane_layouts_keeps_previous_frames_and_avoids_clear_and_reload() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 16,
        height: 8,
    };
    let mut frames = Vec::new();
    for layout in [Yuv420Layout::Nv12, Yuv420Layout::Yv12, Yuv420Layout::Nv12] {
        let mut data = Bytes::zeroed(Yuv420::byte_len(size).unwrap(), &gpu.staging).unwrap();
        data.as_mut_slice()[..128].fill(81);
        match layout {
            Yuv420Layout::Yv12 => {
                data.as_mut_slice()[128..160].fill(240);
                data.as_mut_slice()[160..].fill(90);
            }
            Yuv420Layout::Nv12 => {
                for uv in data.as_mut_slice()[128..].as_chunks_mut::<2>().0 {
                    uv.copy_from_slice(&[90, 240]);
                }
            }
        }
        traffic::reset();
        let frame = gpu
            .upload_yuv(&Yuv420 { size, layout, data }, size)
            .unwrap();
        gpu.resolve().unwrap();
        assert_eq!(traffic::clear_calls(), 0);
        assert!(
            traffic::loaded_pixels() <= 1,
            "conversion reloaded a whole output frame"
        );
        frames.push(frame);
    }
    for frame in frames {
        let pixels = gpu.readback(&frame, size.rect(), false).unwrap();
        assert!(
            pixels
                .data
                .as_slice()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[0] >= 253 && p[1] <= 1 && p[2] <= 1 && p[3] == 255)
        );
    }
}

#[test]
fn video_hit_masks_need_no_readback_and_later_alpha_writes_invalidate_them() {
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: work,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 16,
            height: 8,
        };
        let mut data = Bytes::zeroed(Yuv420::byte_len(size).unwrap(), &gpu.staging).unwrap();
        for (i, y) in data.as_mut_slice()[..128].iter_mut().enumerate() {
            *y = 16 + (i % 200) as u8;
        }
        data.as_mut_slice()[128..].fill(128);
        let video = gpu
            .upload_yuv(
                &Yuv420 {
                    size,
                    layout: Yuv420Layout::Nv12,
                    data,
                },
                size,
            )
            .unwrap();
        let mut target = gpu.create_image(size, 0).unwrap();
        gpu.copy_rect(
            &mut target,
            &video,
            size.rect(),
            0,
            0,
            size.rect(),
            DrawFace::Opaque,
            false,
        )
        .unwrap();
        traffic::reset();
        for image in [&video, &target] {
            let mask = gpu.read_hit_plane(image, false).unwrap();
            assert!(matches!(mask.data, krkr_protocol::hit::Data::Uniform(255)));
        }
        assert_eq!(traffic::read_calls(), 0);
        gpu.fill(
            &mut target,
            &[Fill {
                rectangle: krkr_protocol::graphics::Rect {
                    width: 1,
                    height: 1,
                    ..Default::default()
                },
                color: 0,
                face: DrawFace::Mask,
                hold_alpha: false,
            }],
        )
        .unwrap();
        let mask = gpu.read_hit_plane(&target, false).unwrap();
        assert_eq!(mask.sample(0, 0), 0);
        assert_eq!(mask.sample(1, 0), 255);
        assert!(matches!(
            gpu.read_hit_plane(&video, false).unwrap().data,
            krkr_protocol::hit::Data::Uniform(255)
        ));
    }
}

#[test]
fn retained_video_frames_do_not_exhaust_the_scene_target_cache() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                render_target_cache_entries: 4,
                render_target_cache_bytes: 128 * 1024,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 128,
        height: 128,
    };
    let mut retained = Vec::new();
    for n in 0..6 {
        let mut data = Bytes::zeroed(Yuv420::byte_len(size).unwrap(), &gpu.staging).unwrap();
        data.as_mut_slice()[..128 * 128].fill(32 + n * 20);
        data.as_mut_slice()[128 * 128..].fill(128);
        traffic::reset();
        let frame = gpu
            .upload_yuv(
                &Yuv420 {
                    size,
                    layout: Yuv420Layout::Nv12,
                    data,
                },
                size,
            )
            .unwrap();
        gpu.resolve().unwrap();
        // Scene targets have only one resident slot. Video owns reusable
        // attachments separately, so a retained frame cannot force later
        // conversions through a work-surface store.
        assert_eq!(traffic::store_calls(), 0);
        assert_eq!(traffic::load_calls(), 0);
        retained.push(frame);
    }
    let first = gpu.readback(&retained[0], size.rect(), false).unwrap();
    let last = gpu
        .readback(retained.last().unwrap(), size.rect(), false)
        .unwrap();
    assert!(first.data.as_slice()[0] < last.data.as_slice()[0]);
}

#[test]
fn device_sized_video_reuses_outputs_without_periodic_finish() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                render_target_cache_entries: 8,
                render_target_cache_bytes: 8 * 1024 * 1024,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 960,
        height: 544,
    };
    let logical = Size {
        width: 1920,
        height: 1080,
    };
    let mut data = Bytes::zeroed(Yuv420::byte_len(size).unwrap(), &gpu.staging).unwrap();
    data.as_mut_slice()[..960 * 544].fill(32);
    data.as_mut_slice()[960 * 544..].fill(128);
    let mut pixels = Yuv420 {
        size,
        layout: Yuv420Layout::Nv12,
        data,
    };
    let retained = gpu.upload_yuv(&pixels, logical).unwrap();
    let mut current = None;
    for frame in 0..24 {
        pixels.data.as_mut_slice()[..960 * 544].fill(64 + frame);
        traffic::reset();
        current = Some(gpu.upload_yuv(&pixels, logical).unwrap());
        gpu.resolve().unwrap();
        gpu.maintain().unwrap();
        assert_eq!(traffic::finish_calls(), 0, "frame {frame}");
        assert_eq!(traffic::store_calls(), 0, "frame {frame}");
        assert_eq!(traffic::load_calls(), 0, "frame {frame}");
        if frame >= 2 {
            // Y and UV uploads orphan their two in-flight sample planes;
            // the RGBA output must not allocate storage again.
            assert_eq!(traffic::texture_allocations(), 2, "frame {frame}");
        }
    }
    let probe = krkr_protocol::graphics::Rect {
        width: 1,
        height: 1,
        ..Default::default()
    };
    let first = gpu.readback(&retained, probe, false).unwrap();
    let last = gpu
        .readback(current.as_ref().unwrap(), probe, false)
        .unwrap();
    assert!(first.data.as_slice()[0] < last.data.as_slice()[0]);
    assert_eq!(retained.size, logical);
    assert_eq!(current.unwrap().stored_size(), Some(size));
}
