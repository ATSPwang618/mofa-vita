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
use krkr_render_gles2::{Config, Gpu};
use std::sync::Arc;
#[derive(Debug)]
struct Fixture {
    p: [u32; 16],
    words: Vec<i32>,
}
impl Instance for Fixture {
    fn kernel(&self) -> &'static str {
        "krkr.nagano.v1"
    }
    fn prepare(&self, _: Size, _: u64, _: u64, budget: &Budget) -> Result<Payload, String> {
        let mut table = Bytes::zeroed(self.words.len() * 4, budget).map_err(|e| e.to_string())?;
        for (chunk, n) in table
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(&self.words)
        {
            chunk.copy_from_slice(&n.to_le_bytes());
        }
        Ok(Payload {
            parameters: self.p,
            table: Arc::new(table),
        })
    }
}
fn render(edge: u32, fixture: Arc<Fixture>) -> Vec<u8> {
    let ctx = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            ctx.gl(),
            Config {
                tile_edge: edge,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 8,
        height: 6,
    };
    let mut a = gpu.create_image(size, 0).unwrap();
    let mut b = gpu.create_image(size, 0).unwrap();
    for (image, base) in [(&mut a, 0x80302010u32), (&mut b, 0xf0607050u32)] {
        let fills: Vec<_> = (0..48u32)
            .map(|i| Fill {
                rectangle: Rect {
                    left: (i % 8) as i32,
                    top: (i / 8) as i32,
                    width: 1,
                    height: 1,
                },
                color: base + i * 0x020101,
                face: DrawFace::Alpha,
                hold_alpha: false,
            })
            .collect();
        gpu.fill(image, &fills).unwrap();
    }
    let mut output = gpu.create_image(size, 0xffff00ff).unwrap();
    let custom = custom::Frame {
        instance: fixture,
        elapsed: 500,
        duration: 1000,
        lifetime: Arc::new(()),
    };
    gpu.transition_with_custom(
        &mut output,
        &a,
        &b,
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
    gpu.readback(&output, size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}
#[test]
fn all_nagano_kernels_render_the_same_across_texture_boundaries() {
    for mode in 0..12 {
        for blur_type in 0..if mode == 11 { 2 } else { 1 } {
            let mut p = [0u32; 16];
            p[0] = mode;
            p[1] = 127;
            let mut words = vec![0];
            match mode {
                0 => {
                    words = (0..8).chain(0..6).flat_map(|n| [n, n]).collect();
                }
                1 => p[3] = 3,
                2 => p[4..8].copy_from_slice(&[45, 127, 211, 80]),
                3 => {
                    p[3] = 2;
                    p[4] = 0;
                    p[5] = 90;
                }
                4 => {
                    words = (0..8)
                        .flat_map(|x| [7 - x, 192, if x % 2 == 0 { 1 } else { 2 }])
                        .collect();
                }
                5 => {
                    p[3] = 2;
                    p[5] = 3;
                    p[6] = 6;
                    words = vec![2; 6];
                    words.extend((0..18).map(|i| (0xf0408020u32 + i * 0x00020402) as i32));
                }
                6 => {
                    p[2] = 0x60304050;
                    p[3] = 8;
                    p[4] = 1;
                    p[6] = 180;
                }
                7 => {
                    p[3] = 4;
                    p[4] = 1;
                    p[5] = 2;
                    p[6] = 6;
                }
                8 => {
                    p[3] = 1;
                    p[4] = 8;
                    p[5] = 65536;
                    p[6] = 765;
                    words = vec![4, 3, 6, 230];
                    for i in 0..8 {
                        words.extend([i % 3 - 1, i * 765 / 8]);
                    }
                }
                9 => {
                    p[3] = 8;
                    p[4] = 6;
                    words = vec![0; 1024 + 48];
                    for i in 0..256 {
                        words[i * 4] = 1024;
                        words[i * 4 + 2] = 5;
                        words[i * 4 + 3] = -5;
                    }
                    words[1024..].fill(0xff006000u32 as i32);
                }
                10 => {
                    p[3] = 3;
                    p[4] = 18;
                    words = vec![];
                    for _ in 0..6 {
                        words.extend([1, 8, 0]);
                    }
                    words.extend([0, 0, 8, 0, 0, 6, 0, 0, 8, 0, 0, 6, 1, 1, 7, 1, 1, 5]);
                }
                11 => {
                    p[3..7].copy_from_slice(&[2, 1, 1, 2]);
                    p[7] = blur_type;
                }
                _ => unreachable!(),
            }
            let fixture = Arc::new(Fixture { p, words });
            let single = render(64, fixture.clone());
            let tiled = render(2, fixture);
            assert_eq!(
                single, tiled,
                "Nagano mode {mode}, blur type {blur_type}: seams, missing tiles or stale output"
            );
            assert!(
                single
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|p| *p != [255, 0, 255, 255]),
                "kernel did not write any pixels"
            );
        }
    }
}
