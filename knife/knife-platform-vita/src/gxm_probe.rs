use std::{fs::{File, OpenOptions}, io::Write};
use crate::{data_path, gxm::Gxm, image::DecodedImage};

const PNG: &[u8] = include_bytes!("image_fixtures/alpha.png");
const JPEG: &[u8] = include_bytes!("image_fixtures/colors.jpg");

pub fn run_gxm_probe() -> Result<(), String> {
    crate::native::init_data_dir()?;
    std::fs::write(data_path("gxm-native.log")?, []).map_err(|e| e.to_string())?;
    let mut log = File::create(data_path("gxm-probe.log")?).map_err(|e| e.to_string())?;
    let result = checks(&mut log);
    if let Err(ref error) = result { writeln!(log, "{error}").map_err(|e| e.to_string())?; }
    writeln!(log, "RESULT {}", if result.is_ok() { "PASS" } else { "FAIL" }).map_err(|e| e.to_string())?;
    log.flush().map_err(|e| e.to_string())?;
    result
}

fn checks(log: &mut File) -> Result<(), String> {
    macro_rules! record {
        ($($arg:tt)*) => {
            writeln!(log, $($arg)*).map_err(|e| e.to_string())?;
            log.flush().map_err(|e| e.to_string())?;
            // The emulator buffers its host FILE stream until sceIoClose.
            // Close each DEV observation so the runner can see a stalled stage.
            *log = OpenOptions::new().append(true).open(data_path("gxm-probe.log")?)
                .map_err(|e| e.to_string())?;
        };
    }
    record!("knife Vita DEV direct GXM quad probe v2; pointer width {}", usize::BITS);
    let mut gxm = Gxm::new()?;
    let baseline = gxm.stats()?;
    record!("GXM initialized; {baseline:?}");
    if baseline.program_checks != 2 || baseline.display_width != 960 || baseline.display_height != 544
        || baseline.content_width != 960 || baseline.content_height != 540 || baseline.content_y != 2 {
        return Err("GXM program checks or output dimensions differ".into());
    }
    let colors = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];
    let alpha = [255, 0, 0, 0, 0, 255, 0, 128, 0, 0, 255, 255, 255, 255, 255, 64];
    let texture = gxm.create_texture(&colors, 2, 2, 8)?;
    let alpha_texture = gxm.create_texture(&alpha, 2, 2, 8)?;
    if gxm.create_texture(&colors[..8], 2, 2, 8).is_ok() {
        return Err("short texture buffer accepted".into());
    }
    record!("PASS bounded RGBA upload; distinct opaque and straight-alpha textures");
    // Decode with the native PNG/JPEG boundary, upload the borrowed RGBA, then
    // release its C-owned decode allocation before any frame uses the texture.
    let (png_texture, png_reference) = {
        let image = DecodedImage::decode(PNG).map_err(|e| e.to_string())?;
        (gxm.create_texture(image.pixels(), image.width(), image.height(), image.stride())?,
            image.pixels().to_vec())
    };
    let (jpeg_texture, jpeg_reference) = {
        let image = DecodedImage::decode(JPEG).map_err(|e| e.to_string())?;
        (gxm.create_texture(image.pixels(), image.width(), image.height(), image.stride())?,
            image.pixels().to_vec())
    };
    record!("PASS native PNG/JPEG decode and GXM upload; decoded owners released before drawing");
    let uv = [0., 0., 1., 1.];
    for _ in 0..12 {
        gxm.begin([32. / 255., 64. / 255., 96. / 255., 1.])?;
        gxm.draw(None, [40., 40., 200., 160.], uv, [1., 0., 0., 1.], false)?;
        gxm.draw(Some(&texture), [300., 40., 128., 128.], uv, [1.; 4], false)?;
        gxm.draw(None, [80., 80., 64., 64.], uv, [0., 1., 0., 0.5], true)?;
        gxm.draw(Some(&alpha_texture), [500., 40., 128., 128.], uv, [1.; 4], true)?;
        gxm.draw(Some(&png_texture), [300., 240., 128., 128.], uv, [1.; 4], true)?;
        gxm.draw(Some(&jpeg_texture), [500., 240., 128., 64.], uv, [1.; 4], false)?;
        gxm.present()?;
    }
    record!("FRAME READY: 12 native frames submitted; capturing displayed output");
    // Let the DEV runner capture the scanout independently of CPU readback.
    std::thread::sleep(std::time::Duration::from_secs(6));
    record!("READBACK BEGIN");
    let pixels = gxm.readback()?;
    record!("READBACK RETURNED: {} bytes", pixels.len());
    File::create(data_path("gxm-probe.rgba")?).and_then(|mut f| f.write_all(&pixels)).map_err(|e| e.to_string())?;
    record!("READBACK FILE WRITTEN");
    // Independent CPU reference: rectangle coverage, nearest texels, straight alpha.
    let mut mismatches = 0_u32;
    let mut max_delta = 0_u8;
    let mut first = None;
    for y in 0..544 {
        for x in 0..960 {
            let mut expected = [0, 0, 0, 255];
            if (2..542).contains(&y) {
                let cy = y - 2;
                expected = [32, 64, 96, 255];
                if (40..240).contains(&x) && (40..200).contains(&cy) { expected = [255, 0, 0, 255]; }
                if (300..428).contains(&x) && (40..168).contains(&cy) {
                    let i = (((cy - 40) / 64) * 2 + (x - 300) / 64) * 4;
                    expected.copy_from_slice(&colors[i..i + 4]);
                }
                if (80..144).contains(&x) && (80..144).contains(&cy) { expected = [128, 128, 0, 255]; }
                if (500..628).contains(&x) && (40..168).contains(&cy) {
                    let i = (((cy - 40) / 64) * 2 + (x - 500) / 64) * 4;
                    let a = alpha[i + 3] as u32;
                    for c in 0..3 { expected[c] = ((alpha[i + c] as u32 * a + expected[c] as u32 * (255 - a) + 127) / 255) as u8; }
                }
                if (300..428).contains(&x) && (240..368).contains(&cy) {
                    let i = (((cy - 240) / 64) * 2 + (x - 300) / 64) * 4;
                    let a = png_reference[i + 3] as u32;
                    for c in 0..3 { expected[c] = ((png_reference[i + c] as u32 * a + expected[c] as u32 * (255 - a) + 127) / 255) as u8; }
                }
                if (500..628).contains(&x) && (240..304).contains(&cy) {
                    let i = (((cy - 240) / 8) * 16 + (x - 500) / 8) * 4;
                    expected.copy_from_slice(&jpeg_reference[i..i + 4]);
                }
            }
            let actual = &pixels[(y * 960 + x) * 4..(y * 960 + x + 1) * 4];
            let delta = actual.iter().zip(expected).map(|(a, e)| a.abs_diff(e)).max().unwrap();
            max_delta = max_delta.max(delta);
            if delta > 2 {
                mismatches += 1;
                first.get_or_insert((x, y, actual.to_vec(), expected));
            }
        }
    }
    let stats = gxm.stats()?;
    record!("Completed readback: {} bytes; {stats:?}", pixels.len());
    let readback_error = if mismatches != 0 {
        record!("FAIL pixel reference: {mismatches} mismatches >2/255; max delta {max_delta}; first {first:?}; cleanup follows");
        Some(format!("pixel reference: {mismatches} mismatches >2/255; max delta {max_delta}; first {first:?}"))
    } else {
        record!("PASS all 522240 pixels: output, PNG/JPEG texture orientation, alpha, black borders; max delta {max_delta}/255");
        None
    };
    // A failed readback still exercises independent release/reinitialization.
    // It remains an overall failure after these checks, never a partial PASS.
    if stats.frames != 12 || stats.draws != 84 { return Err("frame/draw submission counts differ".into()); }
    gxm.destroy_texture(&texture)?;
    gxm.destroy_texture(&alpha_texture)?;
    gxm.destroy_texture(&png_texture)?;
    gxm.destroy_texture(&jpeg_texture)?;
    if gxm.draw(Some(&texture), [0., 0., 1., 1.], uv, [1.; 4], false).is_ok() {
        return Err("retired texture handle accepted".into());
    }
    if gxm.stats()?.cdram_mapped_bytes != baseline.cdram_mapped_bytes { return Err("texture CDRAM not reclaimed".into()); }
    record!("PASS texture retirement: GPU completion, handle rejection and CDRAM accounting");
    record!("GXM CLOSE BEGIN");
    gxm.close()?;
    record!("GXM CLOSE RETURNED; second initialization begins");
    // A second lifetime exercises teardown and reinitialization, not just process exit cleanup.
    let mut second = Gxm::new()?;
    record!("GXM SECOND INITIALIZATION RETURNED");
    second.begin([0., 0., 0., 1.])?;
    second.present()?;
    second.close()?;
    record!("PASS GPU/display drain, GXM teardown and a second initialization/frame/teardown");
    if let Some(error) = readback_error { Err(error) } else { Ok(()) }
}
