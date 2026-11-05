//! Identity contract for the single audited immutable DXT coverage plan.
//! A different pack, archive entry, layout or payload always keeps the full quad.

pub const MANIFEST: &str = "0f161f5c868f109cbcaca0da8db2c57a8a1cce7260ad2d0335ba70952e3ae1a3";
pub const ENVELOPE_SHA256: &[u8; 64] = b"d40403d93424babb37a4a40f956cf9df19763467f7d96f089f96f00398fd2e38";

pub fn candidate(manifest: &str, archive: &str, index: usize, name: &str,
    entry: &str, logical: [u32; 2], physical: [u32; 2], envelope_bytes: usize) -> bool {
    manifest == MANIFEST && archive == "jallpac" && index == 1093
        && name == "IMG0436" && entry == "IMG0436.NXZ"
        && logical == [1750, 1165] && physical == [624, 415] && envelope_bytes == 524384
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_only_the_audited_source_descriptor() {
        assert!(candidate(MANIFEST, "jallpac", 1093, "IMG0436", "IMG0436.NXZ", [1750,1165], [624,415], 524384));
        assert!(!candidate("changed", "jallpac", 1093, "IMG0436", "IMG0436.NXZ", [1750,1165], [624,415], 524384));
        assert!(!candidate(MANIFEST, "jallpachd", 1093, "IMG0436", "IMG0436.NXZ", [1750,1165], [624,415], 524384));
        assert!(!candidate(MANIFEST, "jallpac", 1094, "IMG0436", "IMG0436.NXZ", [1750,1165], [624,415], 524384));
        assert!(!candidate(MANIFEST, "jallpac", 1093, "IMG0436", "IMG0436.NXZ", [1750,1165], [312,208], 524384));
        assert!(!candidate(MANIFEST, "jallpac", 1093, "IMG0436", "IMG0436.NXZ", [1750,1165], [624,415], 524383));
    }
}
