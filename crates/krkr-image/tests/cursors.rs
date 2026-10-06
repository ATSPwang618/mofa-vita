use krkr_image::cursor::decode;
use krkr_protocol::budget::Budget;
use std::sync::atomic::AtomicBool;

#[test]
fn dib_cursor_masks_hotspots_and_png_entries_use_the_ecosystem_decoder() {
    let budget = Budget::new(128 * 1024);
    // Two 24-bit BGR pixels followed by the DWORD-aligned AND mask.
    let mut dib = Vec::new();
    for n in [40u32, 2, 2] {
        dib.extend(n.to_le_bytes());
    }
    dib.extend(1u16.to_le_bytes());
    dib.extend(24u16.to_le_bytes());
    dib.extend([0; 24]);
    dib.extend([3, 2, 1, 6, 5, 4, 0, 0]);
    dib.extend([0x40, 0, 0, 0]);
    let mut cur = vec![0, 0, 2, 0, 1, 0, 2, 1, 0, 0, 1, 0, 0, 0];
    cur.extend((dib.len() as u32).to_le_bytes());
    cur.extend(22u32.to_le_bytes());
    cur.extend(dib);
    let image = decode(&cur, &budget, &AtomicBool::new(false)).unwrap();
    assert_eq!(image.hotspot, (1, 0));
    assert_eq!(image.rgba.as_slice(), [1, 2, 3, 255, 4, 5, 6, 0]);
    drop(image);
    assert_eq!(budget.used(), 0);
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 2, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[7, 8, 9, 255, 10, 11, 12, 64])
            .unwrap();
    }
    cur.truncate(22);
    cur[14..18].copy_from_slice(&(png.len() as u32).to_le_bytes());
    cur.extend(png);
    let image = decode(&cur, &budget, &AtomicBool::new(false)).unwrap();
    assert_eq!(image.rgba.as_slice(), [7, 8, 9, 255, 10, 11, 12, 64]);
    drop(image);
    cur[10] = 2;
    assert!(decode(&cur, &budget, &AtomicBool::new(false)).is_err());
    cur[10] = 1;
    assert!(decode(&cur, &budget, &AtomicBool::new(true)).is_err());
    assert!(decode(&cur[..21], &budget, &AtomicBool::new(false)).is_err());
    assert!(decode(&cur, &Budget::new(8), &AtomicBool::new(false)).is_err());
    assert_eq!(budget.used(), 0);
}
