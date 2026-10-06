use krkr_assets::{Vfs, name};
use krkr_image::psd::{BmpStream, Decoder, Document, Image, Meta};
use krkr_protocol::budget::Budget;
use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};
#[path = "support/psd_fixture.rs"]
mod fixture;
fn decode(doc: Arc<Document>, image: Image, budget: &Budget) -> krkr_protocol::pixels::Pixels {
    let mut decoder = Decoder::new(doc, image, budget.clone()).unwrap();
    while !decoder.advance().unwrap() {}
    decoder.finish()
}
#[test]
fn psd_indexed_streams_depths_compression_metadata_and_failures() {
    let temp = tempfile::tempdir().unwrap();
    let mut vfs = Vfs::new(temp.path(), Default::default()).unwrap();
    let budget = Budget::new(1024 * 1024);
    for depth in [8, 16, 32] {
        for compression in 0..=3 {
            let data = fixture::layered(depth, compression);
            std::fs::write(temp.path().join("scene.psd"), &data).unwrap();
            if let Some(out) = std::env::var_os("KRKR_PSD_FIXTURES") {
                let out = std::path::PathBuf::from(out);
                std::fs::create_dir_all(&out).unwrap();
                std::fs::write(out.join(format!("scene-{depth}-{compression}.psd")), &data)
                    .unwrap();
            }
            let plan = Arc::new(vfs.plan(&name::units("scene.psd")).unwrap());
            let doc = Arc::new(Document::load(plan, Default::default()).unwrap());
            assert_eq!(
                (doc.size.width, doc.size.height, doc.layers.len()),
                (6, 5, 5)
            );
            assert_eq!(doc.layers[2].parent, Some(3));
            assert_eq!(doc.layers[3].parent, Some(4));
            assert_eq!(doc.layers[2].fill_opacity, 170);
            assert_eq!(doc.guides.get("vertical").list()[0].integer(-1), 32);
            assert_eq!(doc.comps.get("comps").list()[0].get("id").integer(-1), 9);
            assert_eq!(doc.layers[2].comps["9"].get("offset_y").integer(0), -3);
            assert_eq!(
                doc.slices.get("slices").list()[0]
                    .get("associated_layer_id")
                    .integer(-1),
                42
            );
            assert!(matches!(
                doc.slices.get("slices").list()[0].get("url"),
                Meta::Text(_)
            ));
            let (ids, paths) = doc.storage_index();
            assert_eq!(ids[&42], 2);
            assert_eq!(paths[&name::units("root/group/inner/彩_色.bmp")], 2);
            let raw = decode(doc.clone(), Image::Raw(2), &budget);
            assert_eq!(
                raw.main.as_ref().unwrap().as_slice(),
                fixture::RGBA,
                "depth {depth}, compression {compression}"
            );
            let masked = decode(doc.clone(), Image::Layer(2), &budget);
            for (i, pixel) in masked
                .main
                .as_ref()
                .unwrap()
                .as_slice()
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
            {
                let expected =
                    u16::from(fixture::RGBA[i * 4 + 3]) * u16::from(fixture::MASK[i]) / 255;
                assert!((i16::from(pixel[3]) - expected as i16).abs() <= 1);
            }
            let mask = decode(doc.clone(), Image::Mask(2), &budget);
            for (p, m) in mask
                .main
                .as_ref()
                .unwrap()
                .as_slice()
                .as_chunks::<4>()
                .0
                .iter()
                .zip(fixture::MASK)
            {
                assert_eq!(*p, [m, m, m, 255]);
            }
            let dummy = decode(doc.clone(), Image::Mask(3), &budget);
            assert_eq!(dummy.main.as_ref().unwrap().as_slice(), [0, 0, 0, 255]);
            let merged = decode(doc.clone(), Image::Merged, &budget);
            assert!(
                merged
                    .main
                    .as_ref()
                    .unwrap()
                    .as_slice()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| *p == [12, 34, 56, 255])
            );
            let mut bmp = BmpStream::new(raw).unwrap();
            let mut bytes = Vec::new();
            bmp.read_to_end(&mut bytes).unwrap();
            assert_eq!(&bytes[..2], b"BM");
            assert_eq!(&bytes[54..58], &[255, 255, 255, 255]);
            bmp.seek(SeekFrom::Start(54 + 12)).unwrap();
            let mut first = [0; 4];
            bmp.read_exact(&mut first).unwrap();
            assert_eq!(first, [0, 0, 255, 255]);
            assert!(Decoder::new(doc.clone(), Image::Merged, Budget::new(8)).is_err());
            let cancelled_budget = Budget::new(1024 * 1024);
            let mut cancelled =
                Decoder::new(doc, Image::Layer(2), cancelled_budget.clone()).unwrap();
            cancelled.advance().unwrap();
            drop(cancelled);
            assert_eq!(cancelled_budget.used(), 0);
        }
    }
    let mut palette = vec![0; 768];
    palette[1] = 10;
    palette[257] = 20;
    palette[513] = 30;
    for (mode, depth, channels, width, raw, palette, expected) in [
        (
            0,
            1,
            1,
            3,
            vec![0b01000000],
            vec![],
            vec![255, 255, 255, 255, 0, 0, 0, 255, 255, 255, 255, 255],
        ),
        (
            1,
            8,
            1,
            2,
            vec![20, 90],
            vec![],
            vec![20, 20, 20, 255, 90, 90, 90, 255],
        ),
        (2, 8, 1, 1, vec![1], palette, vec![10, 20, 30, 255]),
        (
            4,
            8,
            4,
            1,
            vec![255, 128, 0, 255],
            vec![],
            vec![254, 127, 0, 255],
        ),
    ] {
        let bytes = fixture::merged(mode, depth, channels, (width, 1), &raw, 1, &palette);
        std::fs::write(temp.path().join("mode.psd"), bytes).unwrap();
        let doc = Arc::new(
            Document::load(
                Arc::new(vfs.plan(&name::units("mode.psd")).unwrap()),
                Default::default(),
            )
            .unwrap(),
        );
        let pixels = decode(doc, Image::Merged, &budget);
        assert_eq!(pixels.main.as_ref().unwrap().as_slice(), expected);
    }
    let good = fixture::layered(8, 1);
    for cut in [0, 25, 29, good.len() - 80] {
        std::fs::write(temp.path().join("bad.psd"), &good[..cut]).unwrap();
        if let Ok(doc) = Document::load(
            Arc::new(vfs.plan(&name::units("bad.psd")).unwrap()),
            Default::default(),
        ) {
            let mut decoder = Decoder::new(Arc::new(doc), Image::Merged, budget.clone());
            let failed = match &mut decoder {
                Err(_) => true,
                Ok(d) => loop {
                    match d.advance() {
                        Err(_) => break true,
                        Ok(true) => break false,
                        Ok(false) => {}
                    }
                },
            };
            assert!(failed, "truncated PSD accepted");
        }
    }
    assert_eq!(budget.used(), 0);
}
