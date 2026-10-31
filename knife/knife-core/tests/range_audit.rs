use std::{io::Write, sync::Arc};
use flate2::{Compression, write::ZlibEncoder};
use knife_core::format::archive::Archive;
use knife_perf::Metric;

#[test]
fn concurrent_subranges_report_full_inflate_and_selected_copy_without_changing_bytes() {
    let root = std::env::temp_dir().join(format!("knife-range-audit-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let pixels: Vec<_> = (0..131072).map(|n| (n % 251) as u8).collect();
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&pixels).unwrap();
    let encoded = encoder.finish().unwrap();
    let mut raw = b"NXCX".to_vec();
    for n in [pixels.len() as u32, encoded.len() as u32, 0] { raw.extend_from_slice(&n.to_le_bytes()); }
    raw.extend_from_slice(&encoded);
    let sectors = raw.len().div_ceil(2048) as u16;
    raw.resize(sectors as usize * 2048, 0);
    let mut hed = 0u32.to_le_bytes().to_vec();
    hed.extend_from_slice(&sectors.to_le_bytes()); hed.extend_from_slice(&64u16.to_le_bytes());
    hed.extend_from_slice(&[255; 8]);
    std::fs::write(root.join("test.hed"), hed).unwrap();
    std::fs::write(root.join("test.mrg"), raw).unwrap();
    let archive = Arc::new(Archive::open_file(&root, "test").unwrap());
    knife_perf::set_enabled(true);
    let jobs: Vec<_> = (0..8).map(|i| {
        let archive = Arc::clone(&archive);
        std::thread::spawn(move || archive.read_uncompressed_range(0, i * 1024, 64).unwrap())
    }).collect();
    for (i, job) in jobs.into_iter().enumerate() { assert_eq!(job.join().unwrap(), pixels[i*1024..i*1024+64]); }
    let ranges = knife_perf::get(Metric::NxRange);
    assert_eq!((ranges.calls, ranges.bytes, ranges.aux), (8, 8 * 131072, 8 * 64));
    assert_eq!(knife_perf::get(Metric::RangeSelect).bytes, 8 * 64);
    assert_eq!(knife_core::io_metrics::snapshot().inflate_calls, 8);
    assert_eq!(knife_perf::take_hotspots(8).0[0].sample.calls, 8);
    knife_perf::set_enabled(false);
    drop(archive);
    std::fs::remove_dir_all(root).unwrap();
}
