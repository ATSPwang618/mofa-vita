//! Host verification of user-supplied archives using Vita's allocation limits.
//! Reads one entry at a time and never extracts, modifies, or copies an archive.

use std::{borrow::Cow, path::Path};

use anyhow::{Context, Result, ensure};
use knife_core::format::archive::{Archive, ArchiveLimits};

fn main() -> Result<()> {
    let arg = std::env::args_os()
        .nth(1)
        .context("usage: verify_archive_file <assets-directory>")?;
    let dir = Path::new(&arg);
    for base in [
        "allpachdml",
        "allpacml",
        "allui",
        "jallpac",
        "jallpachd",
        "parts",
    ] {
        let file = Archive::open_file_with_limits(dir, base, ArchiveLimits::VITA)?;
        let mapped = Archive::open(dir, base)?;
        let mut checked = 0usize;
        let mut missing = 0usize;
        let mut max_decoded = 0usize;
        let mut total_decoded = 0u64;
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for index in 0..file.entries().len() {
            if file.entries()[index]
                .name
                .eq_ignore_ascii_case("NOFILE.EXISTS")
            {
                ensure!(file.raw(index)?.is_none(), "NOFILE index {index} was read");
                missing += 1;
                continue;
            }
            let data = file
                .read(index)
                .with_context(|| format!("{base} entry {index}"))?;
            ensure!(
                data == mapped.read(index)?,
                "file/mmap mismatch at {base} entry {index}"
            );
            if index == 0 {
                ensure!(
                    matches!(file.raw(index)?, Some(Cow::Owned(_))),
                    "file backend did not own entry bytes"
                );
            }
            checked += 1;
            max_decoded = max_decoded.max(data.len());
            total_decoded += data.len() as u64;
            // A deterministic content fingerprint for quick comparison; the
            // verification receipt records SHA-256 of this verifier and metadata.
            for byte in data {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
        }
        println!(
            "{{\"archive\":\"{base}\",\"entries\":{},\"decoded_and_compared\":{checked},\"missing\":{missing},\"max_decoded_bytes\":{max_decoded},\"total_decoded_bytes\":{total_decoded},\"decoded_fnv1a64\":\"{hash:016x}\",\"limits_bytes\":{}}}",
            file.entries().len(),
            ArchiveLimits::VITA.max_unpacked_bytes
        );
    }
    Ok(())
}
