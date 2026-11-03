//! Host-only verification of the offline budget report with the real bounded
//! Archive and DXT decoder. Expected CRCs come from PC's independent DDS path.
//! Does not submit GPU commands or claim console performance evidence.
use std::{collections::HashMap, path::Path};
use anyhow::{Context, Result, ensure};
use knife_core::format::{archive::{Archive, ArchiveLimits}, compact, gxt_decode};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    ensure!(args.len() == 3, "usage: verify_budgeted_art <staged-assets> <native-verify.tsv>");
    let root = Path::new(&args[1]);
    let mut archives = HashMap::new();
    let mut checked = 0u64;
    let mut decoded = 0u64;
    let mut pixels = 0u64;
    let mut maximum = 0usize;
    for line in std::fs::read_to_string(&args[2])?.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        ensure!(fields.len() == 8, "invalid offline verification row");
        let base = fields[0];
        if !archives.contains_key(base) {
            archives.insert(base.to_owned(), Archive::open_file_with_limits(root, base, ArchiveLimits::VITA)?);
        }
        let index: usize = fields[1].parse()?;
        let codec: u32 = fields[2].parse()?;
        let physical = [fields[3].parse::<u32>()?, fields[4].parse::<u32>()?];
        let logical = [fields[5].parse::<u32>()?, fields[6].parse::<u32>()?];
        let expected_crc = u32::from_str_radix(fields[7], 16)?;
        let raw = archives[base].read(index).with_context(||format!("{base}:{index}"))?;
        let image = compact::parse(&raw)?;
        ensure!(image.size == physical && image.logical == logical && image.encoding as u32 == codec,
            "metadata differs at {base}:{index}");
        let payload = (physical[0] as usize).div_ceil(8)*8*physical[1] as usize*4;
        let charge = if payload < 256*1024 { payload.div_ceil(64)*64 } else { payload.div_ceil(256*1024)*256*1024 };
        ensure!(charge <= 1024*1024, "RGBA allocation exceeds offline bound at {base}:{index}");
        maximum = maximum.max(charge);
        if codec == 9 {
            let (w,h,rgba) = gxt_decode::rgba8(image.payload, 1024*1024)?;
            ensure!([w,h] == physical, "GXT dimensions mismatch");
            let mut crc = flate2::Crc::new();
            crc.update(&rgba);
            ensure!(crc.sum() == expected_crc, "independent DXT pixel CRC differs at {base}:{index}");
            // Every possible language selection must equal that exact full-image
            // window, including a crop crossing a four-pixel block boundary.
            for k in 0..4 {
                let top = h as usize*k/4;
                let rows = h as usize/4;
                if rows == 0 { continue; }
                let (_,_,strip) = gxt_decode::rgba8_rows(image.payload,top,rows,1024*1024)?;
                let begin=top*w as usize*4;
                ensure!(strip == rgba[begin..begin+rows*w as usize*4], "strip mismatch at {base}:{index}/{k}");
            }
            decoded += 1;
            pixels += u64::from(w)*u64::from(h);
        }
        checked += 1;
        if checked%1000 == 0 { println!("VERIFIED entries={checked} dxt={decoded} pixels={pixels}"); }
    }
    println!("ALL_BUDGETED_ART_VERIFIED entries={checked} dxt={decoded} pixels={pixels} max_rgba_allocation={maximum}");
    Ok(())
}
