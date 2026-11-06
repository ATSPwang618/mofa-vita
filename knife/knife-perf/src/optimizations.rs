//! Independently selectable implementation changes, recorded in every run header.
//! This is independent of timing probes: disabling logging does not change behavior.
use std::sync::atomic::{AtomicU64, Ordering};

pub const SHARED_ATLAS: u64 = 1u64 << 0;
pub const GXT_READY: u64 = 1u64 << 1;
pub const BATCH_ARENA: u64 = 1u64 << 2;
pub const GROUP_FUSED: u64 = 1u64 << 3;
pub const PS_FETCH: u64 = 1u64 << 4;
pub const ASYNC_SUBMIT: u64 = 1u64 << 5;
pub const COMPACT_STREAM: u64 = 1u64 << 6;
pub const DIRECT_STAGE: u64 = 1u64 << 7;
pub const TEXTURE_POOL: u64 = 1u64 << 8;
pub const DECODE_OWNER: u64 = 1u64 << 9;
pub const GRAY_R8: u64 = 1u64 << 10;
pub const ANIMATION_WINDOW: u64 = 1u64 << 11;
pub const DEPENDENCY_GRAPH: u64 = 1u64 << 12;
pub const SOURCE_RENAME: u64 = 1u64 << 13;
pub const GRADE_CACHE: u64 = 1u64 << 14;
pub const STREAM_INDEX: u64 = 1u64 << 15;
pub const COALESCE_BAKE: u64 = 1u64 << 16;
pub const RESOURCE_IO_CACHE: u64 = 1u64 << 17;
pub const STAGE_PREFIX: u64 = 1u64 << 18;
pub const GROUP_BATCH: u64 = 1u64 << 19;
pub const UI_PASS_CACHE: u64 = 1u64 << 20;
pub const PLAIN_FORCEALPHA: u64 = 1u64 << 21;
pub const UI_SOURCE_PREFETCH: u64 = 1u64 << 22;
pub const ARCHIVE_GLOW_BOUNDS: u64 = 1u64 << 23;
pub const BLEND_CHAIN: u64 = 1u64 << 24;
pub const PRODUCER_REUSE: u64 = 1u64 << 25;
pub const ASYNC_REBAKE: u64 = 1u64 << 26;
pub const TARGET_RING: u64 = 1u64 << 27;
pub const PS_OP7: u64 = 1u64 << 28;
pub const SPARSE_COVERAGE: u64 = 1u64 << 29;
pub const PS_PLAIN_MATERIAL: u64 = 1u64 << 30;
pub const GROUP_BOUNDS: u64 = 1u64 << 31;
pub const HALF_COLOR: u64 = 1u64 << 32;
pub const TILED_TARGETS: u64 = 1u64 << 33;
pub const BLUR_WEIGHTS: u64 = 1u64 << 34;
pub const RT_SCENES_2: u64 = 1u64 << 35;
pub const RT_SCENES_4: u64 = 1u64 << 36;
pub const PS_FULL_MATERIAL: u64 = 1u64 << 37;
pub const PS_ALL_OPERATIONS: u64 = 1u64 << 38;
pub const PS_PLAIN_OP7: u64 = 1u64 << 39;
pub const PS_CHAIN_OP7: u64 = 1u64 << 40;
pub const PS_GROUP_MATERIAL: u64 = 1u64 << 42;
pub const PS_MATERIAL_VARIANTS: u64 = 1u64 << 41;
pub const FEATURES: &[(&str, u64)] = &[
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
    ("ps-op7", PS_OP7),
    ("sparse-coverage", SPARSE_COVERAGE),
    ("ps-plain-material", PS_PLAIN_MATERIAL),
    ("group-bounds", GROUP_BOUNDS),
    ("half-color", HALF_COLOR),
    ("tiled-targets", TILED_TARGETS),
    ("blur-weights", BLUR_WEIGHTS),
    ("rt-scenes-2", RT_SCENES_2),
    ("rt-scenes-4", RT_SCENES_4),
    ("ps-full-material", PS_FULL_MATERIAL),
    ("ps-all-operations", PS_ALL_OPERATIONS),
    ("ps-plain-op7", PS_PLAIN_OP7),
    ("ps-chain-op7", PS_CHAIN_OP7),
    ("ps-group-material", PS_GROUP_MATERIAL),
    ("ps-material-variants", PS_MATERIAL_VARIANTS),
];
// Bits 0..28 are admitted by the existing renderer profile. Sparse coverage
// and newer capacity experiments remain explicit opt-ins and are excluded from
// `all` until their hardware contracts are promoted.
pub const IMPLEMENTED: u64 = SHARED_ATLAS | GXT_READY | BATCH_ARENA | GROUP_FUSED | PS_FETCH
    | ASYNC_SUBMIT | COMPACT_STREAM | DIRECT_STAGE | TEXTURE_POOL
    | DECODE_OWNER | GRAY_R8 | ANIMATION_WINDOW | DEPENDENCY_GRAPH | SOURCE_RENAME
    | GRADE_CACHE | STREAM_INDEX | COALESCE_BAKE | RESOURCE_IO_CACHE | STAGE_PREFIX
    | GROUP_BATCH | UI_PASS_CACHE | PLAIN_FORCEALPHA | UI_SOURCE_PREFETCH
    | ARCHIVE_GLOW_BOUNDS | BLEND_CHAIN | PRODUCER_REUSE | ASYNC_REBAKE | TARGET_RING | PS_OP7
    // The plain OP1/OP6 material contract is the first r5 candidate and is
    // safe to include in the default profile.  Later experiments remain
    // explicit opt-ins until their hardware pixel-diff/performance gates pass.
    | PS_PLAIN_MATERIAL;
/// Latest candidate profile for an explicitly requested hardware test build.
/// `all` remains the admitted baseline; this preset does not imply GPU acceptance.
pub const HARDWARE_TEST_PROFILE: &str = "hardware-test";
pub const HARDWARE_TEST: u64 = IMPLEMENTED | PS_ALL_OPERATIONS | GROUP_BOUNDS
    | HALF_COLOR | TILED_TARGETS | BLUR_WEIGHTS | RT_SCENES_2 | PS_PLAIN_OP7 | PS_CHAIN_OP7 | PS_MATERIAL_VARIANTS | PS_GROUP_MATERIAL;
pub const KNOWN: u64 = IMPLEMENTED | SPARSE_COVERAGE | PS_PLAIN_MATERIAL | GROUP_BOUNDS
    | HALF_COLOR | TILED_TARGETS | BLUR_WEIGHTS | RT_SCENES_2 | RT_SCENES_4 | PS_FULL_MATERIAL | PS_ALL_OPERATIONS | PS_PLAIN_OP7 | PS_CHAIN_OP7 | PS_MATERIAL_VARIANTS | PS_GROUP_MATERIAL;
// Executable startup explicitly selects a profile before creating resources.
static ACTIVE: AtomicU64 = AtomicU64::new(0);
pub fn set(mask: u64) {
    let mut mask = mask & KNOWN;
    if mask & (RT_SCENES_2 | RT_SCENES_4) == (RT_SCENES_2 | RT_SCENES_4) {
        // Direct diagnostic callers get the same safe capacity-1 resolution as C.
        mask &= !(RT_SCENES_2 | RT_SCENES_4);
    }
    ACTIVE.store(mask, Ordering::Relaxed);
}
pub fn mask() -> u64 { ACTIVE.load(Ordering::Relaxed) }
#[inline]
pub fn enabled(feature: u64) -> bool { mask() & feature != 0 }
pub fn names() -> String {
    FEATURES.iter().filter(|(_, bit)| mask() & bit != 0)
        .map(|(name, _)| *name).collect::<Vec<_>>().join(",")
}
pub fn parse(text: &str) -> Result<u64, String> {
    let mut value: u64 = 0;
    for token in text.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()) {
        match token {
            "baseline" => value = 0,
            "all" => value = IMPLEMENTED,
            HARDWARE_TEST_PROFILE => value = HARDWARE_TEST,
            _ => {
                let (remove, name) = token.strip_prefix('-').map_or((false, token), |s| (true, s));
                let bit = FEATURES.iter().find(|(key, _)| *key == name)
                    .map(|(_, bit)| *bit).ok_or_else(|| format!("unknown optimization {name}"))?;
                if !remove && bit & KNOWN == 0 {
                    return Err(format!("optimization {name} is not known in this build"));
                }
                if remove { value &= !bit; } else { value |= bit; }
            }
        }
    }
    if value & (RT_SCENES_2 | RT_SCENES_4) == (RT_SCENES_2 | RT_SCENES_4) {
        return Err("rt-scenes-2 and rt-scenes-4 are mutually exclusive".into());
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StartupProfile {
    pub mask: u64,
    pub legacy_expanded: bool,
}

/// Resolve the compiled default and optional complete data-root override.
/// Only the hardware test build expands the exact previous stock profiles;
/// custom tokens (especially explicit removals) retain complete-override meaning.
pub fn resolve_startup(compiled_profile: &str, file_profile: Option<&str>) -> Result<StartupProfile, String> {
    let legacy_expanded = compiled_profile.trim() == HARDWARE_TEST_PROFILE
        && file_profile.is_some_and(|text| matches!(text.trim(),
            "all" | "all ps-all-operations" | "all ps-full-material"));
    Ok(StartupProfile {
        mask: parse(if legacy_expanded { HARDWARE_TEST_PROFILE }
            else { file_profile.unwrap_or(compiled_profile) })?,
        legacy_expanded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hardware_test_enables_latest_candidates_without_rollback_modes() {
        // Baseline bits 0..28,30 plus candidates 31..35,38. Sparse rollback,
        // capacity four and the superseded full-material path stay disabled.
        assert_eq!(parse("hardware-test").unwrap(), 0x7cf_dfff_ffff);
        assert_eq!(parse("hardware-test -half-color -tiled-targets -rt-scenes-2").unwrap(),
            0x7c4_dfff_ffff);
        assert!(parse("hardware-test rt-scenes-4").is_err());
        assert!(parse("hardware-test typo").is_err());
    }

    #[test]
    fn hardware_test_startup_expands_only_legacy_stock_files() {
        for stock in ["all", "all ps-all-operations", "all ps-full-material", "\r\nall ps-all-operations\r\n"] {
            let selected = resolve_startup("hardware-test", Some(stock)).unwrap();
            assert_eq!(selected.mask, 0x7cf_dfff_ffff, "stock profile {stock:?}");
            assert!(selected.legacy_expanded, "stock profile {stock:?}");
        }
        assert_eq!(resolve_startup("hardware-test", None).unwrap(),
            StartupProfile { mask: 0x7cf_dfff_ffff, legacy_expanded: false });
    }

    #[test]
    fn startup_preserves_explicit_custom_profiles_and_disables() {
        for (custom, expected) in [
            ("baseline", 0),
            ("all -half-color", 0x5fff_ffff),
            ("all ps-all-operations -tiled-targets", 0x40_5fff_ffff),
            ("all -ps-fetch ps-all-operations", 0x40_5fff_ffef),
            ("hardware-test -half-color -rt-scenes-2 rt-scenes-4", 0x7d6_dfff_ffff),
            ("all ps-full-material -ps-full-material", 0x5fff_ffff),
            ("all,ps-all-operations", 0x40_5fff_ffff),
        ] {
            assert_eq!(resolve_startup("hardware-test", Some(custom)).unwrap(),
                StartupProfile { mask: expected, legacy_expanded: false }, "custom profile {custom:?}");
        }
        // Migration belongs to this named test default only, including for
        // equivalent-but-custom compiled profiles and ordinary baseline builds.
        for compiled in ["all", "all ps-all-operations", "hardware-test -half-color"] {
            assert_eq!(resolve_startup(compiled, Some("all")).unwrap(),
                StartupProfile { mask: 0x5fff_ffff, legacy_expanded: false });
        }
    }

    #[test]
    fn startup_rejects_invalid_custom_profiles_and_capacity_conflicts() {
        for custom in ["all typo", "hardware-test rt-scenes-4", "all -batch-arena"] {
            assert!(resolve_startup("hardware-test", Some(custom)).is_err());
        }
        assert!(resolve_startup("typo", None).is_err());
        assert_eq!(resolve_startup("typo", Some("baseline")).unwrap().mask, 0);
    }

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

    #[test]
    fn capacity_bits_are_explicit_and_render_target_modes_are_exclusive() {
        assert_eq!(parse("all").unwrap() & SPARSE_COVERAGE, 0);
        assert_eq!(parse("all").unwrap() & PS_FULL_MATERIAL, 0);
        assert_eq!(parse("all").unwrap() & PS_ALL_OPERATIONS,0);
        assert_eq!(parse("all ps-all-operations").unwrap(),IMPLEMENTED|PS_ALL_OPERATIONS);
        assert_eq!(parse("all ps-full-material").unwrap(),IMPLEMENTED|PS_FULL_MATERIAL);
        assert_eq!(parse("baseline ps-full-material").unwrap(),PS_FULL_MATERIAL);
        assert_eq!(parse("all ps-full-material -ps-full-material").unwrap(),IMPLEMENTED);
        assert_eq!(parse("baseline sparse-coverage ps-plain-material half-color").unwrap(),
            SPARSE_COVERAGE | PS_PLAIN_MATERIAL | HALF_COLOR);
        assert_eq!(parse("baseline rt-scenes-2").unwrap(), RT_SCENES_2);
        assert_eq!(parse("baseline rt-scenes-4").unwrap(), RT_SCENES_4);
        assert!(parse("baseline rt-scenes-2 rt-scenes-4").is_err());
        set(SPARSE_COVERAGE | PS_PLAIN_MATERIAL | RT_SCENES_4);
        assert!(enabled(SPARSE_COVERAGE));
        assert!(enabled(PS_PLAIN_MATERIAL));
        assert!(enabled(RT_SCENES_4));
        assert!(!enabled(RT_SCENES_2));
        set(RT_SCENES_2 | RT_SCENES_4);
        assert_eq!(mask(), 0);
        set(0);
    }

    #[test]
    fn ps_op7_profile_can_be_toggled_without_changing_existing_ps_fetch_profiles() {
        let ps_op7 = 1u64 << 28;
        assert_ne!(parse("all").unwrap() & ps_op7, 0);
        assert_eq!(parse("baseline ps-op7").unwrap(), ps_op7);
        assert_eq!(parse("all -ps-op7").unwrap(), IMPLEMENTED & !ps_op7);
        assert_eq!(parse("all -ps-fetch").unwrap(), IMPLEMENTED & !PS_FETCH);
        assert_eq!(parse("all -ps-fetch -ps-op7").unwrap(), IMPLEMENTED & !(PS_FETCH | ps_op7));
    }
}
