//! Generated markers redirect logical resource names to sibling media files.
//! Physical files keep truthful suffixes; scripts retain their original names.
pub const VIDEO_MARKER_SUFFIX: &str = ".krkr-mp4";
pub const VIDEO_MARKER: &[u8] = b"KRKR-MP4-1\n";
pub const VIDEO_SUFFIX: &str = ".mp4";

/// A bounded, versioned link to one sibling file, never an OS symlink.
pub const LINK_SUFFIX: &str = ".krkr-link";
pub const LINK_HEADER: &[u8] = b"KRKR-LINK-1\n";
pub const MAX_LINK_BYTES: usize = 4096;
pub const MAX_LINK_DEPTH: usize = 16;

pub fn encode_link(target: &str) -> crate::Result<Vec<u8>> {
    validate_leaf(target)?;
    let mut bytes = LINK_HEADER.to_vec();
    bytes.extend_from_slice(target.as_bytes());
    if bytes.len() > MAX_LINK_BYTES {
        return Err(crate::Error::Limit("resource link"));
    }
    Ok(bytes)
}

pub fn decode_link(bytes: &[u8]) -> crate::Result<&str> {
    if bytes.len() > MAX_LINK_BYTES {
        return Err(crate::Error::Limit("resource link"));
    }
    let target = bytes
        .strip_prefix(LINK_HEADER)
        .and_then(|b| std::str::from_utf8(b).ok())
        .ok_or(crate::Error::Format("invalid resource link"))?;
    validate_leaf(target)?;
    Ok(target)
}

fn validate_leaf(target: &str) -> crate::Result<()> {
    if target.is_empty()
        || target == "."
        || target == ".."
        || target.ends_with(['.', ' '])
        || target
            .chars()
            .any(|c| c.is_control() || "/\\:\"<>|?*".contains(c))
    {
        return Err(crate::Error::Format(
            "resource link must name one sibling file",
        ));
    }
    Ok(())
}

pub fn sibling(source: &[u16], leaf: &str) -> Vec<u16> {
    let at = source
        .iter()
        .rposition(|&c| c == 47 || c == 62)
        .map_or(0, |i| i + 1);
    [
        source[..at].to_vec(),
        crate::name::fold(&crate::name::units(leaf)),
    ]
    .concat()
}
