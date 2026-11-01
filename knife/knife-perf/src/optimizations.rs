//! Independently selectable implementation changes, recorded in every run header.
//! This is independent of timing probes: disabling logging does not change behavior.
use std::sync::atomic::{AtomicU32, Ordering};

pub const SHARED_ATLAS: u32 = 1 << 0;
pub const GXT_READY: u32 = 1 << 1;
pub const BATCH_ARENA: u32 = 1 << 2;
pub const GROUP_FUSED: u32 = 1 << 3;
pub const PS_FETCH: u32 = 1 << 4;
pub const ASYNC_SUBMIT: u32 = 1 << 5;
pub const COMPACT_STREAM: u32 = 1 << 6;
pub const DIRECT_STAGE: u32 = 1 << 7;
pub const TEXTURE_POOL: u32 = 1 << 8;
pub const DECODE_OWNER: u32 = 1 << 9;
pub const GRAY_R8: u32 = 1 << 10;
pub const ANIMATION_WINDOW: u32 = 1 << 11;
pub const DEPENDENCY_GRAPH: u32 = 1 << 12;
pub const SOURCE_RENAME: u32 = 1 << 13;
pub const GRADE_CACHE: u32 = 1 << 14;
pub const STREAM_INDEX: u32 = 1 << 15;
pub const COALESCE_BAKE: u32 = 1 << 16;
pub const RESOURCE_IO_CACHE: u32 = 1 << 17;
pub const STAGE_PREFIX: u32 = 1 << 18;
pub const GROUP_BATCH: u32 = 1 << 19;
pub const UI_PASS_CACHE: u32 = 1 << 20;
pub const PLAIN_FORCEALPHA: u32 = 1 << 21;
pub const UI_SOURCE_PREFETCH: u32 = 1 << 22;
pub const ARCHIVE_GLOW_BOUNDS: u32 = 1 << 23;
pub const BLEND_CHAIN: u32 = 1 << 24;
pub const PRODUCER_REUSE: u32 = 1 << 25;
pub const ASYNC_REBAKE: u32 = 1 << 26;
pub const TARGET_RING: u32 = 1 << 27;
pub const FEATURES: &[(&str, u32)] = &[
    ("shared-atlas", SHARED_ATLAS), ("gxt-ready", GXT_READY),
    ("batch-arena", BATCH_ARENA), ("group-fused", GROUP_FUSED),
    ("ps-fetch", PS_FETCH), ("async-submit", ASYNC_SUBMIT),
    ("compact-stream", COMPACT_STREAM), ("direct-stage", DIRECT_STAGE),
    ("texture-pool", TEXTURE_POOL), ("decode-owner", DECODE_OWNER),
    ("gray-r8", GRAY_R8), ("animation-window", ANIMATION_WINDOW),
    ("dependency-graph", DEPENDENCY_GRAPH), ("source-rename", SOURCE_RENAME),
    ("grade-cache", GRADE_CACHE), ("stream-index", STREAM_INDEX),
    ("coalesce-bake", COALESCE_BAKE), ("resource-io-cache", RESOURCE_IO_CACHE),
    ("stage-prefix", STAGE_PREFIX),
    ("group-batch", GROUP_BATCH),
    ("ui-pass-cache", UI_PASS_CACHE),
    ("plain-forcealpha", PLAIN_FORCEALPHA),
    ("ui-source-prefetch", UI_SOURCE_PREFETCH),
    ("archive-glow-bounds", ARCHIVE_GLOW_BOUNDS),
    ("blend-chain", BLEND_CHAIN),
    ("producer-reuse", PRODUCER_REUSE),
    ("async-rebake", ASYNC_REBAKE),
    ("target-ring", TARGET_RING),
];
// Expand only as the corresponding implementation becomes available.
pub const IMPLEMENTED: u32 = SHARED_ATLAS | GXT_READY | BATCH_ARENA | GROUP_FUSED | PS_FETCH
    | ASYNC_SUBMIT | COMPACT_STREAM | DIRECT_STAGE | TEXTURE_POOL
    | DECODE_OWNER | GRAY_R8 | ANIMATION_WINDOW
    | DEPENDENCY_GRAPH | SOURCE_RENAME | GRADE_CACHE | STREAM_INDEX
    | COALESCE_BAKE | RESOURCE_IO_CACHE | STAGE_PREFIX | GROUP_BATCH | UI_PASS_CACHE | PLAIN_FORCEALPHA | UI_SOURCE_PREFETCH | ARCHIVE_GLOW_BOUNDS
    | BLEND_CHAIN | PRODUCER_REUSE | ASYNC_REBAKE | TARGET_RING;
// Executable startup explicitly selects a profile before creating resources.
static ACTIVE: AtomicU32 = AtomicU32::new(0);
pub fn set(mask: u32) { ACTIVE.store(mask & IMPLEMENTED, Ordering::Relaxed); }
pub fn mask() -> u32 { ACTIVE.load(Ordering::Relaxed) }
#[inline]
pub fn enabled(feature: u32) -> bool { mask() & feature != 0 }
pub fn names() -> String {
    FEATURES.iter().filter(|(_, bit)| mask() & bit != 0)
        .map(|(name, _)| *name).collect::<Vec<_>>().join(",")
}
pub fn parse(text: &str) -> Result<u32, String> {
    let mut value = 0;
    for token in text.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()) {
        match token {
            "baseline" => value = 0,
            "all" => value = IMPLEMENTED,
            _ => {
                let (remove, name) = token.strip_prefix('-').map_or((false, token), |s| (true, s));
                let bit = FEATURES.iter().find(|(key, _)| *key == name)
                    .map(|(_, bit)| *bit).ok_or_else(|| format!("unknown optimization {name}"))?;
                if !remove && bit & IMPLEMENTED == 0 {
                    return Err(format!("optimization {name} is not implemented in this build"));
                }
                if remove { value &= !bit; } else { value |= bit; }
            }
        }
    }
    if value & (ASYNC_SUBMIT | COMPACT_STREAM) != 0 && value & BATCH_ARENA == 0 {
        return Err("async-submit and compact-stream require batch-arena".into());
    }
    if value & (DEPENDENCY_GRAPH | SOURCE_RENAME) != 0
        && value & (BATCH_ARENA | ASYNC_SUBMIT) != (BATCH_ARENA | ASYNC_SUBMIT) {
        return Err("dependency-graph and source-rename require batch-arena and async-submit".into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profiles_preserve_dependencies_and_reject_typos() {
        assert_eq!(parse("baseline").unwrap(), 0);
        assert_eq!(parse("all").unwrap(), IMPLEMENTED);
        assert_eq!(parse("baseline batch-arena,async-submit,compact-stream").unwrap(),
            BATCH_ARENA | ASYNC_SUBMIT | COMPACT_STREAM);
        assert!(parse("all -batch-arena").is_err());
        assert!(parse("async-submit").is_err());
        assert!(parse("compact-stream").is_err());
        assert!(parse("dependency-graph batch-arena").is_err());
        assert!(parse("source-rename async-submit").is_err());
        assert_eq!(parse("batch-arena async-submit dependency-graph source-rename").unwrap(),
            BATCH_ARENA | ASYNC_SUBMIT | DEPENDENCY_GRAPH | SOURCE_RENAME);
        assert!(parse("all typo").is_err());
        assert_eq!(parse("all -ps-fetch").unwrap(), IMPLEMENTED & !PS_FETCH);
    }
}
