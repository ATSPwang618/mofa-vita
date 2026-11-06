//! Platform-neutral scaffolding for the phase 5 GPU experiments.
//!
//! These helpers describe candidate behavior and accounting only.  They are
//! deliberately not wired into a renderer by default; callers must opt in via
//! [`ExperimentFeatures`], whose default keeps every experiment disabled.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// Runtime gates for the experimental paths.  The production default is all
/// false until platform evidence enables a particular path explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExperimentFeatures {
    pub half_color: bool,
    pub tiled_targets: bool,
    pub blur_weights: bool,
    pub scene_capacity: bool,
}

impl ExperimentFeatures {
    pub const fn disabled() -> Self {
        Self { half_color: false, tiled_targets: false, blur_weights: false, scene_capacity: false }
    }
}

impl Default for ExperimentFeatures {
    fn default() -> Self { Self::disabled() }
}

/// Inputs captured at the native half-color boundary.  Keep all geometry and
/// arithmetic values in their original FP32 representation while comparing the
/// candidate's final bytes in the native UCHAR4/RGBA8 domain.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HalfColorSample {
    pub uv: [f32; 2],
    pub wpos: [f32; 2],
    pub flags: f32,
    pub denominator: f32,
    pub native_uchar4: [u8; 4],
    pub candidate_rgba8: [u8; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HalfColorDecision {
    Accepted,
    RejectedRgba8ByteDiff { index: usize },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HalfColorValidation {
    pub decision: HalfColorDecision,
    pub uv: [f32; 2],
    pub wpos: [f32; 2],
    pub flags: f32,
    pub denominator: f32,
    pub native_uchar4: [u8; 4],
    pub candidate_rgba8: [u8; 4],
    /// This validator never promotes a default color type implicitly.
    pub default_promotion_applied: bool,
}

/// Validate a half-color candidate without changing the FP32 inputs or color
/// conversion domain.  Any RGBA8 byte mismatch rejects the candidate.
pub fn validate_half_color_candidate(sample: HalfColorSample) -> HalfColorValidation {
    let decision = sample
        .native_uchar4
        .iter()
        .zip(sample.candidate_rgba8.iter())
        .position(|(native, candidate)| native != candidate)
        .map_or(HalfColorDecision::Accepted, |index| {
            HalfColorDecision::RejectedRgba8ByteDiff { index }
        });
    HalfColorValidation {
        decision,
        uv: sample.uv,
        wpos: sample.wpos,
        flags: sample.flags,
        denominator: sample.denominator,
        native_uchar4: sample.native_uchar4,
        candidate_rgba8: sample.candidate_rgba8,
        default_promotion_applied: false,
    }
}

/// Formats that the target probe is allowed to consider.  Existing image,
/// movie, R8 and compressed paths stay outside the tiled candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetFormat {
    Rgba8,
    R8,
    Compressed,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TiledTargetProbeInput {
    /// A null CPU data pointer is required for the native render-target shape.
    pub data_is_null: bool,
    pub format: TargetFormat,
    pub render_target: bool,
    pub width: u32,
    pub height: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub linear_readback_available: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileLayout {
    pub logical_width: u32,
    pub logical_height: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub logical_row_bytes: usize,
    pub padded_row_bytes: usize,
    pub logical_bytes: usize,
    pub padded_bytes: usize,
    pub padding_bytes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetStorageKind {
    Linear,
    TiledRgba8Candidate,
}

impl TileLayout {
    fn rgba8(input: TiledTargetProbeInput) -> Option<Self> {
        if input.width == 0 || input.height == 0 || input.tile_width == 0 || input.tile_height == 0 {
            return None;
        }
        let tiles_x = input.width.div_ceil(input.tile_width) as usize;
        let tiles_y = input.height.div_ceil(input.tile_height) as usize;
        let logical_row_bytes = (input.width as usize).checked_mul(4)?;
        let padded_row_bytes = tiles_x.checked_mul(input.tile_width as usize)?.checked_mul(4)?;
        let logical_bytes = logical_row_bytes.checked_mul(input.height as usize)?;
        let padded_bytes = padded_row_bytes
            .checked_mul(tiles_y.checked_mul(input.tile_height as usize)?)?;
        Some(Self {
            logical_width: input.width,
            logical_height: input.height,
            tile_width: input.tile_width,
            tile_height: input.tile_height,
            logical_row_bytes,
            padded_row_bytes,
            logical_bytes,
            padded_bytes,
            padding_bytes: padded_bytes.checked_sub(logical_bytes)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadbackPlan {
    /// A CPU-linear staging buffer is populated before readback/capture.
    LinearStaging { bytes: usize, row_bytes: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TiledTargetCandidate {
    pub storage: TargetStorageKind,
    pub layout: TileLayout,
    pub readback: ReadbackPlan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TiledTargetFallback {
    NotDataNull,
    NotRgba8,
    NotRenderTarget,
    InvalidDimensions,
    LinearReadbackUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TiledTargetProbe {
    Candidate(TiledTargetCandidate),
    Fallback { reason: TiledTargetFallback },
}

/// Probe a strict data-null RGBA8 render-target candidate.  No other texture
/// format or upload path is accepted, and no tiled readback is attempted
/// without a linear staging plan.
pub fn probe_tiled_target(input: TiledTargetProbeInput) -> TiledTargetProbe {
    if !input.data_is_null {
        return TiledTargetProbe::Fallback { reason: TiledTargetFallback::NotDataNull };
    }
    if input.format != TargetFormat::Rgba8 {
        return TiledTargetProbe::Fallback { reason: TiledTargetFallback::NotRgba8 };
    }
    if !input.render_target {
        return TiledTargetProbe::Fallback { reason: TiledTargetFallback::NotRenderTarget };
    }
    let Some(layout) = TileLayout::rgba8(input) else {
        return TiledTargetProbe::Fallback { reason: TiledTargetFallback::InvalidDimensions };
    };
    if !input.linear_readback_available {
        return TiledTargetProbe::Fallback { reason: TiledTargetFallback::LinearReadbackUnavailable };
    }
    TiledTargetProbe::Candidate(TiledTargetCandidate {
        storage: TargetStorageKind::TiledRgba8Candidate,
        layout,
        readback: ReadbackPlan::LinearStaging {
            bytes: layout.logical_bytes,
            row_bytes: layout.logical_row_bytes,
        },
    })
}

/// Exact shader-compatible weight order and formula.  `n` is the half-tap
/// count and weights are emitted for k = -n ..= n in that order.
pub fn compute_blur_weights_exact(sigma: f32, n: usize) -> Option<Vec<f32>> {
    if !sigma.is_finite() || sigma <= 0.0 || n > 16 {
        return None;
    }
    // Match the shader literal exactly; using `f32::consts::PI` would alter
    // the final bytes for some weights.
    let norm = (2.0_f32 * sigma * sigma * 3.1416_f32).sqrt();
    let mut weights = Vec::with_capacity(n.checked_mul(2)?.checked_add(1)?);
    for k in -(n as i32)..=(n as i32) {
        let x = k as f32;
        weights.push((-(x * x) / (2.0_f32 * sigma * sigma)).exp() / norm);
    }
    Some(weights)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlurWeightKey {
    pub sigma_bits: u32,
    pub half_taps: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlurWeightFallback {
    InvalidSigma,
    MoreThanSixteenHalfTaps,
    EntryExceedsByteBudget,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BlurWeightResult {
    Cached(Arc<[f32]>),
    Fallback { reason: BlurWeightFallback },
}

/// Small LRU cache for exact blur groups.  It stores at most sixteen groups and
/// four KiB of weight payload; larger taps remain on the original shader path.
#[derive(Debug, Default)]
pub struct BlurWeightCache {
    entries: HashMap<BlurWeightKey, Arc<[f32]>>,
    order: VecDeque<BlurWeightKey>,
    bytes: usize,
}

impl BlurWeightCache {
    pub const MAX_GROUPS: usize = 16;
    pub const MAX_BYTES: usize = 4 * 1024;

    pub fn get_or_compute(&mut self, sigma: f32, n: usize) -> BlurWeightResult {
        if !sigma.is_finite() || sigma <= 0.0 {
            return BlurWeightResult::Fallback { reason: BlurWeightFallback::InvalidSigma };
        }
        if n > 16 {
            return BlurWeightResult::Fallback { reason: BlurWeightFallback::MoreThanSixteenHalfTaps };
        }
        let half_taps = n as u8;
        let key = BlurWeightKey { sigma_bits: sigma.to_bits(), half_taps };
        if let Some(weights) = self.entries.get(&key).cloned() {
            self.touch(key);
            return BlurWeightResult::Cached(weights);
        }
        let Some(weights) = compute_blur_weights_exact(sigma, n) else {
            return BlurWeightResult::Fallback { reason: BlurWeightFallback::InvalidSigma };
        };
        let bytes = weights.len().saturating_mul(core::mem::size_of::<f32>());
        if bytes > Self::MAX_BYTES {
            return BlurWeightResult::Fallback { reason: BlurWeightFallback::EntryExceedsByteBudget };
        }
        while self.entries.len() >= Self::MAX_GROUPS || self.bytes.saturating_add(bytes) > Self::MAX_BYTES {
            let Some(oldest) = self.order.pop_front() else { break; };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(removed.len() * core::mem::size_of::<f32>());
            }
        }
        let weights: Arc<[f32]> = Arc::from(weights.into_boxed_slice());
        self.bytes = self.bytes.saturating_add(bytes);
        self.order.push_back(key);
        self.entries.insert(key, weights.clone());
        BlurWeightResult::Cached(weights)
    }

    fn touch(&mut self, key: BlurWeightKey) {
        if let Some(index) = self.order.iter().position(|entry| *entry == key) {
            self.order.remove(index);
        }
        self.order.push_back(key);
    }

    pub fn group_count(&self) -> usize { self.entries.len() }
    pub fn bytes_used(&self) -> usize { self.bytes }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SceneCapacity {
    One = 1,
    Two = 2,
    Four = 4,
}

impl SceneCapacity {
    pub const fn value(self) -> usize { self as usize }
    pub const fn is_fallback(self) -> bool { matches!(self, Self::One) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SceneCapacitySelection {
    pub requested: SceneCapacity,
    pub selected: SceneCapacity,
    pub used_fallback: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SceneCapacityError<E> {
    RequestedPreflightFailed { capacity: SceneCapacity, error: E },
    RequestedAllocationFailed { capacity: SceneCapacity, error: E },
    FallbackPreflightFailed { error: E },
    FallbackAllocationFailed { error: E },
}

/// Select a scene-ring capacity only after driver-memory preflight and an
/// allocation attempt.  Any multi-scene failure invokes cleanup and retries
/// capacity one; a failure at one is returned after cleanup.
pub fn select_scene_capacity<Preflight, Allocate, Cleanup, E>(
    requested: SceneCapacity,
    mut preflight: Preflight,
    mut allocate: Allocate,
    mut cleanup: Cleanup,
) -> Result<SceneCapacitySelection, SceneCapacityError<E>>
where
    Preflight: FnMut(SceneCapacity) -> Result<(), E>,
    Allocate: FnMut(SceneCapacity) -> Result<(), E>,
    Cleanup: FnMut(),
{
    let attempt = |capacity: SceneCapacity,
                   preflight: &mut Preflight,
                   allocate: &mut Allocate|
     -> Result<(), SceneCapacityError<E>> {
        preflight(capacity).map_err(|error| SceneCapacityError::RequestedPreflightFailed { capacity, error })?;
        allocate(capacity).map_err(|error| SceneCapacityError::RequestedAllocationFailed { capacity, error })
    };

    match attempt(requested, &mut preflight, &mut allocate) {
        Ok(()) => Ok(SceneCapacitySelection { requested, selected: requested, used_fallback: false }),
        Err(_error) if requested != SceneCapacity::One => {
            cleanup();
            if let Err(fallback_error) = preflight(SceneCapacity::One) {
                cleanup();
                return Err(SceneCapacityError::FallbackPreflightFailed { error: fallback_error });
            }
            if let Err(fallback_error) = allocate(SceneCapacity::One) {
                cleanup();
                return Err(SceneCapacityError::FallbackAllocationFailed { error: fallback_error });
            }
            Ok(SceneCapacitySelection { requested, selected: SceneCapacity::One, used_fallback: true })
        }
        Err(error) => {
            cleanup();
            Err(error)
        }
    }
}
