//! Finite direct-GXM sampling of digest-locked, synthetic offline GXT files.
//! The host captures each active stage and compares it to independent CPU decodes.
use std::{fs::{self, File, OpenOptions}, io::Write, time::{Duration, Instant}};
use crate::{data_path, gxm::{Gxm, TextureFormat, TextureId}, gxt::GxtTexture, image::DecodedImage};

const STAGES: [(&str, &str, bool); 7] = [
    ("BACKGROUND_RGBA", "background.png", false),
    ("BACKGROUND_PVRTC4", "background-pvrtc4.gxt", false),
    ("BACKGROUND_DXT1", "background-dxt1.gxt", false),
    ("BACKGROUND_DXT5", "background-dxt5.gxt", false),
    ("PORTRAIT_RGBA", "portrait.png", true),
    ("PORTRAIT_PVRTC4", "portrait-pvrtc4.gxt", true),
    ("PORTRAIT_DXT5", "portrait-dxt5.gxt", true),
];

fn record(log: &mut File, message: &str) -> Result<(), String> {
    writeln!(log, "{message}").and_then(|()| log.flush()).map_err(|e| e.to_string())?;
    // Close/reopen to publish the current stage through Vita3K's FILE layer.
    *log = OpenOptions::new().append(true).open(data_path("compressed-probe.log")?)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn read_fixture(name: &str) -> Result<Vec<u8>, String> {
    // Only callers' fixed fixture names are accepted; no external image paths.
    let filename = data_path(&format!("compressed-fixtures/{name}"))?;
    let metadata = fs::metadata(&filename).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 16 * 1024 * 1024 {
        return Err(format!("invalid bounded fixture {name}"));
    }
    let data = fs::read(filename).map_err(|e| e.to_string())?;
    if data.len() as u64 != metadata.len() { return Err(format!("fixture changed: {name}")); }
    Ok(data)
}

fn frame(g: &mut Gxm, texture: &TextureId, checker: &TextureId, portrait: bool) -> Result<(), String> {
    g.begin([0., 0., 0., 1.])?;
    if portrait {
        g.draw(Some(checker), [0., 0., 960., 540.], [0., 0., 1., 1.], [1.; 4], false)?;
    }
    let rect = if portrait { [320., 30., 320., 480.] } else { [0., 0., 960., 540.] };
    g.draw(Some(texture), rect, [0., 0., 1., 1.], [1.; 4], portrait)?;
    g.present()
}

fn checks(log: &mut File) -> Result<(), String> {
    record(log, "TITLE_ID KNIF00012")?;
    record(log, "SYNTHETIC ONLY; seven stages; point sampling; sRGB=false; straight alpha")?;
    let mut g = Gxm::new()?;
    let baseline = g.stats()?;
    let checker: Vec<u8> = (0..960 * 540).flat_map(|i| {
        if ((i % 960) / 16 + (i / 960) / 16) % 2 == 0 {
            [48, 64, 80, 255]
        } else { [208, 192, 176, 255] }
    }).collect();
    let checker = g.create_texture_ex(Some(&checker), 960, 540, 3840,
        TextureFormat::Rgba8, false, false, false)?;
    let stage_baseline = g.stats()?.cdram_mapped_bytes;
    record(log, &format!("GXM READY baseline_cdram={} checker_cdram={} parameter_reserve={}",
        baseline.cdram_mapped_bytes, stage_baseline, baseline.parameter_reserved_bytes))?;

    for (stage, filename, portrait) in STAGES {
        record(log, &format!("STAGE {stage} BEGIN source={filename}"))?;
        let io_started = Instant::now();
        let encoded = read_fixture(filename)?;
        let read_us = io_started.elapsed().as_micros();
        let dimensions = if portrait { (320, 480) } else { (960, 540) };
        let (texture, decode_us, upload_us) = if filename.ends_with(".png") {
            let started = Instant::now();
            let image = DecodedImage::decode(&encoded).map_err(|e| e.to_string())?;
            let decode_us = started.elapsed().as_micros();
            let original = read_fixture(if portrait { "portrait.rgba" } else { "background.rgba" })?;
            if (image.width(), image.height()) != dimensions || image.pixels() != original {
                return Err(format!("PNG/palette/straight-alpha reference mismatch: {stage}"));
            }
            let started = Instant::now();
            let texture = g.create_texture_ex(Some(image.pixels()), image.width(), image.height(),
                image.stride(), TextureFormat::Rgba8, false, false, false)?;
            (texture, decode_us, started.elapsed().as_micros())
        } else {
            let view = GxtTexture::parse(&encoded).map_err(|e| e.to_string())?;
            if (u32::from(view.width), u32::from(view.height)) != dimensions {
                return Err(format!("GXT logical dimensions mismatch: {stage}"));
            }
            record(log, &format!("STAGE {stage} GXT logical={}x{} backing={}x{} format={:?} payload={} runtime_decode=NONE",
                view.width, view.height, view.backing_width, view.backing_height, view.format, view.data.len()))?;
            let started = Instant::now();
            let texture = g.create_gxt_texture(&encoded, false, false)?;
            let upload_us = started.elapsed().as_micros();
            if g.read_texture(&texture).is_ok()
                || g.update_texture(&texture, [0,0], [1,1], &[0;4], 4).is_ok() {
                return Err("immutable compressed texture accepted a CPU pixel operation".into());
            }
            (texture, 0, upload_us)
        };
        // No decode allocation or encoded file is retained by the native texture.
        let encoded_bytes = encoded.len();
        drop(encoded);
        let allocated = g.stats()?.cdram_mapped_bytes - stage_baseline;
        record(log, &format!("STAGE {stage} LOAD encoded_bytes={encoded_bytes} read_us={read_us} decode_us={decode_us} upload_us={upload_us} mapped_bytes={allocated}"))?;
        frame(&mut g, &texture, &checker, portrait)?;
        record(log, &format!("STAGE {stage} FRAME READY fence=COMPLETE"))?;
        let started = Instant::now();
        for _ in 0..60 { frame(&mut g, &texture, &checker, portrait)?; }
        record(log, &format!("STAGE {stage} FRAMES count=60 elapsed_us={} authority=CURRENT_RUNTIME", started.elapsed().as_micros()))?;
        // Keep this exact stage alive long enough for a bounded external capture.
        std::thread::sleep(Duration::from_millis(1600));
        record(log, &format!("STAGE {stage} RETIRE BEGIN"))?;
        g.destroy_texture(&texture)?;
        if g.stats()?.cdram_mapped_bytes != stage_baseline {
            return Err(format!("texture CDRAM did not return to baseline: {stage}"));
        }
        record(log, &format!("STAGE {stage} PASS frames=61 texture_retired=true"))?;
    }
    g.destroy_texture(&checker)?;
    if g.stats()?.cdram_mapped_bytes != baseline.cdram_mapped_bytes {
        return Err("checker retirement CDRAM mismatch".into());
    }
    record(log, "GXM CLOSE BEGIN")?;
    g.close()?;
    record(log, "GXM CLOSE PASS")?;
    Ok(())
}

pub fn run_compressed_probe() -> Result<(), String> {
    crate::native::init_data_dir()?;
    fs::write(data_path("gxm-native.log")?, []).map_err(|e| e.to_string())?;
    let mut log = File::create(data_path("compressed-probe.log")?).map_err(|e| e.to_string())?;
    let result = checks(&mut log);
    if let Err(error) = &result { record(&mut log, &format!("ERROR {error}"))?; }
    record(&mut log, if result.is_ok() { "RESULT PASS" } else { "RESULT FAIL" })?;
    result
}
