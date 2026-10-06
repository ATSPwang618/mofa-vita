use krkr_assets::{Vfs, name::units};
use krkr_protocol::{
    budget::Budget,
    graphics::Size,
    texture::{Format, reorder_bc},
};
use std::sync::atomic::AtomicBool;

fn load(
    data: &[u8],
    budget: &Budget,
) -> krkr_image::Result<(krkr_protocol::texture::Compressed, krkr_image::Tags)> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.kbct"), data).unwrap();
    let mut vfs = Vfs::new(dir.path(), Default::default()).unwrap();
    krkr_image::resolve::request(
        &mut vfs,
        &units("test.kbct"),
        0x02ffffff,
        None,
        budget.clone(),
    )?
    .probe(&AtomicBool::new(false))?
    .into_compressed_with_tags()
}

#[test]
fn packed_bc_tiles_preserve_tags_and_exact_native_blocks() {
    let tile = Size {
        width: 128,
        height: 64,
    };
    for format in [Format::Bc1Rgb, Format::Bc3Rgba] {
        let stride = if format == Format::Bc1Rgb { 8 } else { 16 };
        let blocks: Vec<u8> = (0..512)
            .flat_map(|i| (0..stride).map(move |c| (c * 29 + i % 4) as u8))
            .collect();
        let ktx = krkr_image::compressed::ktx_format(tile, format, &blocks).unwrap();
        let packed = krkr_image::packed_bc::wrap_packed_bc(&ktx).unwrap();
        assert_eq!(
            krkr_image::packed_bc::storage_stats(&packed).unwrap(),
            (1, 0, blocks.len())
        );
        let tags = vec![("offset_x".into(), "-17".into())];
        let canvas = Size {
            width: 256,
            height: 64,
        };
        let data =
            krkr_image::packed_bc::assemble(canvas, &[packed.clone(), packed.clone()], &tags)
                .unwrap();
        let budget = Budget::new(256 << 10);
        let (texture, decoded_tags) = load(&data, &budget).unwrap();
        assert_eq!(decoded_tags, tags);
        assert_eq!(texture.size, canvas);
        assert_eq!(texture.tile_size, tile);
        assert_eq!(texture.format, format.vita().unwrap());
        let mut expected = vec![0; blocks.len()];
        reorder_bc(tile, format.vita().unwrap(), &blocks, &mut expected, true).unwrap();
        assert_eq!(texture.data(), expected.repeat(2));
        drop(texture);
        assert_eq!(budget.used(), 0);
        assert!(load(&data, &Budget::new(1024)).is_err());

        // Keep a syntactically valid tile header but truncate its range stream.
        let mut broken = packed;
        let metadata = u32::from_le_bytes(broken[28..32].try_into().unwrap()) as usize;
        let at = 32 + metadata;
        broken.truncate(at + 8 + 4);
        broken[at + 4..at + 8].copy_from_slice(&4u32.to_le_bytes());
        assert!(load(&broken, &budget).is_err());
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn incompressible_small_tiles_keep_native_storage() {
    let size = Size {
        width: 8,
        height: 8,
    };
    let blocks: Vec<_> = (0..32u8).map(|i| i.wrapping_mul(79)).collect();
    let ktx = krkr_image::compressed::ktx_format(size, Format::Bc1Rgb, &blocks).unwrap();
    let packed = krkr_image::packed_bc::wrap_packed_bc(&ktx).unwrap();
    assert_eq!(
        krkr_image::packed_bc::storage_stats(&packed).unwrap(),
        (0, 1, 32)
    );
    assert_eq!(packed, krkr_image::packed_bc::wrap_bc(&ktx).unwrap());
}

#[test]
fn large_packed_tiles_match_tight_budget_serial_decode_and_release_errors() {
    let tile = Size {
        width: 1024,
        height: 1024,
    };
    let blocks = vec![0; Format::Bc1Rgb.byte_len(tile).unwrap()];
    let ktx = krkr_image::compressed::ktx_format(tile, Format::Bc1Rgb, &blocks).unwrap();
    let packed = krkr_image::packed_bc::wrap_packed_bc(&ktx).unwrap();
    let canvas = Size {
        width: 2048,
        height: 2048,
    };
    let parts = vec![packed; 4];
    let tags = vec![("offset_y".into(), "-32".into())];
    let data = krkr_image::packed_bc::assemble(canvas, &parts, &tags).unwrap();
    let wide = Budget::new(8 << 20);
    let tight = Budget::new(data.len() + blocks.len() * 5 + 65536);
    let (parallel, parallel_tags) = load(&data, &wide).unwrap();
    let (serial, serial_tags) = load(&data, &tight).unwrap();
    assert_eq!(parallel.data(), serial.data());
    assert_eq!(parallel.data(), blocks.repeat(4));
    assert_eq!(parallel_tags, tags);
    assert_eq!(serial_tags, tags);
    drop((parallel, serial));
    assert_eq!(wide.used(), 0);
    assert_eq!(tight.used(), 0);

    // Corrupt the last independently decoded stream, leaving the tile table
    // valid. Both worker and caller must finish before releasing their buffers.
    let mut broken = data;
    let metadata = u32::from_le_bytes(broken[28..32].try_into().unwrap()) as usize;
    let mut at = 32 + metadata;
    for _ in 0..3 {
        at += 8 + u32::from_le_bytes(broken[at + 4..at + 8].try_into().unwrap()) as usize;
    }
    broken[at + 8..at + 12].fill(255);
    assert!(load(&broken, &wide).is_err());
    assert_eq!(wide.used(), 0);
}
