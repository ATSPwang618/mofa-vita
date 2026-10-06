#[cfg(feature = "sgx-binaries")]
#[path = "src/draw_source.rs"]
mod draw_source;
#[cfg(feature = "sgx-binaries")]
#[path = "src/resample_source.rs"]
mod resample_source;
#[cfg(feature = "sgx-binaries")]
#[allow(dead_code)]
#[path = "src/scene_batch_source.rs"]
mod scene_batch_source;
#[cfg(feature = "sgx-binaries")]
#[path = "build_support/sgx.rs"]
mod sgx;
#[cfg(feature = "sgx-binaries")]
#[path = "src/transition_source.rs"]
mod transition_source;

fn main() {
    #[cfg(feature = "sgx-binaries")]
    build_binaries();
}

#[cfg(feature = "sgx-binaries")]
fn build_binaries() {
    use draw_source::{Key, Kind, Sampling};
    use pvr_compiler::Stage;
    use std::{env, fmt::Write, fs, path::PathBuf};

    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let mut sources = vec![(Stage::Vertex, include_str!("src/quad.vert").to_owned())];
    sources.push((
        Stage::Vertex,
        include_str!("src/glyph_batch.vert").to_owned(),
    ));
    sources.push((
        Stage::Vertex,
        include_str!("src/color_batch.vert").to_owned(),
    ));
    for face in [0, 1, 4] {
        sources.push((
            Stage::Fragment,
            draw_source::fragment_source(&draw_source::color_batch(face)),
        ));
    }
    sources.push((
        Stage::Fragment,
        draw_source::fragment_source(&draw_source::glyph_batch()),
    ));
    sources.push((
        Stage::Fragment,
        draw_source::fragment_source(include_str!("src/glyph_pair.frag")),
    ));
    let mut keys = Vec::new();
    for covered in [false, true] {
        keys.push(Key {
            sampling: Sampling::Display,
            mode: 2,
            covered,
            ..Key::raw()
        });
    }
    for sampling in [
        Sampling::Nearest,
        Sampling::Logical,
        Sampling::Affine,
        Sampling::Linear,
        Sampling::Wrapped,
        Sampling::LogicalAffine,
        Sampling::LogicalLinear,
        Sampling::Display,
        Sampling::SharpenedDisplay,
    ] {
        keys.push(Key {
            sampling,
            ..Key::raw()
        });
        if matches!(
            sampling,
            Sampling::Affine | Sampling::Linear | Sampling::LogicalAffine | Sampling::LogicalLinear
        ) {
            keys.push(Key {
                sampling,
                clear: true,
                ..Key::raw()
            });
            keys.push(Key {
                sampling,
                clear: true,
                covered: true,
                ..Key::raw()
            });
        }
    }
    for sampling in [
        Sampling::Nearest,
        Sampling::Logical,
        Sampling::Linear,
        Sampling::LogicalLinear,
        Sampling::Display,
        Sampling::SharpenedDisplay,
    ] {
        keys.push(Key {
            sampling,
            covered: true,
            ..Key::raw()
        });
    }
    keys.push(Key {
        kind: Kind::Fill,
        sampling: Sampling::Constant,
        ..Key::raw()
    });
    keys.push(Key {
        kind: Kind::Glyph,
        ..Key::raw()
    });
    for face in [0, 1, 4] {
        keys.push(Key {
            kind: Kind::Solid,
            sampling: Sampling::Constant,
            face,
            ..Key::raw()
        });
        // Ordinary layers and common Photoshop effects must not compile on
        // their first animation frame. Mode 3 is used by Mahoyo's first scene.
        // Rare lookup modes keep the runtime LRU.
        for mode in [1, 2, 3, 12, 13, 14, 15, 16, 17, 18, 19, 22] {
            for sampling in [
                Sampling::Nearest,
                Sampling::Logical,
                Sampling::Display,
                Sampling::SharpenedDisplay,
            ] {
                keys.push(Key {
                    kind: Kind::Blend,
                    sampling,
                    mode,
                    face,
                    clear: false,
                    constant_backdrop: false,
                    covered: false,
                });
            }
        }
    }
    // Match the common blend catalog on solid regions too: the first portrait
    // on a cleared layer must not trigger shader compilation on the handheld.
    keys.extend(
        keys.clone()
            .into_iter()
            .filter(|key| matches!(key.kind, Kind::Solid | Kind::Blend))
            .map(|key| Key {
                constant_backdrop: true,
                ..key
            }),
    );
    // Keep opacity uniforms and arithmetic out of ordinary copies and final
    // presentation. Premultiplied display draws own their shader variants.
    keys.extend(
        keys.clone()
            .into_iter()
            .filter(|key| key.kind == Kind::Raw && key.mode == 0)
            .map(|key| Key {
                kind: Kind::PremultipliedDisplay,
                ..key
            }),
    );
    for key in keys {
        sources.push((
            Stage::Fragment,
            draw_source::fragment_source(&draw_source::fragment(key)),
        ));
    }
    for layers in 2..=4 {
        for face in [0, 1, 4] {
            for constant in [false, true] {
                for clipped in [false, true] {
                    for (display, sharpen) in [(false, false), (true, false), (true, true)] {
                        sources.push((
                            Stage::Fragment,
                            draw_source::fragment_source(&scene_batch_source::fragment(
                                scene_batch_source::Key {
                                    layers,
                                    face,
                                    constant,
                                    clipped,
                                    display,
                                    sharpen,
                                },
                            )),
                        ));
                    }
                }
            }
        }
    }
    sources.push((Stage::Vertex, include_str!("src/fills.vert").to_owned()));
    for taps in resample_source::TAPS {
        sources.push((
            Stage::Fragment,
            draw_source::fragment_source(&resample_source::fragment(taps)),
        ));
    }
    for face in [0, 1, 4] {
        for rule in [false, true] {
            for direct in [false, true] {
                sources.push((
                    Stage::Fragment,
                    draw_source::fragment_source(&transition_source::fragment(rule, face, direct)),
                ));
            }
        }
    }
    for mode in (0..=4).chain(16..=27) {
        sources.push((
            Stage::Fragment,
            draw_source::fragment_source(&transition_source::custom(mode)),
        ));
    }
    for fragment in [
        include_str!("src/fills.frag"),
        include_str!("src/video.frag"),
        include_str!("src/box_blur_small.frag"),
    ] {
        sources.push((Stage::Fragment, draw_source::fragment_source(fragment)));
    }
    // Effect setup must not invoke the SGX compiler on its first game frame.
    // Keep these strings identical to Program::new's runtime sources.
    sources.push((
        Stage::Fragment,
        draw_source::fragment_source(&format!(
            "{}\n{}",
            include_str!("src/integer.glsl"),
            include_str!("src/lines.frag")
        )),
    ));
    for stage in 0..2 {
        for fragment in [
            format!(
                "#define BOX_STAGE {stage}\n{}\n{}",
                include_str!("src/integer.glsl"),
                include_str!("src/box_blur.frag")
            ),
            format!(
                "#define PACKED_STAGE {stage}\n{}",
                include_str!("src/box_blur_packed.frag")
            ),
        ] {
            sources.push((Stage::Fragment, draw_source::fragment_source(&fragment)));
        }
    }
    for kind in [0, 3, 4] {
        sources.push((
            Stage::Fragment,
            draw_source::fragment_source(&format!(
                "#define ADJUST_KIND {kind}\n{}\n{}",
                include_str!("src/integer.glsl"),
                include_str!("src/adjust.frag")
            )),
        ));
    }

    if sgx::run_worker(&sources, &output) {
        return;
    }
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_support");
    sgx::compile(&sources, &output);

    let mut catalog = String::from("static BINARIES: &[(u32, &str, &[u8])] = &[\n");
    let mut bytes = 0;
    for (index, (stage, _)) in sources.iter().enumerate() {
        bytes += fs::metadata(output.join(format!("sgx-{index}.bin")))
            .unwrap()
            .len();
        let kind = if *stage == Stage::Vertex {
            0x8B31u32
        } else {
            0x8B30
        };
        writeln!(catalog, "({kind}, include_str!(concat!(env!(\"OUT_DIR\"), \"/sgx-{index}.glsl\")), include_bytes!(concat!(env!(\"OUT_DIR\"), \"/sgx-{index}.bin\"))),").unwrap();
    }
    catalog.push_str("];\n");
    // Catch accidental catalog expansion before it consumes handheld RAM.
    assert!(
        bytes < 2 * 1024 * 1024,
        "SGX catalog exceeds 2 MiB: {bytes}"
    );
    fs::write(output.join("sgx_catalog.rs"), catalog).unwrap();
    println!(
        "cargo:warning=Embedded {} SGX543 shaders, {bytes} binary bytes",
        sources.len()
    );
}
