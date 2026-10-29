//! Independent synthetic production-native Sprite / Alter / Present validation.
//!
//! The same 160x112 NPOT atlas has three storage paths. Its KIMG logical extent
//! is 320x224; texture sampling uses the physical 160x112 extent, not padded
//! 256x128 BC storage. This module deliberately does not exercise renderer
//! facade caching, archive decoding, or a real game's image loading path.
use knife_platform_vita::{
    DATA_ROOT, data_path,
    gxm::{EffectBlend, EffectShader, Gxm, TextureFormat, TextureId},
    gxt::{GxtFormat, GxtTexture},
};
use knife_render_api::Vertex;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    time::{Duration, Instant},
};

const WIDTH: u32 = 160;
const HEIGHT: u32 = 112;
const STAGES: [&str; 3] = ["UI_RUNTIME_ALTER", "UI_OFFLINE_RGBA", "UI_OFFLINE_DXT5"];
const LOG: &str = "ui-color-probe.log";
const CHECKER_COLORS: [[u8; 4]; 2] = [[97, 118, 134, 255], [221, 203, 184, 255]];

#[derive(Clone, Copy)]
struct Panel {
    rect: [f32; 4],
    uv: [f32; 4],
    color: [f32; 4],
}

// Keep this finite contract in sync with check-vita-ui-color-pixels.py.
// UV coordinates outside 0..1 deliberately exercise GXM's linear clamp mode.
const PANELS: [Panel; 4] = [
    Panel {
        rect: [24., 24., 432., 216.],
        uv: [-0.05, -0.05, 1.05, 0.55],
        color: [1.; 4],
    },
    Panel {
        rect: [504., 24., 432., 216.],
        uv: [0., 0., 1., 1.],
        color: [0.64, 0.90, 0.45, 0.72],
    },
    Panel {
        rect: [72., 284., 352., 224.],
        uv: [0.10, 0.45, 0.90, 1.05],
        color: [0.85, 0.55, 1., 0.85],
    },
    Panel {
        rect: [520.25, 288.25, 352., 216.],
        uv: [1.10, 1.05, -0.10, -0.05],
        color: [0.50, 1., 0.65, 0.65],
    },
];

fn record(log: &mut File, message: &str) -> Result<(), String> {
    writeln!(log, "{message}")
        .and_then(|()| log.flush())
        .map_err(|e| e.to_string())?;
    // Closing the guest FILE publishes the latest stage to an external runner.
    *log = OpenOptions::new()
        .append(true)
        .open(data_path(LOG)?)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn read_fixture(name: &str, expected_length: Option<usize>) -> Result<Vec<u8>, String> {
    let path = data_path(&format!("ui-color-fixtures/{name}"))?;
    let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 16 * 1024 * 1024 {
        return Err(format!("invalid bounded synthetic fixture: {name}"));
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() as u64 != metadata.len() || expected_length.is_some_and(|n| n != bytes.len()) {
        return Err(format!("changed or wrong-length fixture: {name}"));
    }
    Ok(bytes)
}

fn vertices(panel: Panel) -> [Vertex; 6] {
    [[0., 0.], [1., 0.], [1., 1.], [0., 0.], [1., 1.], [0., 1.]].map(|p| Vertex {
        pos: [
            (panel.rect[0] + p[0] * panel.rect[2]) * 2. / 960. - 1.,
            1. - (panel.rect[1] + p[1] * panel.rect[3]) * 2. / 540.,
        ],
        uv: [
            panel.uv[0] + p[0] * (panel.uv[2] - panel.uv[0]),
            panel.uv[1] + p[1] * (panel.uv[3] - panel.uv[1]),
        ],
        color: panel.color,
        haze: [[0.; 4]; 2],
        soft: [[0.; 4]; 3],
    })
}

fn fullscreen() -> [Vertex; 6] {
    vertices(Panel {
        rect: [0., 0., 960., 540.],
        uv: [0., 0., 1., 1.],
        color: [1.; 4],
    })
}

fn frame(
    g: &mut Gxm,
    canvas: &TextureId,
    checker: &TextureId,
    atlas: &TextureId,
) -> Result<(), String> {
    g.begin_target(canvas, Some([0., 0., 0., 1.]))?;
    // Opaque checker is independently sRGB-encoded and point sampled. Alpha
    // panels use Sprite's premultiplied output and native Alpha blending.
    g.draw_effect(
        &fullscreen(),
        EffectShader::Sprite,
        EffectBlend::Opaque,
        [Some(checker), None, None, None],
        [0.; 8],
    )?;
    for panel in PANELS {
        g.draw_effect(
            &vertices(panel),
            EffectShader::Sprite,
            EffectBlend::Alpha,
            [Some(atlas), None, None, None],
            [0.; 8],
        )?;
    }
    g.end_target()?;
    g.present_canvas(canvas)
}

fn runtime_alter(g: &mut Gxm, original: &[u8]) -> Result<TextureId, String> {
    let source = g.create_texture_ex(
        Some(original),
        WIDTH,
        HEIGHT,
        WIDTH * 4,
        TextureFormat::Rgba8,
        true,
        true,
        false,
    )?;
    let altered = g.create_texture_ex(
        None,
        WIDTH,
        HEIGHT,
        WIDTH * 4,
        TextureFormat::Rgba8,
        true,
        true,
        true,
    )?;
    g.begin_target(&altered, Some([0.; 4]))?;
    // Identical native production Alter GXP and RGBA8 sRGB destination; the
    // fullscreen coordinates remain -1..1 at the small target's viewport.
    g.draw_effect(
        &fullscreen(),
        EffectShader::Alter,
        EffectBlend::Opaque,
        [Some(&source), None, None, None],
        [0.; 8],
    )?;
    g.end_target()?;
    g.destroy_texture(&source)?;
    Ok(altered)
}

fn checks(log: &mut File) -> Result<(), String> {
    record(log, "TITLE_ID KNIF00013")?;
    record(
        log,
        "SYNTHETIC UI COLOR; logical=320x224 physical=160x112 BC_backing=256x128; source_srgb=true canvas_srgb=true linear_clamp=true straight_source_alpha=true",
    )?;
    record(
        log,
        "PATH production-native Sprite/Alpha -> RGBA8 sRGB canvas -> Present; no facade/cache/archive equivalence claim",
    )?;
    let mut g = Gxm::new()?;
    let baseline = g.stats()?.cdram_mapped_bytes;
    let checker_raw = read_fixture("ui-checker.rgba", Some(960 * 540 * 4))?;
    for (i, pixel) in checker_raw.chunks_exact(4).enumerate() {
        if pixel != CHECKER_COLORS[((i % 960 / 16) + (i / 960 / 16)) & 1] {
            return Err("checker pixels differ from finite color contract".into());
        }
    }
    let checker = g.create_texture_ex(
        Some(&checker_raw),
        960,
        540,
        960 * 4,
        TextureFormat::Rgba8,
        true,
        false,
        false,
    )?;
    drop(checker_raw);
    let canvas = g.create_texture_ex(
        None,
        960,
        540,
        960 * 4,
        TextureFormat::Rgba8,
        true,
        true,
        true,
    )?;
    let stage_baseline = g.stats()?.cdram_mapped_bytes;
    record(
        log,
        &format!("GXM READY baseline_cdram={baseline} common_cdram={stage_baseline}"),
    )?;
    for stage in STAGES {
        record(log, &format!("STAGE {stage} BEGIN"))?;
        let started = Instant::now();
        let atlas = match stage {
            "UI_RUNTIME_ALTER" => runtime_alter(
                &mut g,
                &read_fixture("ui-source.rgba", Some((WIDTH * HEIGHT * 4) as usize))?,
            )?,
            "UI_OFFLINE_RGBA" => g.create_texture_ex(
                Some(&read_fixture(
                    "ui-offline.alter.rgba",
                    Some((WIDTH * HEIGHT * 4) as usize),
                )?),
                WIDTH,
                HEIGHT,
                WIDTH * 4,
                TextureFormat::Rgba8,
                true,
                true,
                false,
            )?,
            "UI_OFFLINE_DXT5" => {
                let encoded = read_fixture("ui-offline.gxt", None)?;
                let view = GxtTexture::parse(&encoded).map_err(|e| e.to_string())?;
                if (
                    view.width,
                    view.height,
                    view.backing_width,
                    view.backing_height,
                    view.format,
                    view.data.len(),
                ) != (160, 112, 256, 128, GxtFormat::Dxt5, 32768)
                {
                    return Err("DXT5 metadata differs from finite fixture contract".into());
                }
                g.create_gxt_texture(&encoded, true, true)?
            }
            _ => return Err("unknown synthetic stage".into()),
        };
        record(
            log,
            &format!(
                "STAGE {stage} LOAD elapsed_us={} mapped_bytes={}",
                started.elapsed().as_micros(),
                g.stats()?.cdram_mapped_bytes - stage_baseline
            ),
        )?;
        frame(&mut g, &canvas, &checker, &atlas)?;
        record(log, &format!("STAGE {stage} FRAME READY fence=COMPLETE"))?;
        for _ in 0..60 {
            frame(&mut g, &canvas, &checker, &atlas)?;
        }
        // The runner must bracket screen.capture with this stage's live log.
        std::thread::sleep(Duration::from_millis(2000));
        record(log, &format!("STAGE {stage} RETIRE BEGIN"))?;
        g.destroy_texture(&atlas)?;
        if g.stats()?.cdram_mapped_bytes != stage_baseline {
            return Err(format!("CDRAM did not return to stage baseline: {stage}"));
        }
        record(
            log,
            &format!("STAGE {stage} PASS frames=61 texture_retired=true"),
        )?;
    }
    g.destroy_texture(&canvas)?;
    g.destroy_texture(&checker)?;
    if g.stats()?.cdram_mapped_bytes != baseline {
        return Err("common CDRAM did not return to initial baseline".into());
    }
    record(log, "GXM CLOSE BEGIN")?;
    g.close()?;
    record(log, "GXM CLOSE PASS")?;
    Ok(())
}

pub fn run_ui_color_probe() -> Result<(), String> {
    fs::create_dir_all(DATA_ROOT).map_err(|e| e.to_string())?;
    let mut log = File::create(data_path(LOG)?).map_err(|e| e.to_string())?;
    let result = checks(&mut log);
    if let Err(error) = &result {
        record(&mut log, &format!("ERROR {error}"))?;
    }
    record(
        &mut log,
        if result.is_ok() {
            "RESULT PASS"
        } else {
            "RESULT FAIL"
        },
    )?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vertex_contract_preserves_outside_mirror_uv_and_f32_color() {
        assert_eq!(std::mem::size_of::<Vertex>(), 112);
        let quad = vertices(PANELS[3]);
        assert_eq!(quad[0].uv, [1.10, 1.05]);
        assert!((quad[2].uv[0] + 0.10).abs() < 0.000_001);
        assert!((quad[2].uv[1] + 0.05).abs() < 0.000_001);
        for vertex in quad {
            assert_eq!(vertex.color, [0.50, 1., 0.65, 0.65]);
            assert_eq!(vertex.haze, [[0.; 4]; 2]);
            assert_eq!(vertex.soft, [[0.; 4]; 3]);
        }
    }
    #[test]
    fn fullscreen_is_target_relative_and_fractional_rect_is_retained() {
        let full = fullscreen();
        assert_eq!(full[0].pos, [-1., 1.]);
        assert_eq!(full[2].pos, [1., -1.]);
        let quad = vertices(PANELS[3]);
        assert!((quad[0].pos[0] - (520.25 * 2. / 960. - 1.)).abs() < 0.000_001);
        assert!((quad[0].pos[1] - (1. - 288.25 * 2. / 540.)).abs() < 0.000_001);
    }
}
