//! Offline reduced-density archives. The originals are never opened for writing.

use std::{fs::{self, File}, io::{BufWriter, Write}, path::Path, sync::Once};
use anyhow::{Context, Result, ensure};
use image::{DynamicImage, Rgba, RgbaImage, imageops::{self, FilterType}};
use knife_core::format::{archive::{Archive, SECTOR}, bntx::{self, TexFormat}, bntx_decode, compact::{self, Encoding}};

unsafe extern "C" {
    fn knife_bc7_init();
    fn knife_bc7_encode(rgba: *const u8, width: u32, height: u32, output: *mut u8);
}
static INIT: Once = Once::new();

fn encode_bc7(image: &RgbaImage) -> Vec<u8> {
    INIT.call_once(|| unsafe { knife_bc7_init() });
    let mut out = vec![0u8; image.width().div_ceil(4) as usize * image.height().div_ceil(4) as usize * 16];
    // SAFETY: RgbaImage owns width*height*4 bytes; out has precisely the padded BC7 block size.
    // The encoder's shared tables were initialized once, and all per-block scratch state is local.
    unsafe { knife_bc7_encode(image.as_ptr(), image.width(), image.height(), out.as_mut_ptr()) };
    out
}

fn resize_alpha(image: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    if image.pixels().all(|p| p[3] == 255) {
        return imageops::resize(image, width, height, FilterType::Lanczos3);
    }
    let premultiplied = image::ImageBuffer::from_fn(image.width(), image.height(), |x, y| {
        let p = image.get_pixel(x, y);
        let a = f32::from(p[3]) / 255.0;
        // image's f32 resampler clamps every channel to [0, 1].
        Rgba([f32::from(p[0]) / 255.0 * a, f32::from(p[1]) / 255.0 * a, f32::from(p[2]) / 255.0 * a, a])
    });
    let scaled = imageops::resize(&premultiplied, width, height, FilterType::Lanczos3);
    RgbaImage::from_fn(width, height, |x, y| {
        let p = scaled.get_pixel(x, y);
        let a = p[3].clamp(0.0, 1.0);
        if a <= 1.0 / 510.0 { return Rgba([0, 0, 0, 0]); }
        Rgba([(p[0] / a * 255.0).round().clamp(0.0, 255.0) as u8,
            (p[1] / a * 255.0).round().clamp(0.0, 255.0) as u8,
            (p[2] / a * 255.0).round().clamp(0.0, 255.0) as u8, (a * 255.0).round() as u8])
    })
}

fn resize(image: &RgbaImage, scale: f32, strips: u32) -> Result<RgbaImage> {
    ensure!(strips > 0 && image.height().is_multiple_of(strips), "image height is not divisible by language count");
    let width = ((image.width() as f32 * scale).round() as u32).max(1);
    let rows = image.height() / strips;
    let height = if strips > 1 {
        // Whole BC7 rows per language keep blocks and filtering from mixing adjacent translations.
        (((rows as f32 * scale / 4.0).round() as u32).max(1) * 4).min(rows)
    } else { ((rows as f32 * scale).round() as u32).max(1) };
    let mut out = RgbaImage::new(width, height * strips);
    for strip in 0..strips {
        let src = imageops::crop_imm(image, 0, rows * strip, image.width(), rows).to_image();
        let scaled = resize_alpha(&src, width, height);
        imageops::replace(&mut out, &scaled, 0, i64::from(height * strip));
    }
    Ok(out)
}

struct Converted {
    bytes: Vec<u8>,
    logical: [u32; 2],
    physical: [u32; 2],
    unpacked: usize,
    kind: &'static str,
}

fn convert(a: &Archive, index: usize, scale: f32, strips: u32, thumbnail_start: usize) -> Result<Converted> {
    let raw = a.raw(index)?.unwrap_or_default();
    let keep = || Converted { bytes: raw.to_vec(), logical: [0, 0], physical: [0, 0], unpacked: 0, kind: "original" };
    if raw.is_empty() || index >= thumbnail_start { return Ok(keep()); }
    let data = a.read(index)?;
    let (image, srgb, jpeg) = if bntx::is_bntx(&data) {
        let textures = bntx::parse(&data)?;
        ensure!(textures.len() == 1, "multi-texture archives must retain their atlas coordinates");
        let t = &textures[0];
        if matches!(t.format, TexFormat::Bc4Unorm | TexFormat::R8Unorm) || t.width <= 16 || t.height <= 16 {
            return Ok(keep());
        }
        (RgbaImage::from_raw(t.width, t.height, bntx_decode::to_rgba8(t)?).context("invalid decoded RGBA")?, t.format.is_srgb(), false)
    } else if data.starts_with(&[0xff, 0xd8]) || data.starts_with(b"\x89PNG\r\n\x1a\n") {
        (image::load_from_memory(&data)?.to_rgba8(), true, data.starts_with(&[0xff, 0xd8]))
    } else { return Ok(keep()); };
    let logical = [image.width(), image.height()];
    let scaled = resize(&image, scale, strips)?;
    let physical = [scaled.width(), scaled.height()];
    let (encoding, payload, kind) = if jpeg {
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 92)
            .encode_image(&DynamicImage::ImageRgba8(scaled))?;
        (Encoding::Jpeg, bytes, "jpeg")
    } else {
        (if srgb { Encoding::Bc7Srgb } else { Encoding::Bc7Unorm }, encode_bc7(&scaled), "bc7")
    };
    let encoded = compact::encode(logical, physical, encoding, &payload)?;
    let unpacked = encoded.len();
    let mut compressor = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    compressor.write_all(&encoded)?;
    let compressed = compressor.finish()?;
    let mut bytes = b"NXCX".to_vec();
    bytes.extend_from_slice(&u32::try_from(unpacked)?.to_le_bytes());
    bytes.extend_from_slice(&u32::try_from(compressed.len())?.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 4]);
    bytes.extend_from_slice(&compressed);
    Ok(Converted { bytes, logical, physical, unpacked, kind })
}

pub fn archive(romfs: &Path, name: &str, out: &Path, scale: f32, jobs: usize, strips: u32) -> Result<()> {
    ensure!(scale.is_finite() && scale > 0.0 && scale < 1.0, "scale must be between 0 and 1");
    ensure!((1..=32).contains(&jobs), "jobs must be between 1 and 32");
    ensure!(matches!(name, "jallpac" | "jallpachd" | "allpacml" | "allpachdml"), "compact only supports the four WoH story archives");
    ensure!((1..=8).contains(&strips), "invalid language count");
    fs::create_dir_all(out)?;
    let a = Archive::open(romfs, name)?;
    let thumbnail_start = a.entries().iter().position(|e| e.name.starts_with("THUMSP_TOP")).unwrap_or(a.entries().len());
    let extensions = ["mrg", "hed", "nam", "compact.tsv"];
    for ext in extensions {
        ensure!(!out.join(format!("{name}.{ext}")).exists(), "output {name}.{ext} already exists");
        ensure!(!out.join(format!("{name}.{ext}.partial")).exists(), "unfinished output {name}.{ext}.partial already exists");
    }
    let mut mrg = BufWriter::with_capacity(8 * 1024 * 1024, File::create_new(out.join(format!("{name}.mrg.partial")))?);
    let mut report = BufWriter::new(File::create_new(out.join(format!("{name}.compact.tsv.partial")))?);
    writeln!(report, "index\tname\tkind\tlogical_width\tlogical_height\tphysical_width\tphysical_height\tstored_bytes")?;
    let mut hed = Vec::new();
    let (mut offset, mut changed) = (0u64, 0usize);
    let start = std::time::Instant::now();
    for first in (0..a.entries().len()).step_by(jobs) {
        let last = (first + jobs).min(a.entries().len());
        let batch = std::thread::scope(|scope| {
            let threads: Vec<_> = (first..last).map(|i| {
                let a = &a;
                scope.spawn(move || convert(a, i, scale, strips, thumbnail_start).with_context(|| format!("entry {i}: {}", a.entries()[i].name)))
            }).collect();
            threads.into_iter().map(|t| t.join().map_err(|_| anyhow::anyhow!("texture worker panicked"))?).collect::<Result<Vec<_>>>()
        })?;
        for (i, entry) in (first..last).zip(batch) {
            let sectors = entry.bytes.len().div_ceil(SECTOR as usize);
            let original = &a.entries()[i];
            hed.extend_from_slice(&u32::try_from(offset / SECTOR)?.to_le_bytes());
            hed.extend_from_slice(&u16::try_from(sectors).context("stored image exceeds archive sector field")?.to_le_bytes());
            let unpacked = if entry.unpacked == 0 { original.unpacked_sectors } else { u16::try_from(entry.unpacked.div_ceil(SECTOR as usize))? };
            hed.extend_from_slice(&unpacked.to_le_bytes());
            mrg.write_all(&entry.bytes)?;
            let padding = sectors * SECTOR as usize - entry.bytes.len();
            mrg.write_all(&[0u8; SECTOR as usize][..padding])?;
            offset += sectors as u64 * SECTOR;
            changed += usize::from(entry.kind != "original");
            writeln!(report, "{i}\t{}\t{}\t{}\t{}\t{}\t{}\t{}", original.name, entry.kind,
                entry.logical[0], entry.logical[1], entry.physical[0], entry.physical[1], entry.bytes.len())?;
        }
        if first / 256 != last / 256 || last == a.entries().len() {
            println!("{name}: {last}/{} entries, {changed} resized, {:.1} MiB, {:.1}s", a.entries().len(), offset as f64 / 1048576.0, start.elapsed().as_secs_f64());
        }
    }
    hed.extend_from_slice(&[0xff; 8]);
    mrg.flush()?;
    report.flush()?;
    drop(mrg);
    drop(report);
    fs::write(out.join(format!("{name}.hed.partial")), hed)?;
    fs::copy(romfs.join(format!("{name}.nam")), out.join(format!("{name}.nam.partial")))?;
    for ext in extensions {
        fs::rename(out.join(format!("{name}.{ext}.partial")), out.join(format!("{name}.{ext}")))?;
    }
    Ok(())
}

pub fn extract(data: &[u8], out: &Path) -> Result<()> {
    let image = compact::parse(data)?;
    if let Some(t) = image.texture() {
        image::save_buffer(out, &bntx_decode::to_rgba8(&t)?, t.width, t.height, image::ExtendedColorType::Rgba8)?;
    } else {
        image::load_from_memory(image.payload)?.save(out)?;
    }
    println!("logical {}x{}, physical {}x{}, {:?}", image.logical[0], image.logical[1], image.size[0], image.size[1], image.encoding);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_colour_does_not_bleed_into_resized_sprites() {
        let src = RgbaImage::from_fn(8, 4, |x, _| if x < 4 { Rgba([255, 0, 0, 255]) } else { Rgba([0, 0, 255, 0]) });
        let small = resize_alpha(&src, 2, 1);
        assert!(small.pixels().any(|p| p[3] > 0 && p[3] < 255));
        for p in small.pixels().filter(|p| p[3] > 0) {
            assert!(p[0] >= 250 && p[1] == 0 && p[2] == 0, "transparent blue contaminated {p:?}");
        }
    }

    #[test]
    fn language_strips_are_filtered_separately_and_keep_bc7_boundaries() {
        let colors = [Rgba([255, 0, 0, 255]), Rgba([0, 255, 0, 255]), Rgba([0, 0, 255, 255]), Rgba([255, 255, 0, 255])];
        let src = RgbaImage::from_fn(8, 32, |_, y| colors[(y / 8) as usize]);
        let small = resize(&src, 0.5, 4).unwrap();
        assert_eq!(small.dimensions(), (4, 16));
        for (i, color) in colors.iter().enumerate() {
            for y in i as u32 * 4..i as u32 * 4 + 4 { assert_eq!(small.get_pixel(0, y), color); }
        }
    }

    #[test]
    fn bc7_encoder_pads_odd_edges_without_losing_alpha() {
        let src = RgbaImage::from_pixel(7, 5, Rgba([220, 40, 80, 100]));
        let payload = encode_bc7(&src);
        assert_eq!(payload.len(), 64);
        let encoded = compact::encode([14, 10], [7, 5], Encoding::Bc7Srgb, &payload).unwrap();
        let texture = compact::parse(&encoded).unwrap().texture().unwrap();
        let rgba = bntx_decode::to_rgba8(&texture).unwrap();
        assert_eq!(rgba.len(), 140);
        for p in rgba.chunks_exact(4) {
            for (actual, expected) in p.iter().zip([220u8, 40, 80, 100]) {
                assert!(actual.abs_diff(expected) <= 5, "unexpected BC7 colour {p:?}");
            }
        }
    }

    #[test]
    fn archive_round_trip_keeps_indices_language_layout_and_original_markers() {
        let scratch = std::env::temp_dir().join(format!("knife-compact-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let source = scratch.join("source");
        let out = scratch.join("converted");
        fs::create_dir_all(&source).unwrap();
        let colors = [Rgba([255, 0, 0, 255]), Rgba([0, 255, 0, 255]), Rgba([0, 0, 255, 255]), Rgba([255, 255, 0, 255])];
        let image = RgbaImage::from_fn(32, 64, |_, y| colors[(y / 16) as usize]);
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let names = ["NOFILE.EXISTS", "IMG0001.PNG", "RULE.MZP", "THUMSP_TOP.MZP", "THUMB.PNG"];
        let payloads = [vec![], png.into_inner(), b"mrgd-marker".to_vec(), b"mrgd-thumbnail-marker".to_vec(), vec![31u8; 77]];
        let (mut hed, mut nam, mut mrg) = (Vec::new(), Vec::new(), Vec::new());
        for (name, payload) in names.iter().zip(&payloads) {
            hed.extend_from_slice(&(mrg.len() as u32 / 2048).to_le_bytes());
            let sectors = payload.len().div_ceil(2048) as u16;
            hed.extend_from_slice(&sectors.to_le_bytes());
            hed.extend_from_slice(&sectors.to_le_bytes());
            let mut record = [0; 32];
            record[..name.len()].copy_from_slice(name.as_bytes());
            nam.extend_from_slice(&record);
            mrg.extend_from_slice(payload);
            mrg.resize(mrg.len().div_ceil(2048) * 2048, 0);
        }
        hed.extend_from_slice(&[255; 8]);
        for (ext, bytes) in [("hed", &hed), ("nam", &nam), ("mrg", &mrg)] {
            fs::write(source.join(format!("allpacml.{ext}")), bytes).unwrap();
        }
        archive(&source, "allpacml", &out, 0.5, 2, 4).unwrap();
        let converted = Archive::open(&out, "allpacml").unwrap();
        assert_eq!(converted.entries().iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), names);
        assert!(converted.raw(0).unwrap().is_none());
        let bytes = converted.read(1).unwrap();
        let compact = compact::parse(&bytes).unwrap();
        assert_eq!(compact.logical, [32, 64]);
        assert_eq!(compact.size, [16, 32]);
        let rgba = bntx_decode::to_rgba8(&compact.texture().unwrap()).unwrap();
        for (i, color) in colors.iter().enumerate() {
            let pixel = &rgba[i * 8 * 16 * 4..][..4];
            for (actual, expected) in pixel.iter().zip(color.0) { assert!(actual.abs_diff(expected) <= 5); }
        }
        for index in [2, 3, 4] {
            assert!(converted.read(index).unwrap().starts_with(&payloads[index]));
        }
        assert_eq!(fs::read(source.join("allpacml.mrg")).unwrap(), mrg);
        assert_eq!(fs::read(out.join("allpacml.nam")).unwrap(), nam);
        assert!(archive(&source, "allpacml", &out, 0.5, 2, 4).is_err());
        drop(converted);
        fs::remove_dir_all(&scratch).unwrap();
    }
}
