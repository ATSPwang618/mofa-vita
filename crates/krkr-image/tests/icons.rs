use image::ImageEncoder;
use krkr_assets::{Limits, Vfs};
use krkr_image::icon;
use krkr_protocol::budget::Budget;
use std::{fs, sync::atomic::AtomicBool};

// Uses the same independent wire fixtures as the complete windowEx scenario.
#[test]
fn ico_png_and_first_pe_group_are_bounded_without_loading_the_executable() {
    let ico = include_bytes!("../../krkr-plugins/tests/fixtures/icons/red.ico");
    let pe = include_bytes!("../../krkr-plugins/tests/fixtures/icons/blue-pe.bin");
    let budget = Budget::new(8 * 1024 * 1024);
    let cancelled = AtomicBool::new(false);
    for (bytes, pixel) in [
        (ico.as_slice(), [255, 0, 0, 255]),
        (pe.as_slice(), [0, 0, 255, 255]),
    ] {
        let image = icon::decode(bytes, &budget, &cancelled).unwrap();
        assert_eq!((image.width, image.height), (32, 32));
        assert_eq!(image.rgba.as_slice()[3], 0);
        assert_eq!(image.rgba.as_slice()[4..8], pixel);
        assert_eq!(budget.used(), 32 * 32 * 4);
        drop(image);
        assert_eq!(budget.used(), 0);
    }
    let mut pe64 = pe.to_vec();
    pe64.copy_within(376..416, 392); // Section header follows the larger PE32+ header.
    pe64.copy_within(248..376, 264); // Data directories move forward 16 bytes.
    pe64[148..150].copy_from_slice(&240u16.to_le_bytes());
    pe64[152..154].copy_from_slice(&0x20bu16.to_le_bytes());
    pe64[260..264].copy_from_slice(&16u32.to_le_bytes());
    assert_eq!(
        icon::decode(&pe64, &budget, &cancelled)
            .unwrap()
            .rgba
            .as_slice()[4..8],
        [0, 0, 255, 255]
    );

    let pixels = [0, 0, 255, 255].repeat(32 * 32);
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&pixels, 32, 32, image::ExtendedColorType::Rgba8)
        .unwrap();
    let mut png_ico = vec![0, 0, 1, 0, 1, 0, 32, 32, 0, 0, 1, 0, 32, 0];
    png_ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
    png_ico.extend_from_slice(&22u32.to_le_bytes());
    png_ico.extend_from_slice(&png);
    assert_eq!(
        icon::decode(&png_ico, &budget, &cancelled)
            .unwrap()
            .rgba
            .as_slice(),
        pixels
    );

    // A game EXE may append hundreds of MiB of XP3. Only its selected icon
    // resource is read, even with a decode pool far smaller than the file.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("game.exe");
    fs::write(&path, pe).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(512 * 1024 * 1024)
        .unwrap();
    let plan = Vfs::new(directory.path(), Limits::default())
        .unwrap()
        .plan(&"game.exe".encode_utf16().collect::<Vec<_>>())
        .unwrap();
    let image = icon::read(plan, budget.clone(), &cancelled).unwrap();
    assert_eq!(image.rgba.as_slice()[4..8], [0, 0, 255, 255]);
    drop(image);

    for (source, offset, patch) in [
        (ico.as_slice(), 4, &[255, 255][..]),
        (ico.as_slice(), 34, &[255, 255, 255, 255][..]),
        (pe.as_slice(), 60, &[255, 255, 255, 255][..]),
        (pe.as_slice(), 152, &[0, 0][..]),
        (pe.as_slice(), 1024 + 28, &[0, 0, 0, 128][..]),
        (png_ico.as_slice(), 22 + 16, &[127, 255, 255, 255][..]),
    ] {
        let mut damaged = source.to_vec();
        damaged[offset..offset + patch.len()].copy_from_slice(patch);
        assert!(icon::decode(&damaged, &budget, &cancelled).is_err());
        assert_eq!(budget.used(), 0);
    }
    assert!(icon::decode(ico, &Budget::new(1024), &cancelled).is_err());
    assert!(icon::decode(pe, &budget, &AtomicBool::new(true)).is_err());
    assert_eq!(budget.used(), 0);
}
