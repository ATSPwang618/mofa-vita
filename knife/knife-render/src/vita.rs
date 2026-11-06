//! Native GXM implementation of the renderer facade.
//!
//! Encoders retain resources and defer commands until submission, as on desktop.
//! Compatible draws share submissions; eligible consecutive pixel-local PS layers
//! share a bounded blend-chain shader. The native owner retains all GPU fences.

#![cfg_attr(not(target_os = "vita"), allow(dead_code))]

use knife_perf::{Metric, Scope, Gauge};
use std::{cell::{Cell, RefCell}, collections::{HashMap, HashSet}, rc::{Rc, Weak}, sync::Arc};

use anyhow::{Context, Result, anyhow, bail, ensure};

#[cfg(target_os = "vita")]
use knife_platform_vita::gxm::{EffectBlend, EffectShader, Gxm, TextureFormat, TextureId};
#[cfg(target_os = "vita")]
use knife_platform_vita::gxt::{GxtFormat, GxtTexture};
#[cfg(all(test, not(target_os = "vita")))]
#[path = "../../knife-platform-vita/src/gxt.rs"]
mod host_gxt;
#[cfg(all(test, not(target_os = "vita")))]
use host_gxt::{GxtFormat, GxtTexture};
#[cfg(all(test, not(target_os = "vita")))]
use test_backend::{EffectBlend, EffectShader, Gxm, TextureFormat, TextureId};

pub use knife_render_api::{
    Blend, BlurMode, CANVAS_H, CANVAS_W, PsOp, SOFT_LUT_ROWS, SOFT_LUT_W,
    Shade, SoftClip, Vertex,
};

// The scene/effect chain renders at the display's content size and the present
// pass places it inside the 960x544 scanout (2 px letterbox top and bottom).
// Script coordinates remain the 1920x1080 logical canvas.
pub const RENDER_W: u32 = 960;
pub const RENDER_H: u32 = 540;

// Bound recorded CPU work independently of native texture/vertex ring budgets.
// Exceeding either limit is a reported error, never a partial frame submission.
const MAX_FRAME_COMMANDS: usize = 32_768;
const MAX_FRAME_VERTEX_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEFERRED_RETIREMENTS: usize = 256;
// Compressed atlas bytes stay in main RAM; only textures used by a submitted
// frame occupy the cache. Leave CDRAM for canvas, scene targets, fonts and baking.
const DEFERRED_TEXTURE_BYTES: usize = knife_perf::memory_budget::GPU_RESOURCE_BYTES;
const DEFERRED_SOURCE_BYTES: usize = knife_perf::memory_budget::GPU_SOURCE_BYTES;
/// Upper bound on one merged draw's vertex count (16-bit indices into the ring).
const MERGED_DRAW_VERTICES: usize = 16_000;
#[cfg(test)]
const CDRAM_GRANULE: usize = 256 * 1024;

// A game-owned idle image cache may participate in the same native reservation.
// The callback is invoked for a validated cache miss, before its native upload.
type FrameAdmission<'a> = dyn FnMut(&mut dyn FnMut() -> Result<()>) -> Result<()> + 'a;

/// No commands from the pending frame were submitted. The game may retain
/// the previous complete targets and show its ordinary resource-error UI.
#[derive(Debug)]
pub struct FramePreparationError;
impl std::fmt::Display for FramePreparationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Vita frame resource preparation failed before submission")
    }
}
impl std::error::Error for FramePreparationError {}

/// Only clean allocation/admission pressure may trigger optional-cache recovery.
/// Driver/retirement errors must not be mistaken for a shortage of free bytes.
#[derive(Debug)]
struct TextureMemoryPressure;
impl std::fmt::Display for TextureMemoryPressure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("native texture memory pressure")
    }
}
impl std::error::Error for TextureMemoryPressure {}

/// Deferred-atlas preparation accounting. A pre-baked GXT is only *registered* by
/// the upload path, so without these counters its read/parse/upload work is
/// invisible and hides inside render_us.
#[cfg(target_os = "vita")]
mod deferred_metrics {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};
    pub static ITEMS: AtomicU64 = AtomicU64::new(0);
    pub static ITEM_US: AtomicU64 = AtomicU64::new(0);
    pub static UPLOAD_US: AtomicU64 = AtomicU64::new(0);
    pub static BAKE_US: AtomicU64 = AtomicU64::new(0);
    pub fn add(counter: &AtomicU64, elapsed: Duration) {
        counter.fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
    }
    pub fn snapshot() -> (u64, u64, u64, u64) {
        (ITEMS.load(Ordering::Relaxed), ITEM_US.load(Ordering::Relaxed),
         UPLOAD_US.load(Ordering::Relaxed), BAKE_US.load(Ordering::Relaxed))
    }
    /// Counts one prepared atlas for the whole iteration, including early exits.
    pub struct Item(pub Instant);
    impl Item { pub fn start() -> Self { Self(Instant::now()) } }
    impl Drop for Item {
        fn drop(&mut self) {
            ITEMS.fetch_add(1, Ordering::Relaxed);
            add(&ITEM_US, self.0.elapsed());
        }
    }
}

/// DEV tracing for the five menu buttons whose object/Quad records exist in
/// game-runtime.log. This observes the recorded, baked and submitted boundaries;
/// it neither reads GPU pixels nor claims the SDK accepted pixels correctly.
#[cfg(target_os = "vita")]
mod menu_draw_diagnostic {
    use super::*;
    use std::{fmt, io::Write, sync::atomic::{AtomicU32, Ordering}};

    static RECORDED: AtomicU32 = AtomicU32::new(0);
    static SUBMITTED: AtomicU32 = AtomicU32::new(0);
    static COMPRESSED_UPLOADS: AtomicU32 = AtomicU32::new(0);
    static CG_QUADS: AtomicU32 = AtomicU32::new(0);
    static CG_SUBMITTED: AtomicU32 = AtomicU32::new(0);
    static RESIDENCY_LOADS: AtomicU32 = AtomicU32::new(0);
    static RESIDENCY_EVICTIONS: AtomicU32 = AtomicU32::new(0);

    fn enabled() -> bool {
        option_env!("KNIFE_VITA_DEV_DIAGNOSTICS") == Some("1")
    }

    pub(super) fn record(args: fmt::Arguments<'_>) {
        // Close each append so the emulator host can observe the stage now.
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true)
            .open("ux0:data/knife-woh-dev/game-runtime.log")
        {
            let _ = writeln!(file, "{args}");
            let _ = file.sync_all();
        }
    }

    pub(super) fn texture(event: &str, texture: &NativeTexture, label: &str) {
        if enabled() {
            record(format_args!("GPU_OWNER event={event} native={:p} id={:?} physical={:?} bpp={} target={} deferred={} label={label:?}",
                texture, texture.id.borrow(), texture.size, texture.bpp, texture.render_target,
                texture.deferred.is_some()));
        }
    }

    pub(super) fn residency(event: &str, texture: &NativeTexture, pinned: usize) {
        if !enabled() { return; }
        let counter = if event == "RELOAD" { &RESIDENCY_LOADS } else { &RESIDENCY_EVICTIONS };
        let sequence = counter.fetch_add(1, Ordering::Relaxed) + 1;
        if sequence <= 160 {
            record(format_args!("CG_RESIDENCY event={event} sequence={sequence} epoch={} native={:p} label={:?} physical={:?} pinned={pinned} resident_bytes={}",
                texture.device.epoch.get(), texture,
                texture.deferred.as_ref().map(|source| source.label.as_str()), texture.size,
                texture.device.resident_deferred_bytes.get()));
        }
    }

    pub(super) fn residency_stats(device: &Device) {
        if enabled() && device.epoch.get() % 120 == 0 {
            record(format_args!("CG_RESIDENCY_STATS epoch={} reloads={} evictions={} resident_bytes={}",
                device.epoch.get(), RESIDENCY_LOADS.load(Ordering::Relaxed),
                RESIDENCY_EVICTIONS.load(Ordering::Relaxed), device.resident_deferred_bytes.get()));
        }
    }

    pub(super) fn frame_pressure(commands: &[Command]) {
        if !enabled() { return; }
        let mut seen = HashSet::new();
        let mut add = |texture: &Rc<NativeTexture>| {
            if seen.insert(Rc::as_ptr(texture)) {
                record(format_args!("GPU_FRAME_PIN native={:p} id={:?} physical={:?} native_refs={} label={:?}",
                    &**texture, texture.id.borrow(), texture.size, Rc::strong_count(texture),
                    texture.deferred.as_ref().map(|source| source.label.as_str())));
            }
        };
        for command in commands {
            match command {
                Command::Begin { target, .. } => add(target),
                Command::Copy { src, dst, .. } => { add(src); add(dst); }
                Command::Draw { textures, .. } => textures.iter().flatten().for_each(&mut add),
                Command::End | Command::GroupCut(_) => {}
            }
        }
    }

    fn is_menu(texture: &NativeTexture) -> bool {
        texture.deferred.as_ref().is_some_and(|source| {
            matches!(source.label.as_str(), "menu_btn_en" | "menu_btn_zc")
        })
    }

    fn button(uvs: impl Iterator<Item = [f32; 2]>) -> Option<u32> {
        let [mut x, mut y] = [f32::INFINITY; 2];
        for uv in uvs { x = x.min(uv[0]); y = y.min(uv[1]); }
        if (x * 1748.0 - 868.0).abs() > 1.0 { return None; }
        // These are source atlas rows, not destination Y coordinates. In
        // particular object 173 uses row 566, as recorded by Pen::spr.
        [16.0, 126.0, 236.0, 566.0, 672.0].iter().position(|row| {
            (y * 840.0 - row).abs() <= 1.0
        }).map(|index| 170 + index as u32)
    }

    fn once(seen: &AtomicU32, object: u32, alpha: f32) -> Option<&'static str> {
        if !alpha.is_finite() || alpha <= 0.0 { return None; }
        let solid = alpha >= 0.999_99;
        let bit = 1 << (object - 170 + if solid { 5 } else { 0 });
        (seen.fetch_or(bit, Ordering::Relaxed) & bit == 0)
            .then_some(if solid { "solid" } else { "fade" })
    }

    pub(super) fn quad(q: &Quad, target: &RenderTarget, vertices: &[Vertex], scale: [f32; 2]) {
        if cg_once(&CG_QUADS, &q.tex.native, q.color[3]) {
            let geometry: Vec<_> = vertices.iter().take(6).map(|v| (v.pos, v.uv)).collect();
            record(format_args!("CG_RENDER QUAD label={:?} cache_epoch={} logical={:?} physical={:?} source_id={:?} target_id={:?} target_logical={:?} target_physical={:?} pos={:?} uv={:?} color={:?} shade={:?} blend={:?} clip={:?} soft={} rule={} vertices_len={} geometry={geometry:?}",
                q.tex.native.deferred.as_ref().map(|s| s.label.as_str()),
                q.tex.native.device.epoch.get(), [q.tex.width, q.tex.height], q.tex.native.size,
                q.tex.native.id.borrow(), target.tex.native.id.borrow(),
                [target.tex.width, target.tex.height], target.tex.native.size, q.pos, q.uv, q.color,
                q.shade, q.blend, q.clip, q.soft.is_some(), q.rule.is_some(), vertices.len()));
        }
        if !enabled() || !is_menu(&q.tex.native) || [q.tex.width, q.tex.height] != [1748, 840] {
            return;
        }
        let Some(object) = button(q.uv.iter().copied()) else { return; };
        let Some(phase) = once(&RECORDED, object, q.color[3]) else { return; };
        record(format_args!(
            "MENU_RENDER QUAD object={object} phase={phase} cache_epoch={} source_logical={:?} source_physical={:?} source_resident={:?} source_rt={} source_srgb={} target_logical={:?} target_physical={:?} target_resident={:?} target_rt={} target_srgb={} scale={scale:?} pos={:?} uv={:?} color={:?} shade={:?} blend={:?} clip={:?} soft={} rule={} vertices_len={} vertices_first={:?}",
            q.tex.native.device.epoch.get(), [q.tex.width, q.tex.height], q.tex.native.size,
            q.tex.native.id.borrow(), q.tex.native.render_target, q.tex.native.srgb,
            [target.tex.width, target.tex.height], target.tex.native.size,
            target.tex.native.id.borrow(), target.tex.native.render_target, target.tex.native.srgb,
            q.pos, q.uv, q.color, q.shade, q.blend, q.clip, q.soft.is_some(), q.rule.is_some(),
            vertices.len(), &vertices[..vertices.len().min(18)]));
    }

    fn cg_once(seen: &AtomicU32, texture: &NativeTexture, alpha: f32) -> bool {
        if !enabled() || !alpha.is_finite() || alpha <= 0.0 { return false; }
        let Some(source) = texture.deferred.as_ref() else { return false; };
        let Some(index) = ["img1227", "img2376", "img2326", "img2328"].iter()
            .position(|label| source.label.eq_ignore_ascii_case(label)) else { return false; };
        let phase = usize::from(alpha >= 0.999_99)
            + 2 * usize::from(texture.device.epoch.get() >= 2100);
        let bit = 1 << (index * 4 + phase);
        seen.fetch_or(bit, Ordering::Relaxed) & bit == 0
    }

    pub(super) fn compressed_upload(texture: &NativeTexture, info: GxtDescriptor) {
        if !enabled() || COMPRESSED_UPLOADS.fetch_add(1, Ordering::Relaxed) >= 6 { return; }
        record(format_args!(
            "GXT_UPLOAD label={} physical={:?} backing={:?} format={:?} payload_bytes={} cdram_bytes={:?} resident_id={:?} source_srgb=false sampler_request=linear_clamp runtime_decode=false runtime_alter=false",
            texture.deferred.as_ref().map_or("", |source| source.label.as_str()),
            info.physical, info.backing, info.format, info.payload_bytes,
            info.allocation_bytes(), texture.id.borrow()));
    }

    pub(super) fn draw_begin(device: &Device, command_index: usize, vertices: &[Vertex],
        shader: EffectShader, blend: EffectBlend, textures: &[Option<Rc<NativeTexture>>; 4],
        ids: &[Option<TextureId>; 4], params: [f32; 8], target: Option<&NativeTexture>)
        -> Option<(u32, &'static str)>
    {
        if !enabled() { return None; }
        let source = textures[0].as_ref()?;
        if cg_once(&CG_SUBMITTED, source, vertices.first().map_or(0.0, |v| v.color[3])) {
            let geometry: Vec<_> = vertices.iter().take(6).map(|v| (v.pos, v.uv)).collect();
            record(format_args!("CG_RENDER DRAW_BEGIN label={:?} cache_epoch={} command={command_index} source_ids={ids:?} source_physical={:?} source_rt={} source_srgb={} target_id={:?} target_physical={:?} shader={shader:?} blend={blend:?} params={params:?} vertices_len={} geometry={geometry:?}",
                source.deferred.as_ref().map(|s| s.label.as_str()), device.epoch.get(),
                source.size, source.render_target, source.srgb,
                target.map(|t| t.id.borrow().clone()), target.map(|t| t.size), vertices.len()));
        }
        if !is_menu(source) { return None; }
        let object = button(vertices.iter().map(|vertex| vertex.uv))?;
        let phase = once(&SUBMITTED, object, vertices.first()?.color[3])?;
        record(format_args!(
            "MENU_RENDER DRAW_BEGIN object={object} phase={phase} cache_epoch={} command={command_index} source_ids={ids:?} source_physical={:?} source_rt={} source_srgb={} target_id={:?} target_physical={:?} target_rt={:?} target_srgb={:?} shader={shader:?} blend={blend:?} params={params:?} sampler_request=linear_clamp descriptor_readback=unobserved native_memblock_uid=unobserved vertices_len={} vertices_first={:?}",
            device.epoch.get(), source.size, source.render_target, source.srgb,
            target.map(|target| target.id.borrow().clone()), target.map(|target| target.size),
            target.map(|target| target.render_target), target.map(|target| target.srgb),
            vertices.len(), &vertices[..vertices.len().min(18)]));
        Some((object, phase))
    }

    pub(super) fn draw_end(trace: Option<(u32, &'static str)>, result: &std::result::Result<(), String>) {
        if let Some((object, phase)) = trace {
            record(format_args!("MENU_RENDER DRAW_END object={object} phase={phase} result={result:?}"));
        }
    }
}

/// Native Vita works and displays in the same encoded 8-bit colour domain.
pub fn output_level(v: u8) -> u8 { v }

/// Captures/previews use the same bytes as the native scanout copy.
pub fn target_level(v: u8) -> u8 { v }

#[derive(Clone)]
pub struct RuleMask {
    pub tex: TexRef,
    pub progress: f32,
    pub flip: u8,
    pub invert: bool,
    pub soft: f32,
}

impl RuleMask {
    pub fn new(tex: TexRef, progress: f32) -> Self {
        Self {
            tex,
            progress,
            flip: 0,
            invert: false,
            soft: 128.0,
        }
    }
    fn flags(&self) -> f32 {
        rule_flags(self.flip, self.invert, self.soft)
    }
}

fn rule_flags(flip: u8, invert: bool, soft: f32) -> f32 {
    ((flip & 3) as u32 | (invert as u32) << 2 | (soft.clamp(1.0, 255.0) as u32) << 3) as f32
}

/// The GXM owner is retained by each native texture, including encoder references.
/// Neither this owner nor a texture may leave the creating thread.
struct Device {
    gxm: RefCell<Gxm>,
    retired: RefCell<Vec<TextureId>>,
    retirement_error: RefCell<Option<String>>,
    deferred: RefCell<Vec<Weak<NativeTexture>>>,
    deferred_budget: Cell<usize>,
    // Committed GPU allocations only. Keep admission O(1) until either this
    // policy budget or the native CDRAM allowance actually needs reclamation.
    resident_deferred_bytes: Cell<usize>,
    encoded_bytes: Cell<usize>,
    epoch: Cell<u64>,
    #[cfg(test)]
    admission_scans: Cell<usize>,
    #[cfg(test)]
    preparation_sorts: Cell<usize>,
}

impl Device {
    fn drain(&self) -> Result<()> {
        if let Some(error) = self.retirement_error.borrow().as_ref() {
            bail!("previous GXM texture retirement failed: {error}");
        }
        if self.retired.borrow().is_empty() {
            return Ok(());
        }
        let mut gxm = self
            .gxm
            .try_borrow_mut()
            .map_err(|_| anyhow!("GXM owner is already in use"))?;
        let pending = std::mem::take(&mut *self.retired.borrow_mut());
        for id in pending {
            if let Err(error) = gxm.destroy_texture(&id) {
                *self.retirement_error.borrow_mut() = Some(error.clone());
                bail!("GXM deferred texture retirement: {error}");
            }
        }
        Ok(())
    }

    fn call<T>(&self, f: impl FnOnce(&mut Gxm) -> std::result::Result<T, String>) -> Result<T> {
        self.drain()?;
        let value = {
            let mut gxm = self
                .gxm
                .try_borrow_mut()
                .map_err(|_| anyhow!("GXM owner is already in use"))?;
            f(&mut gxm).map_err(anyhow::Error::msg)
        };
        let retirement = self.drain();
        match (value, retirement) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    fn retire(&self, id: &TextureId) {
        // Native destroy fences outstanding use. Never retry a partly released ID.
        if self.retirement_error.borrow().is_some() {
            return;
        }
        if let Ok(mut gxm) = self.gxm.try_borrow_mut() {
            if let Err(error) = gxm.destroy_texture(id) {
                tracing::error!(%error, "GXM texture retirement failed; owner retains native cleanup");
                *self.retirement_error.borrow_mut() = Some(error);
            }
        } else {
            let mut pending = self.retired.borrow_mut();
            if pending.len() < MAX_DEFERRED_RETIREMENTS {
                pending.push(id.clone());
            } else {
                let error = "GXM deferred texture retirement limit exceeded".to_string();
                tracing::error!(%error);
                *self.retirement_error.borrow_mut() = Some(error);
            }
        }
    }
}

struct NativeTexture {
    device: Rc<Device>,
    id: RefCell<Option<TextureId>>,
    size: [u32; 2],
    bpp: u32,
    srgb: bool,
    render_target: bool,
    deferred: Option<DeferredSource>,
    last_used: Cell<u64>,
    content_version: Cell<u64>,
    coverage: Cell<coverage::Stamp>,
    rebuild_us: Cell<u64>,
}

pub type DecodeDeferred = fn(&[u8]) -> Result<(u32, u32, Vec<u8>, bool)>;
pub type DecodeImageReload = Rc<dyn Fn(&[u8]) -> Result<(u32, u32, Vec<u8>, bool)>>;
pub type DecodeOwnedPixels = Rc<dyn Fn(&[u8]) -> Result<(u32, u32, crate::PixelOwner, bool)>>;
pub type CachedImageReload = Rc<dyn Fn() -> Option<(u32, u32, crate::PixelOwner, bool)>>;

/// Borrow a complete GXT from its archive envelope without decoding pixels.
/// Layout dimensions belong to KIMG; physical dimensions belong to both KIMG
/// and GXT. Reloads must agree with the descriptor checked at registration.
pub struct DeferredGxt<'a> {
    pub logical: [u32; 2],
    pub physical: [u32; 2],
    pub file: &'a [u8],
}

pub type ExtractDeferredGxt = for<'a> fn(&'a [u8]) -> Result<DeferredGxt<'a>>;

/// Immutable offline metadata tied to the selected asset manifest. This admits
/// a locator only; its first use still parses the full GXT and checks equality.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexedGxtMetadata { pub layout: u32, pub format: u32, pub payload_bytes: usize }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GxtDescriptor {
    physical: [u32; 2],
    backing: [u16; 2],
    layout: u32,
    format: GxtFormat,
    payload_bytes: usize,
}

impl GxtDescriptor {
    fn from_texture(texture: &GxtTexture<'_>) -> Self {
        Self {
            physical: [u32::from(texture.width), u32::from(texture.height)],
            backing: [texture.backing_width, texture.backing_height],
            layout: texture.texture_type,
            format: texture.format,
            payload_bytes: texture.data.len(),
        }
    }

    fn from_indexed(physical: [u32; 2], metadata: IndexedGxtMetadata) -> Result<Self> {
        ensure!(physical.iter().all(|v| *v != 0 && *v <= 2048), "indexed GXT dimensions invalid");
        let format = match metadata.format {
            0x8100_0000 => GxtFormat::Pvrtc4, 0x8500_0000 => GxtFormat::Dxt1,
            0x8700_0000 => GxtFormat::Dxt5, _ => anyhow::bail!("indexed GXT format unsupported"),
        };
        ensure!(matches!(metadata.layout, 0 | 0xa000_0000), "indexed GXT layout unsupported");
        ensure!(metadata.layout != 0 || physical.iter().all(|v| v.is_power_of_two()),
            "indexed GXT linear dimensions must be powers of two");
        let minimum = if format == GxtFormat::Pvrtc4 { 8 } else { 4 };
        let backing = physical.map(|v| v.next_power_of_two().max(minimum));
        let pixels = backing[0] as usize * backing[1] as usize;
        let expected = if format == GxtFormat::Dxt5 { pixels } else { pixels / 2 };
        ensure!(metadata.payload_bytes == expected, "indexed GXT block length invalid");
        Ok(Self { physical, backing: backing.map(|v| v as u16), layout: metadata.layout,
            format, payload_bytes: expected })
    }

    fn allocation_bytes(self) -> Result<usize> {
        Ok(Gxm::texture_charge(u32::try_from(self.payload_bytes)
            .context("compressed texture exceeds native address space")?).map_err(anyhow::Error::msg)? as usize)
    }
}

/// Immutable encoded storage with explicit C/Rust ownership and no Vec-to-Arc slice copy.
// Shared atlas views are loaded only for submitted textures; the cache owns the whole block.
enum DeferredBytes {
    Retained(Arc<[u8]>),
    Shared {
        encoded_len: usize,
        load: Rc<dyn Fn() -> Result<crate::ByteView>>,
    },
    Ready {
        ready: RefCell<Option<crate::ByteView>>,
        encoded_len: usize,
        load: Rc<dyn Fn() -> Result<Vec<u8>>>,
    },
    Reload {
        encoded_len: usize,
        load: Rc<dyn Fn() -> Result<Vec<u8>>>,
    },
}

impl DeferredBytes {
    fn encoded_len(&self) -> usize {
        match self {
            Self::Retained(bytes) => bytes.len(),
            Self::Reload { encoded_len, .. } | Self::Ready { encoded_len, .. } | Self::Shared { encoded_len, .. } => *encoded_len,
        }
    }

    fn retained_len(&self) -> usize {
        match self {
            Self::Retained(bytes) => bytes.len(),
            Self::Ready { ready, .. } => ready.borrow().as_ref().map_or(0, crate::ByteView::allocation_len),
            Self::Reload { .. } | Self::Shared { .. } => 0,
        }
    }

    fn release_ready(&self) -> usize {
        match self {
            Self::Ready { ready, .. } => ready.borrow_mut().take().map_or(0, |view| view.allocation_len()),
            _ => 0,
        }
    }

    fn with_bytes<T>(&self, metric: Metric, label: &str, f: impl FnOnce(&[u8]) -> Result<T>) -> Result<T> {
        match self {
            Self::Retained(bytes) => f(bytes),
            Self::Shared { encoded_len, load } => {
                let bytes = load().context("loading shared atlas view")?;
                ensure!(bytes.len() == *encoded_len, "shared atlas range changed");
                f(&bytes)
            },
            Self::Ready { ready, encoded_len, load } => {
                // Keep the first worker payload through retries; release it only after commit.
                if let Some(bytes) = ready.borrow().as_ref() { return f(bytes); }
                let reload = Scope::new(metric);
                let bytes = load().context("reloading evicted GXT bytes")?;
                ensure!(bytes.len() == *encoded_len && bytes.len() <= DEFERRED_SOURCE_BYTES,
                    "deferred ready reload byte length changed");
                reload.finish_hot(label, bytes.len() as u64, 0);
                f(&bytes)
            },
            Self::Reload { encoded_len, load } => {
                let reload = Scope::new(metric);
                let bytes = load().context("reloading deferred image bytes")?;
                ensure!(bytes.len() == *encoded_len && bytes.len() <= DEFERRED_SOURCE_BYTES,
                    "deferred reload byte length changed: {} != {encoded_len}", bytes.len());
                reload.finish_hot(label, bytes.len() as u64, 0);
                // Keep this bounded temporary alive through parsing and the
                // synchronous native block copy; never retain it in the cache.
                f(&bytes)
            }
        }
    }
}

struct DeferredSource {
    bytes: DeferredBytes,
    label: String,
    kind: DeferredKind,
}

enum DeferredKind {
    Alter { decode: DecodeOwnedPixels },
    Image { decode: DecodeOwnedPixels, cached: Option<CachedImageReload> },
    GxtAltered { logical: [u32; 2], info: GxtDescriptor, extract: ExtractDeferredGxt },
}

impl NativeTexture {
    fn changed(&self) {
        let version = self.content_version.get().wrapping_add(1);
        self.content_version.set(version);
        self.coverage.set(coverage::Stamp::unknown(version));
    }
    fn coverage_for(&self, encoder: u64) -> CoverageBound {
        let stamp = self.coverage.get();
        if stamp.version == self.content_version.get() && (stamp.pending == 0 || stamp.pending == encoder) {
            stamp.bound
        } else { CoverageBound::Unknown }
    }
    fn predict_coverage(&self, encoder: u64, bound: CoverageBound) {
        self.changed();
        self.coverage.set(coverage::Stamp { version: self.content_version.get(), pending: encoder, bound });
    }
    fn resident_id(&self) -> Result<TextureId> {
        self.id.borrow().clone().context("texture is not resident in GXM")
    }

    fn allocation_bytes(&self) -> Result<usize> {
        match self.deferred.as_ref().map(|source| &source.kind) {
            Some(DeferredKind::GxtAltered { info, .. }) => info.allocation_bytes(),
            _ => native_allocation_bytes(self.size, self.bpp),
        }
    }

    fn commit_residency(&self, id: TextureId) -> Result<()> {
        let committed = (|| {
            ensure!(self.deferred.is_some() && self.id.borrow().is_none(),
                "invalid deferred texture residency transition");
            self.device.resident_deferred_bytes.get().checked_add(self.allocation_bytes()?)
                .context("deferred resident accounting overflow")
        })();
        match committed {
            Ok(total) => {
                *self.id.borrow_mut() = Some(id);
                self.device.resident_deferred_bytes.set(total);
                knife_perf::gauge_set(Gauge::ResidentBytes, total as u64);
                Ok(())
            }
            Err(error) => {
                self.device.retire(&id);
                Err(error)
            }
        }
    }

    fn release_resident_charge(&self) -> Result<()> {
        let total = self.device.resident_deferred_bytes.get()
            .checked_sub(self.allocation_bytes()?).context("deferred resident accounting underflow")?;
        self.device.resident_deferred_bytes.set(total);
        knife_perf::gauge_set(Gauge::ResidentBytes, total as u64);
        Ok(())
    }
}

impl Drop for NativeTexture {
    fn drop(&mut self) {
        #[cfg(target_os = "vita")]
        menu_draw_diagnostic::texture("DROP", self, "");
        if let Some(id) = self.id.get_mut().take() {
            self.device.retire(&id);
            if self.deferred.is_some() && self.device.retirement_error.borrow().is_none() {
                // A queued destruction drains before the next native query.
                // Failed retirement instead poisons admission and keeps its charge.
                if let Err(error) = self.release_resident_charge() {
                    tracing::error!(%error, "GXM deferred resident accounting failed");
                    *self.device.retirement_error.borrow_mut() = Some(error.to_string());
                }
            }
        }
        if let Some(source) = &self.deferred {
            self.device.encoded_bytes.set(self.device.encoded_bytes.get() - source.bytes.retained_len());
            knife_perf::gauge_set(Gauge::EncodedBytes, self.device.encoded_bytes.get() as u64);
        }
    }
}

pub struct GpuTexture {
    native: Rc<NativeTexture>,
    pub width: u32,
    pub height: u32,
    sparse_coverage: Option<SparseCoveragePlan>,
}

pub type TexRef = Arc<GpuTexture>;

/// A coverage plan belongs to exact immutable asset bytes, admitted by Assets.
/// It changes only source geometry, never the texture allocation or UV domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SparseCoveragePlan { Img0436 }

impl GpuTexture {
    pub fn with_sparse_coverage(mut tex: TexRef, plan: SparseCoveragePlan) -> TexRef {
        Arc::get_mut(&mut tex).expect("set sparse coverage before sharing an uploaded texture")
            .sparse_coverage = Some(plan);
        tex
    }
    /// Identity plus pixel mutations; residency reloads of immutable bytes do not change it.
    pub fn render_identity(&self) -> (usize, u64) {
        (Rc::as_ptr(&self.native) as usize, self.native.content_version.get())
    }
    /// Deferred image bytes are immutable; live targets and writable font/UI
    /// pages cannot be inputs to an independently retained stage prefix.
    pub fn vita_immutable_stage_source(&self) -> bool {
        !self.native.render_target && self.native.deferred.is_some()
    }
    #[cfg(target_os = "vita")]
    pub fn vita_diagnostic_ownership(&self) -> String {
        format!("native={:p} id={:?} physical={:?} native_refs={} deferred={}",
            &*self.native, self.native.id.borrow(), self.native.size,
            Rc::strong_count(&self.native), self.native.deferred.is_some())
    }
    pub fn physical_size(&self) -> [u32; 2] {
        self.native.size
    }
    // Both linear and arbitrary-swizzled GXM descriptors use physical image
    // dimensions. Their pitch/backing padding never rescales logical UVs.
    pub fn uv_max(&self) -> [f32; 2] {
        [1.0, 1.0]
    }
    pub fn uv_extent(&self) -> [f32; 2] {
        [self.width as f32, self.height as f32]
    }
    pub fn with_logical_size(mut tex: TexRef, [width, height]: [u32; 2]) -> TexRef {
        assert!(
            width != 0 && height != 0,
            "logical texture dimensions must be nonzero"
        );
        let unique =
            Arc::get_mut(&mut tex).expect("set layout size before sharing an uploaded texture");
        unique.width = width;
        unique.height = height;
        tex
    }
}

/// Creator-thread handle for completing any submitted movie-frame sampling.
#[derive(Clone)]
pub struct MovieGpuFence { device: Rc<Device> }
#[cfg(target_os = "vita")]
impl MovieGpuFence {
    pub fn finish(&self) -> Result<()> { self.device.call(|g| g.finish_movie_sampling()) }
}

pub struct RenderTarget {
    pub tex: TexRef,
}

/// Conservative physical-pixel coverage supplied by a group producer.
///
/// A bound is used only for the final group composite draw. Producer clears,
/// producer draws, and backdrop copies retain their full-canvas behavior. A
/// rectangle is interpreted as `[x, y, width, height]` in the target's
/// physical pixel space, with `(0, 0)` at the top-left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverageBound {
    Empty,
    Rect { x: u32, y: u32, width: u32, height: u32 },
    Full,
    Unknown,
}

impl CoverageBound {
    pub const fn rect(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self::Rect { x, y, width, height }
    }
}

// The owner and its encoders share CPU-only scratch storage. Native submit
// copies these vertices before returning; this never aliases a GXM arena.
#[derive(Default)]
struct VertexBuffers { buffers: Vec<Vec<Vertex>>, bytes: usize }
impl VertexBuffers {
    fn pop(&mut self) -> Option<Vec<Vertex>> {
        let buffer = self.buffers.pop()?;
        self.bytes -= buffer.capacity() * std::mem::size_of::<Vertex>();
        knife_perf::add(Metric::VertexBufferReuse, 0, 1);
        Some(buffer)
    }
    fn push(&mut self, mut buffer: Vec<Vertex>) {
        let bytes = buffer.capacity().saturating_mul(std::mem::size_of::<Vertex>());
        if bytes == 0 || bytes > MAX_FRAME_VERTEX_BYTES.saturating_sub(self.bytes) { return; }
        self.bytes += bytes;
        buffer.clear();
        self.buffers.push(buffer);
        knife_perf::add(Metric::VertexBufferRecycle, 0, 1);
    }
}

/// A pending frame. Dropping an unsubmitted encoder discards its commands.
/// Texture references stay alive until all commands have returned from GXM.
pub struct CommandEncoder {
    device: Rc<Device>,
    vertex_reuse: Rc<RefCell<VertexBuffers>>,
    commands: Vec<Command>,
    vertex_bytes: usize,
    /// Target whose render pass is still open at the end of `commands`. Consecutive
    /// draws into the same target append to it instead of opening another pass.
    open: Option<Rc<NativeTexture>>,
    coverage_id: u64,
    coverage_open: Option<Rc<NativeTexture>>,
}

impl Drop for CommandEncoder {
    fn drop(&mut self) { coverage::finish(&self.commands, self.coverage_id, false); }
}

/// Simulates a minimal barrier scheme: a pass needs a Finish only when it samples a
/// target that an earlier pass in this frame wrote. Returns (passes, passes_needing).
#[cfg(target_os = "vita")]
fn analyse_passes(commands: &[Command]) -> (u64, u64) {
    let flags = pass_finish_flags(commands);
    (flags.len() as u64, flags.iter().filter(|f| **f).count() as u64)
}

/// One flag per pass, in submission order: does this pass sample a target that an earlier
/// pass in the same frame wrote? A Finish is only needed for those.
fn pass_finish_flags(commands: &[Command]) -> Vec<bool> {
    // RAW: a pass samples a target an earlier pass wrote.
    let mut written: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // WAR: a pass renders into a target an earlier in-flight pass is sampling.
    let mut sampled: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut flags = Vec::new();
    let mut i = 0;
    while i < commands.len() {
        let Command::Begin { target, .. } = &commands[i] else { i += 1; continue };
        let target_ptr = Rc::as_ptr(target) as usize;
        let mut j = i + 1;
        let mut need = sampled.contains(&target_ptr);
        while j < commands.len() {
            match &commands[j] {
                Command::End => break,
                Command::Draw { textures, .. } => {
                    for t in textures.iter().flatten() {
                        let ptr = Rc::as_ptr(t) as usize;
                        if written.contains(&ptr) { need = true; }
                    }
                }
                Command::Copy { src, .. } => {
                    if written.contains(&(Rc::as_ptr(src) as usize)) { need = true; }
                }
                _ => {}
            }
            j += 1;
        }
        if need {
            // A Finish drains everything submitted so far.
            written.clear();
            sampled.clear();
        }
        written.insert(target_ptr);
        for k in (i + 1)..j {
            if let Command::Draw { textures, .. } = &commands[k] {
                for t in textures.iter().flatten() {
                    sampled.insert(Rc::as_ptr(t) as usize);
                }
            } else if let Command::Copy { src, .. } = &commands[k] {
                sampled.insert(Rc::as_ptr(src) as usize);
            }
        }
        flags.push(need);
        i = j;
    }
    flags
}

impl CommandEncoder {
    /// Close an open pass. Every command that is not a draw into the same target
    /// must call this first: GXM copies and passes cannot interleave.
    fn close_pass(&mut self) {
        if self.open.take().is_some() {
            self.commands.push(Command::End);
            self.coverage_open = None;
        }
    }
}

#[path = "vita_blend_chain.rs"]
mod blend_chain;
#[path = "vita_sparse_coverage.rs"]
mod sparse_coverage;

// Uniform inputs must agree only where the selected shader actually reads a
// shared uniform. RimText28 reads hx/steps per glyph and keeps only RGB24 shared.
fn draw_uniforms_match(shader: EffectShader, a: &[Vertex], b: &[Vertex]) -> bool {
    match shader {
        EffectShader::RimTextBatched =>
            a.first().map(|v|v.haze[1][3]) == b.first().map(|v|v.haze[1][3]),
        EffectShader::PsChain2 =>
            a.first().map(|v|v.haze[1]) == b.first().map(|v|v.haze[1]),
        _ => a.first().map(|v|v.haze) == b.first().map(|v|v.haze),
    }
}

#[path = "vita_executed_work.rs"]
mod executed_work;
#[path = "vita_coverage.rs"]
mod coverage;

/// Successful native draw totals, sampled at the same boundary as FrameProbe.
pub fn executed_work_snapshot() -> (u64,u64) { executed_work::snapshot() }

enum Command {
    Begin {
        target: Rc<NativeTexture>,
        clear: Option<[f32; 4]>,
    },
    End,
    /// CPU-only dependency proof; prevents cropped composites from coalescing.
    GroupCut(coverage::GroupCut),
    Draw {
        work: executed_work::DrawWork,
        vertices: Vec<Vertex>,
        shader: EffectShader,
        blend: EffectBlend,
        textures: [Option<Rc<NativeTexture>>; 4],
        params: [f32; 8],
    },
    Copy {
        metric: Metric,
        src: Rc<NativeTexture>,
        dst: Rc<NativeTexture>,
    },
}

impl CommandEncoder {
    fn append<C>(&mut self, commands: C) -> Result<()>
    where C: AsRef<[Command]> + IntoIterator<Item = Command> {
        let _record = Scope::new(Metric::CommandAppend);
        let validation = (|| -> Result<usize> {
        let command_count = self
            .commands
            .len()
            .checked_add(commands.as_ref().len())
            .context("frame command count overflow")?;
        ensure!(
            command_count <= MAX_FRAME_COMMANDS,
            "frame exceeds {MAX_FRAME_COMMANDS} GXM commands"
        );
        let mut vertex_bytes = self.vertex_bytes;
        for command in commands.as_ref() {
            if let Command::Draw { vertices, .. } = command {
                vertex_bytes = vertex_bytes
                    .checked_add(
                        vertices
                            .len()
                            .checked_mul(std::mem::size_of::<Vertex>())
                            .context("vertex buffer size overflow")?,
                    )
                    .context("frame vertex size overflow")?;
            }
        }
        ensure!(
            vertex_bytes <= MAX_FRAME_VERTEX_BYTES,
            "frame exceeds {MAX_FRAME_VERTEX_BYTES} recorded vertex bytes"
        );
        self.commands
            .try_reserve(commands.as_ref().len())
            .context("reserving frame command memory")?;
        Ok(vertex_bytes)
        })();
        let vertex_bytes = match validation {
            Ok(bytes) => bytes,
            Err(error) => {
                coverage::finish(&self.commands, self.coverage_id, false);
                coverage::invalidate(commands.as_ref());
                if let Some(target) = &self.coverage_open { target.changed(); }
                return Err(error);
            }
        };
        let mut work_target = self.coverage_open.clone();
        self.coverage_open = coverage::record(commands.as_ref(), self.coverage_id, self.coverage_open.take());
        // Coalesce a run of draws that share every bound state into one submission, the
        // way the reference Vita renderer batches quads: the driver cost is per draw, and
        // a heavy frame issues 60-95 of them.
        for mut command in commands {
            match &mut command {
                Command::Begin { target, .. } => work_target = Some(target.clone()),
                Command::End => work_target = None,
                Command::Draw { work, vertices, shader, blend, params, .. } => {
                    let size = work_target.as_ref().map_or([0;2], |target|target.size);
                    let role = work_target.as_ref().map_or(0, |target|scene_kind(target));
                    *work = executed_work::DrawWork::prepare(*shader,*blend,*params,vertices,size,role);
                }
                _ => {},
            }
            if knife_perf::enabled() {
                if let Command::Draw { vertices, shader, blend, textures, params, .. } = &command {
                    let reason = match self.commands.last() {
                        Some(Command::Draw { vertices: last, shader: a, blend: b, textures: t, params: p, .. }) => {
                            if a != shader { Some(Metric::MergeShader) }
                            else if b != blend { Some(Metric::MergeBlend) }
                            else if p != params { Some(Metric::MergeParams) }
                            else if !draw_uniforms_match(*shader,last,vertices) { Some(Metric::MergeHaze) }
                            else if last.len() + vertices.len() > MERGED_DRAW_VERTICES { Some(Metric::MergeCapacity) }
                            else if !t.iter().zip(textures).all(|(a,b)| match (a,b) {
                                (Some(a),Some(b)) => Rc::ptr_eq(a,b), (None,None) => true, _ => false,
                            }) { Some(Metric::MergeTextures) } else { None }
                        },
                        _ => Some(Metric::MergeBoundary),
                    };
                    if let Some(reason) = reason { knife_perf::add(reason,0,0); }
                }
            }
            if let Command::Draw { work, vertices, shader, blend, textures, params } = &mut command
                && let Some(Command::Draw {
                    vertices: last_vertices,
                    shader: last_shader,
                    blend: last_blend,
                    textures: last_textures,
                    params: last_params,
                    work: last_work,
                }) = self.commands.last_mut()
                && *last_shader == *shader
                && *last_blend == *blend
                && last_params == params
                // The shader reads haze of the first vertex as a uniform, so a merged run
                // must not cross a haze change.
                && draw_uniforms_match(*shader,last_vertices,vertices)
                && last_vertices.len() + vertices.len() <= MERGED_DRAW_VERTICES
                && last_textures.iter().zip(textures.iter()).all(|(a, b)| match (a, b) {
                    (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                })
            {
                let copy = Scope::new(Metric::CommandMerge);
                if *shader == EffectShader::RimTextBatched { knife_perf::add(Metric::RimBatchMerge,0,1); }
                last_vertices.extend_from_slice(vertices);
                last_work.merge(*work);
                copy.finish((vertices.len() * std::mem::size_of::<Vertex>()) as u64, 0);
                if knife_perf::optimizations::enabled(knife_perf::optimizations::COMPACT_STREAM) {
                    self.vertex_reuse.borrow_mut().push(std::mem::take(vertices));
                }
                continue;
            }
            self.commands.push(command);
        }
        self.vertex_bytes = vertex_bytes;
        Ok(())
    }
}

#[derive(Clone)]
pub struct Quad {
    pub tex: TexRef,
    pub pos: [[f32; 2]; 4],
    pub uv: [[f32; 2]; 4],
    pub color: [f32; 4],
    pub blend: Blend,
    pub shade: Shade,
    pub clip: Option<[[f32; 2]; 4]>,
    pub rule: Option<RuleMask>,
    pub haze: [[f32; 4]; 2],
    pub soft: Option<SoftClip>,
    /// Draw this quad multiplied by the layer-group mask carried in `rule` instead of
    /// compositing a mask surface afterwards.
    pub masked: bool,
}

impl Quad {
    /// Exact cache input, with float representations and source generations.
    /// Unlike a digest this can be compared without collision assumptions.
    /// The cache must also keep a live/weak source owner to reject pointer reuse.
    pub fn render_key(&self) -> Vec<u64> {
        let texture = |tex: &GpuTexture, out: &mut Vec<u64>| {
            let (id, generation) = tex.render_identity();
            out.extend([id as u64, generation, u64::from(tex.width), u64::from(tex.height)]);
            out.extend(tex.physical_size().map(u64::from));
        };
        let mut out = Vec::with_capacity(80);
        texture(&self.tex, &mut out);
        let (blend, op) = match self.blend {
            Blend::Alpha => (0, 0), Blend::Add => (1, 0), Blend::Sub => (2, 0),
            Blend::Multiply => (3, 0), Blend::Screen => (4, 0), Blend::Opaque => (5, 0),
            Blend::Ps(op) => (6, op as u64), Blend::Over => (7, 0),
            Blend::Footprint => (8, 0), Blend::MaskAlpha => (9, 0),
        };
        out.extend([blend, op, self.shade as u64, u64::from(self.masked)]);
        let floats = |values: &[f32], out: &mut Vec<u64>| {
            out.extend(values.iter().map(|v| u64::from(v.to_bits())));
        };
        for p in self.pos { floats(&p, &mut out); }
        for p in self.uv { floats(&p, &mut out); }
        floats(&self.color, &mut out);
        for p in self.haze { floats(&p, &mut out); }
        out.push(u64::from(self.clip.is_some()));
        if let Some(clip) = self.clip { for p in clip { floats(&p, &mut out); } }
        out.push(u64::from(self.soft.is_some()));
        if let Some(m) = self.soft {
            floats(&m.inner, &mut out); floats(&m.pivot, &mut out);
            floats(&[m.rot, m.radius, m.fill], &mut out);
            out.extend([u64::from(m.row), u64::from(m.texels)]);
        }
        out.push(u64::from(self.rule.is_some()));
        if let Some(rule) = &self.rule {
            texture(&rule.tex, &mut out);
            floats(&[rule.progress, rule.soft], &mut out);
            out.extend([u64::from(rule.flip), u64::from(rule.invert)]);
        }
        out
    }

    pub fn render_signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.tex.render_identity().hash(&mut h);
        [self.tex.width, self.tex.height].hash(&mut h);
        self.tex.physical_size().hash(&mut h);
        self.blend.hash(&mut h); self.shade.hash(&mut h); self.masked.hash(&mut h);
        let floats = |values: &[f32], h: &mut std::collections::hash_map::DefaultHasher| {
            for value in values { value.to_bits().hash(h); }
        };
        for p in self.pos { floats(&p, &mut h); }
        for p in self.uv { floats(&p, &mut h); }
        floats(&self.color, &mut h);
        for p in self.haze { floats(&p, &mut h); }
        self.clip.is_some().hash(&mut h);
        if let Some(clip) = self.clip { for p in clip { floats(&p, &mut h); } }
        self.soft.is_some().hash(&mut h);
        if let Some(m) = self.soft {
            floats(&m.inner, &mut h); floats(&m.pivot, &mut h);
            floats(&[m.rot,m.radius,m.fill], &mut h); m.row.hash(&mut h); m.texels.hash(&mut h);
        }
        self.rule.is_some().hash(&mut h);
        if let Some(rule) = &self.rule {
            rule.tex.render_identity().hash(&mut h);
            [rule.tex.width,rule.tex.height].hash(&mut h); rule.tex.physical_size().hash(&mut h);
            floats(&[rule.progress,rule.soft], &mut h); rule.flip.hash(&mut h); rule.invert.hash(&mut h);
        }
        h.finish()
    }
    pub fn scaled(mut self, s: f32) -> Self {
        let sc = |p: &mut [f32; 2]| *p = [p[0] * s, p[1] * s];
        self.pos.iter_mut().for_each(sc);
        if let Some(c) = &mut self.clip {
            c.iter_mut().for_each(sc);
        }
        if let Some(m) = &mut self.soft {
            m.inner = m.inner.map(|v| v * s);
            sc(&mut m.pivot);
            m.radius *= s;
        }
        self
    }

    pub fn rect(tex: TexRef, [x, y, w, h]: [f32; 4], color: [f32; 4]) -> Self {
        Self {
            tex,
            pos: [[x, y], [x + w, y], [x + w, y + h], [x, y + h]],
            uv: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            color,
            blend: Blend::Alpha,
            shade: Shade::Sprite,
            clip: None,
            rule: None,
            haze: [[0.0; 4]; 2],
            soft: None,
            masked: false,
        }
    }

    pub fn with_uv_rect(mut self, u0: f32, v0: f32, u1: f32, v1: f32) -> Self {
        self.uv = [[u0, v0], [u1, v0], [u1, v1], [u0, v1]];
        self
    }
}

pub enum Upload<'a> {
    Rgba8(&'a [u8]),
    Bc7(&'a [u8]),
    Bc1(&'a [u8]),
    Bc3(&'a [u8]),
    R8(&'a [u8]),
}

pub struct Renderer {
    device: Rc<Device>,
    backdrops: HashMap<[u32; 2], Rc<NativeTexture>>,
    command_reuse: RefCell<Vec<Command>>,
    vertex_reuse: Rc<RefCell<VertexBuffers>>,
    soft_lut: Rc<NativeTexture>,
    pub bc_supported: bool,
    pub white: TexRef,
    pub hidden: bool,
}

impl Renderer {
    /// Bounded executed-draw summary. The counters are updated only after a
    /// successful native draw and are intentionally cumulative between log
    /// windows, matching the existing audit-bank policy.
    pub fn vita_work_stats_json(&self) -> String { executed_work::json() }

    /// Read current native mappings without flushing commands or retiring textures.
    /// These counters do not include Rust/newlib heap usage or total process RAM.
    #[cfg(target_os = "vita")]
    pub fn vita_performance_stats(&self) -> Result<knife_platform_vita::gxm::GxmStats> {
        self.device.gxm.try_borrow().map_err(|_| anyhow!("GXM stats owner is busy"))?
            .stats().map_err(anyhow::Error::msg)
    }

    /// Number of shared PS backdrop targets currently allocated (tests only).
    #[cfg(test)]
    pub(crate) fn backdrop_count(&self) -> usize { self.backdrops.len() }

    /// (exec_us, present_us, frames) accumulated by the submission path.
    #[cfg(target_os = "vita")]
    pub fn vita_frame_breakdown(&self) -> (u64, u64, u64, u64, u64, u64) {
        frame_metrics::snapshot()
    }

    /// Top render passes by cumulative blocked time, as "label:ms:passes".
    #[cfg(target_os = "vita")]
    pub fn vita_pass_summary(&self, top: usize) -> String {
        pass_metrics::snapshot(top)
    }

    /// Deferred atlas work: items, total per-item time, the GPU create call, and
    /// the RGBA bake pass. GXT atlas uploads only register a handle here, so the
    /// per-item total is the honest end-to-end number for them.
    #[cfg(target_os = "vita")]
    pub fn vita_deferred_timing(&self) -> (u64, u64, u64, u64) {
        deferred_metrics::snapshot()
    }

    /// Cumulative GPU-sync counters. Every value is the CPU side of a wait, so it
    /// bounds but never equals GPU execution time.
    #[cfg(target_os = "vita")]
    pub fn vita_gpu_timing(&self) -> Result<knife_platform_vita::gxm::GxmTiming> {
        self.device.gxm.try_borrow().map_err(|_| anyhow!("GXM owner is busy"))?
            .timing().map_err(anyhow::Error::msg)
    }

    /// Vita owns a fixed 960x544 display; scene targets keep 1920x1080 script units.
    pub fn new() -> Result<Self> {
        Self::new_with_gxm(Gxm::new().map_err(anyhow::Error::msg)?)
    }

    pub fn movie_gpu_fence(&self) -> MovieGpuFence {
        MovieGpuFence { device: self.device.clone() }
    }

    /// Convert one newly leased native YVU surface into an owned UNORM target.
    /// No display/present or CPU pixel copy occurs. Native sampling is completed
    /// before returning, allowing the decoder owner to reclaim this lease while
    /// subsequent app frames keep sampling the same stable RGBA cache.
    #[cfg(target_os = "vita")]
    pub fn try_cache_movie_frame(&self, source: &TexRef, target: &RenderTarget, uv: [f32; 4]) -> Result<()> {
        let probe = Scope::new(Metric::MovieFrameCache);
        self.check_texture(source)?; self.check_target(target)?;
        ensure!(source.native.bpp == 0 && target.tex.physical_size() == [960, 540]
            && !Rc::ptr_eq(&source.native, &target.tex.native), "invalid movie cache source/target");
        ensure!(uv.iter().all(|v| v.is_finite()) && uv[0] >= 0.0 && uv[1] >= 0.0
            && uv[2] <= 1.0 && uv[3] <= 1.0 && uv[0] < uv[2] && uv[1] < uv[3], "invalid movie UV window");
        let quad = Quad::rect(source.clone(), [0.0, 0.0, 960.0, 540.0], [1.0; 4])
            .with_uv_rect(uv[0], uv[1], uv[2], uv[3]);
        let reuse = self.vertex_reuse.borrow_mut().pop().unwrap_or_default();
        let vertices = quad_vertices_reusing(&quad, [960.0, 540.0], [1.0; 2], reuse)?;
        let work = executed_work::DrawWork::prepare(EffectShader::Sprite,EffectBlend::Opaque,
            [0.0;8],&vertices,target.tex.native.size,scene_kind(&target.tex.native));
        let mut commands = [Command::Begin { target: target.tex.native.clone(), clear: None },
            Command::Draw { work, vertices, shader: EffectShader::Sprite, blend: EffectBlend::Opaque,
                textures: [Some(source.native.clone()), Some(self.white.native.clone()), None, Some(self.soft_lut.clone())],
                params: [0.0; 8] }, Command::End];
        let submitted = execute(&self.device, &commands);
        // Even a partial/failed submit must be settled before its native source
        // can be acknowledged. The caller quarantines the lease if this fails.
        let settled = self.device.call(|g| g.finish_movie_sampling());
        if let Command::Draw { vertices, .. } = &mut commands[1] {
            self.vertex_reuse.borrow_mut().push(std::mem::take(vertices));
        }
        settled?; submitted?;
        target.tex.native.changed();
        probe.finish(960 * 540 * 4, 1);
        Ok(())
    }

    /// # Safety
    /// Pointer must be a mapped AvPlayer YVU420P2_CSC1 frame whose allocation stays
    /// alive until finish_movie_sampling completes after the draw using it.
    #[cfg(target_os = "vita")]
    pub unsafe fn try_movie_texture(&self, frame: *const u8, width: u32, height: u32) -> Result<TexRef> {
        let id = self.device.call(|g| unsafe { g.create_movie_texture(frame, width, height) })?;
        Ok(Arc::new(GpuTexture {
            native: Rc::new(NativeTexture {
                device: self.device.clone(), id: RefCell::new(Some(id)),
                size: [width, height], bpp: 0, srgb: false, render_target: false,
                deferred: None, last_used: Cell::new(0), content_version: Cell::new(0),
                coverage: Cell::new(coverage::Stamp::unknown(0)), rebuild_us: Cell::new(0),
            }),
            width, height, sparse_coverage: None,
        }))
    }

    /// # Safety
    /// Caller guarantees the GPU-mapped frame allocation outlives the last
    /// submitted sampling command; this call finishes prior commands first.
    #[cfg(target_os = "vita")]
    pub unsafe fn try_movie_rebind(&self, tex: &TexRef, frame: *const u8, width: u32, height: u32) -> Result<()> {
        self.check_texture(tex)?;
        ensure!(tex.native.bpp == 0 && tex.native.deferred.is_none()
            && tex.physical_size() == [width, height], "movie descriptor layout mismatch");
        let id = tex.native.resident_id()?;
        self.device.call(|g| unsafe { g.rebind_movie_texture(&id, frame, width, height) })?;
        tex.native.changed();
        Ok(())
    }

    /// Consume an initialized native owner, preserving its creating-thread lifetime.
    pub fn new_with_gxm(gxm: Gxm) -> Result<Self> {
        let device = Rc::new(Device {
            gxm: RefCell::new(gxm),
            retired: RefCell::new(Vec::new()),
            retirement_error: RefCell::new(None),
            deferred: RefCell::new(Vec::new()),
            deferred_budget: Cell::new(DEFERRED_TEXTURE_BYTES),
            resident_deferred_bytes: Cell::new(0),
            encoded_bytes: Cell::new(0),
            epoch: Cell::new(0),
            #[cfg(test)]
            admission_scans: Cell::new(0),
            #[cfg(test)]
            preparation_sorts: Cell::new(0),
        });
        let white = make_texture(
            &device,
            [1, 1],
            [1, 1],
            Some(&[255; 4]),
            TextureFormat::Rgba8,
            false,
            true,
            false,
        )
        .context("creating white GXM texture")?;
        let soft_lut = make_texture(
            &device,
            [SOFT_LUT_W, SOFT_LUT_ROWS],
            [SOFT_LUT_W, SOFT_LUT_ROWS],
            None,
            TextureFormat::R8,
            false,
            false,
            false,
        )
        .context("creating soft clip LUT")?
        .native
        .clone();
        Ok(Self {
            device,
            backdrops: HashMap::new(),
            command_reuse: RefCell::new(Vec::new()),
            vertex_reuse: Rc::new(RefCell::new(VertexBuffers::default())),
            soft_lut,
            bc_supported: false,
            white,
            hidden: false,
        })
    }

    /// The handheld display is fixed; this retains the desktop window callback API.
    pub fn resize(&mut self, _width: u32, _height: u32) {}

    pub fn set_soft_lut(&self, rows: &[Vec<u8>]) {
        self.try_set_soft_lut(rows)
            .expect("updating Vita soft clip LUT");
    }

    pub fn try_set_soft_lut(&self, rows: &[Vec<u8>]) -> Result<()> {
        let mut data = vec![0; (SOFT_LUT_W * SOFT_LUT_ROWS) as usize];
        for (i, row) in rows.iter().take(SOFT_LUT_ROWS as usize).enumerate() {
            let n = row.len().min(SOFT_LUT_W as usize);
            data[i * SOFT_LUT_W as usize..][..n].copy_from_slice(&row[..n]);
        }
        let id = self.soft_lut.resident_id()?;
        self.device.call(|g| {
            g.update_texture(
                &id,
                [0, 0],
                [SOFT_LUT_W, SOFT_LUT_ROWS],
                &data,
                SOFT_LUT_W,
            )
        })
    }

    pub fn upload(&self, width: u32, height: u32, data: Upload<'_>, label: &str) -> TexRef {
        self.try_upload(width, height, data, label)
            .expect("uploading Vita texture")
    }

    pub fn try_upload(
        &self,
        width: u32,
        height: u32,
        data: Upload<'_>,
        label: &str,
    ) -> Result<TexRef> {
        let tex = self.upload_raw(width, height, data, false)
            .with_context(|| format!("uploading {label}"))?;
        #[cfg(target_os = "vita")]
        menu_draw_diagnostic::texture("LABEL", &tex.native, label);
        Ok(tex)
    }

    pub fn upload_baked(&self, width: u32, height: u32, data: &[u8], label: &str) -> TexRef {
        self.try_upload_baked(width, height, data, label)
            .expect("uploading Vita baked texture")
    }

    pub fn try_upload_baked(
        &self,
        width: u32,
        height: u32,
        data: &[u8],
        label: &str,
    ) -> Result<TexRef> {
        let tex = self.upload_raw(width, height, Upload::Rgba8(data), true)
            .with_context(|| format!("uploading baked {label} ({width}x{height} RGBA8)"))?;
        #[cfg(target_os = "vita")]
        menu_draw_diagnostic::texture("LABEL", &tex.native, label);
        Ok(tex)
    }

    fn upload_raw(&self, width: u32, height: u32, data: Upload<'_>, srgb: bool) -> Result<TexRef> {
        let (bytes, format, srgb) = match data {
            Upload::Rgba8(bytes) => (bytes, TextureFormat::Rgba8, srgb),
            // Desktop R8Unorm never interprets coverage bytes as sRGB, even
            // when upload_altered receives an sRGB source hint.
            Upload::R8(bytes) => (bytes, TextureFormat::R8, false),
            Upload::Bc1(_) => bail!("BC1 uploads are unsupported on Vita; supply decoded RGBA8"),
            Upload::Bc3(_) => bail!("BC3 uploads are unsupported on Vita; supply decoded RGBA8"),
            Upload::Bc7(_) => bail!("BC7 uploads are unsupported on Vita; supply decoded RGBA8"),
        };
        make_texture(
            &self.device,
            [width, height],
            [width, height],
            Some(bytes),
            format,
            srgb,
            true,
            false,
        )
    }

    pub fn upload_altered(
        &self,
        width: u32,
        height: u32,
        data: Upload<'_>,
        srgb: bool,
        label: &str,
    ) -> TexRef {
        self.try_upload_altered(width, height, data, srgb, label)
            .expect("uploading Vita altered texture")
    }

    pub fn try_upload_altered(
        &self,
        width: u32,
        height: u32,
        data: Upload<'_>,
        srgb: bool,
        label: &str,
    ) -> Result<TexRef> {
        // Vita follows the native encoded UNORM8 contract. The desktop Alter
        // pass and its temporary render target are deliberately absent.
        let _ = srgb; // Source-file metadata does not enable hardware gamma.
        self.upload_raw(width, height, data, false)
            .with_context(|| format!("uploading native encoded texture {label}"))
    }

    /// Check the complete upload peak against measured native allowance and
    /// retire idle deferred atlases as needed. Asset caches may drop their own
    /// idle images and retry this check; active scene textures remain owned.
    /// This observes headroom, so native allocations still return failures.
    pub fn try_reserve_image_upload(
        &self,
        width: u32,
        height: u32,
        bpp: u32,
        _altered: bool,
    ) -> Result<()> {
        let allocation = native_allocation_bytes([width, height], bpp)?;
        reserve_cdram(&self.device, allocation, 0, &HashSet::new())
    }

    /// Retain bounded compressed atlas bytes; decoding, upload and Alter happen
    /// only before the first submitted frame that actually samples this image.
    pub fn defer_altered(
        &self,
        logical: [u32; 2],
        physical: [u32; 2],
        encoded: Arc<[u8]>,
        label: &str,
        decode: DecodeDeferred,
    ) -> Result<TexRef> {
        self.defer_altered_source(logical, physical, DeferredBytes::Retained(encoded), label, decode)
    }

    /// Borrow one range of a bounded shared atlas cache, without per-image copies.
    pub fn defer_altered_shared(&self, logical: [u32; 2], physical: [u32; 2],
        encoded_len: usize, label: &str, load: Rc<dyn Fn() -> Result<crate::ByteView>>,
        decode: DecodeOwnedPixels) -> Result<TexRef> {
        self.defer_source(logical, physical, DeferredBytes::Shared { encoded_len, load }, label,
            DeferredKind::Alter { decode })
    }

    /// Keep only a bounded reload descriptor. The loader runs on a submitted
    /// cache miss; recorded and abandoned encoders do not perform file I/O.
    pub fn defer_altered_reload(
        &self,
        logical: [u32; 2],
        physical: [u32; 2],
        encoded_len: usize,
        label: &str,
        load: Rc<dyn Fn() -> Result<Vec<u8>>>,
        decode: DecodeDeferred,
    ) -> Result<TexRef> {
        self.defer_altered_source(logical, physical,
            DeferredBytes::Reload { encoded_len, load }, label, decode)
    }

    /// Transfer a freshly uploaded, unpublished CG into reloadable residency.
    /// The first worker decode and GPU bake are reused. Stage/animation handles
    /// keep their identity after pressure evicts the backing native allocation.
    #[allow(clippy::too_many_arguments)]
    pub fn adopt_image_reload(
        &self,
        uploaded: TexRef,
        encoded_len: usize,
        label: &str,
        altered: bool,
        load: Rc<dyn Fn() -> Result<Vec<u8>>>,
        decode: DecodeImageReload,
    ) -> Result<TexRef> {
        self.adopt_image_reload_owned(uploaded, encoded_len, label, altered, load,
            Rc::new(move |bytes| decode(bytes).map(|(w, h, px, srgb)| (w, h, px.into(), srgb))))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn adopt_image_reload_owned(&self, uploaded: TexRef, encoded_len: usize, label: &str,
        altered: bool, load: Rc<dyn Fn() -> Result<Vec<u8>>>, decode: DecodeOwnedPixels) -> Result<TexRef> {
        self.adopt_image_reload_cached(uploaded, encoded_len, label, altered, load, decode, None)
    }

    pub fn set_image_rebuild_us(&self, texture: &TexRef, us: u64) -> Result<()> {
        ensure!(self.owns_texture(texture), "foreign rebuild cost texture");
        texture.native.rebuild_us.set(us); Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn adopt_image_reload_cached(&self, uploaded: TexRef, encoded_len: usize, label: &str,
        _altered: bool, load: Rc<dyn Fn() -> Result<Vec<u8>>>, decode: DecodeOwnedPixels,
        cached: Option<CachedImageReload>) -> Result<TexRef> {
        ensure!(self.owns_texture(&uploaded) && uploaded.native.deferred.is_none(),
            "CG adoption requires an ordinary texture from this renderer");
        ensure!(Arc::strong_count(&uploaded) == 1 && Rc::strong_count(&uploaded.native) == 1,
            "CG adoption must precede publishing the texture or recording draws");
        ensure!(uploaded.native.bpp == 4, "CG reload requires RGBA8 pixels");
        let replacement = self.defer_source([uploaded.width, uploaded.height], uploaded.native.size,
            DeferredBytes::Reload { encoded_len, load }, label,
            DeferredKind::Image { decode, cached })?;
        reserve_cdram(&self.device, 0, replacement.native.allocation_bytes()?, &HashSet::new())
            .with_context(|| format!("admitting reloadable CG {label}"))?;
        let id = uploaded.native.id.borrow_mut().take().context("CG adoption has no native allocation")?;
        replacement.native.commit_residency(id)?;
        Ok(replacement)
    }

    /// Register an immutable, offline-altered GXT. Initial bytes establish only
    /// the descriptor; a submitted cache miss reloads and uploads blocks with
    /// no pixel decoder, RGBA staging texture, or runtime Alter pass.
    #[allow(clippy::too_many_arguments)]
    pub fn defer_gxt_altered_reload(
        &self,
        logical: [u32; 2],
        physical: [u32; 2],
        encoded: &[u8],
        label: &str,
        load: Rc<dyn Fn() -> Result<Vec<u8>>>,
        extract: ExtractDeferredGxt,
    ) -> Result<TexRef> {
        let audit = Scope::new(Metric::GxtRegister);
        let view = extract(encoded).with_context(|| format!("reading GXT envelope {label}"))?;
        ensure!(view.logical == logical && view.physical == physical,
            "GXT envelope dimensions disagree for {label}");
        let gxt = GxtTexture::parse(view.file).with_context(|| format!("validating GXT {label}"))?;
        let info = GxtDescriptor::from_texture(&gxt);
        ensure!(info.physical == physical, "GXT physical dimensions disagree for {label}");
        let result = self.defer_source(logical, physical, DeferredBytes::Reload { encoded_len: encoded.len(), load },
            label, DeferredKind::GxtAltered { logical, info, extract });
        if result.is_ok() { audit.finish_hot(label, encoded.len() as u64, info.payload_bytes as u64); }
        result
    }

    /// First use consumes the worker payload; only a later eviction needs archive I/O.
    #[allow(clippy::too_many_arguments)]
    pub fn defer_gxt_ready(&self, logical: [u32; 2], physical: [u32; 2],
        encoded: crate::ByteView, label: &str, load: Rc<dyn Fn() -> Result<Vec<u8>>>,
        extract: ExtractDeferredGxt) -> Result<TexRef> {
        self.defer_gxt_source(logical, physical, &encoded, label,
            DeferredBytes::Ready { ready: RefCell::new(Some(encoded.clone())), encoded_len: encoded.len(), load }, extract)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn defer_gxt_shared(&self, logical: [u32; 2], physical: [u32; 2],
        initial: &[u8], label: &str, load: Rc<dyn Fn() -> Result<crate::ByteView>>,
        extract: ExtractDeferredGxt) -> Result<TexRef> {
        self.defer_gxt_source(logical, physical, initial, label,
            DeferredBytes::Shared { encoded_len: initial.len(), load }, extract)
    }

    /// Register checked metadata without reading any atlas payload. Immutable
    /// manifest binding is supplied by Assets; prepare still validates full data.
    #[allow(clippy::too_many_arguments)]
    pub fn defer_gxt_indexed(&self, logical: [u32; 2], physical: [u32; 2],
        encoded_len: usize, metadata: IndexedGxtMetadata, label: &str,
        load: Rc<dyn Fn() -> Result<crate::ByteView>>, extract: ExtractDeferredGxt) -> Result<TexRef> {
        let audit = Scope::new(Metric::GxtRegister);
        let info = GxtDescriptor::from_indexed(physical, metadata)?;
        ensure!(encoded_len >= 64 + info.payload_bytes.next_multiple_of(16),
            "indexed GXT envelope shorter than its blocks");
        let result = self.defer_source(logical, physical, DeferredBytes::Shared { encoded_len, load },
            label, DeferredKind::GxtAltered { logical, info, extract });
        if result.is_ok() { audit.finish_hot(label, encoded_len as u64, info.payload_bytes as u64); }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn defer_gxt_source(&self, logical: [u32; 2], physical: [u32; 2], initial: &[u8],
        label: &str, bytes: DeferredBytes, extract: ExtractDeferredGxt) -> Result<TexRef> {
        let audit = Scope::new(Metric::GxtRegister);
        let view = extract(initial).with_context(|| format!("reading GXT envelope {label}"))?;
        ensure!(view.logical == logical && view.physical == physical, "GXT envelope dimensions disagree for {label}");
        let gxt = GxtTexture::parse(view.file).with_context(|| format!("validating GXT {label}"))?;
        let info = GxtDescriptor::from_texture(&gxt);
        ensure!(info.physical == physical, "GXT physical dimensions disagree for {label}");
        // Bounded ready storage: discard only immutable CPU payloads. Their locators remain valid.
        let charge = bytes.retained_len();
        if self.device.encoded_bytes.get().saturating_add(charge) > DEFERRED_SOURCE_BYTES {
            for weak in self.device.deferred.borrow().iter() {
                if let Some(texture) = weak.upgrade() && let Some(source) = texture.deferred.as_ref() {
                    let released = source.bytes.release_ready();
                    self.device.encoded_bytes.set(self.device.encoded_bytes.get().saturating_sub(released));
                    if self.device.encoded_bytes.get().saturating_add(charge) <= DEFERRED_SOURCE_BYTES { break; }
                }
            }
            knife_perf::gauge_set(Gauge::EncodedBytes, self.device.encoded_bytes.get() as u64);
        }
        // A legacy retained atlas may occupy the whole optional CPU budget.
        // Losing a speculative ready payload must never turn a valid image into
        // an upload failure; its checked immutable locator remains a safe fallback.
        let bytes = if self.device.encoded_bytes.get().saturating_add(charge) > DEFERRED_SOURCE_BYTES {
            match bytes {
                DeferredBytes::Ready { encoded_len, load, .. } => DeferredBytes::Reload { encoded_len, load },
                bytes => bytes,
            }
        } else { bytes };
        let result = self.defer_source(logical, physical, bytes, label,
            DeferredKind::GxtAltered { logical, info, extract });
        if result.is_ok() { audit.finish_hot(label, initial.len() as u64, info.payload_bytes as u64); }
        result
    }

    fn defer_altered_source(
        &self,
        logical: [u32; 2],
        physical: [u32; 2],
        bytes: DeferredBytes,
        label: &str,
        decode: DecodeDeferred,
    ) -> Result<TexRef> {
        self.defer_source(logical, physical, bytes, label,
            DeferredKind::Alter {
                decode: Rc::new(move |bytes| decode(bytes).map(|(w, h, px, srgb)| (w, h, px.into(), srgb))),
            })
    }

    fn defer_source(
        &self,
        logical: [u32; 2],
        physical: [u32; 2],
        bytes: DeferredBytes,
        label: &str,
        kind: DeferredKind,
    ) -> Result<TexRef> {
        ensure!(logical.iter().all(|v| *v != 0), "deferred logical dimensions must be nonzero");
        // Native compressed pan/zoom art may cross either old axis bound while
        // retaining the existing 960x1080 CPU pixel budget. Targets stay 540p.
        let compressed = matches!(&kind, DeferredKind::GxtAltered { .. });
        let max_size = if compressed || matches!(&kind, DeferredKind::Image { .. }) { [2048, 2048] } else { [960, 1080] };
        ensure!(physical[0] != 0 && physical[1] != 0
            && physical[0] <= max_size[0] && physical[1] <= max_size[1], "deferred atlas dimensions exceed compact bounds");
        ensure!(!compressed || u64::from(physical[0]) * u64::from(physical[1]) <= 960 * 1080,
            "deferred GXT exceeds compact decoded pixel budget");
        ensure!(bytes.encoded_len() != 0, "deferred image has no encoded bytes");
        ensure!(bytes.encoded_len() <= DEFERRED_SOURCE_BYTES, "deferred image exceeds encoded entry bound");
        let total = self.device.encoded_bytes.get().checked_add(bytes.retained_len())
            .context("deferred source accounting overflow")?;
        ensure!(total <= DEFERRED_SOURCE_BYTES, "deferred encoded source budget exceeded");
        let native = Rc::new(NativeTexture {
            device: self.device.clone(), id: RefCell::new(None), size: physical,
            bpp: if matches!(&kind, DeferredKind::GxtAltered { .. }) { 0 } else { 4 },
            srgb: false, render_target: false,
            deferred: Some(DeferredSource { bytes, label: label.into(), kind }),
            last_used: Cell::new(0), content_version: Cell::new(0),
            coverage: Cell::new(coverage::Stamp::unknown(0)), rebuild_us: Cell::new(0),
        });
        self.device.encoded_bytes.set(total);
        knife_perf::gauge_set(Gauge::EncodedBytes, total as u64);
        let mut cache = self.device.deferred.borrow_mut();
        cache.retain(|weak| weak.strong_count() != 0);
        cache.push(Rc::downgrade(&native));
        Ok(Arc::new(GpuTexture { native, width: logical[0], height: logical[1], sparse_coverage: None }))
    }

    pub fn owns_texture(&self, tex: &GpuTexture) -> bool {
        Rc::ptr_eq(&self.device, &tex.native.device)
    }

    pub fn write_region(&self, tex: &GpuTexture, rect: [u32; 4], bpp: u32, data: &[u8]) {
        self.try_write_region(tex, rect, bpp, data)
            .expect("updating Vita texture region");
    }

    pub fn try_write_region(
        &self,
        tex: &GpuTexture,
        [x, y, w, h]: [u32; 4],
        bpp: u32,
        data: &[u8],
    ) -> Result<()> {
        self.check_texture(tex)?;
        ensure!(tex.native.bpp != 0, "nonlinear texture cannot be CPU-updated");
        ensure!(tex.native.deferred.is_none(), "deferred source textures are immutable");
        ensure!(
            bpp == tex.native.bpp,
            "region bytes per pixel does not match texture format"
        );
        let [tw, th] = tex.physical_size();
        ensure!(
            w != 0
                && h != 0
                && x.checked_add(w).is_some_and(|v| v <= tw)
                && y.checked_add(h).is_some_and(|v| v <= th),
            "texture update region is out of bounds"
        );
        let stride = w
            .checked_mul(bpp)
            .context("texture update row size overflow")?;
        ensure!(
            layout_bytes(w, h, bpp, stride)? <= data.len(),
            "texture update data is truncated"
        );
        let id = tex.native.resident_id()?;
        self.device.call(|g| g.update_texture(&id, [x, y], [w, h], data, stride))?;
        tex.native.changed();
        Ok(())
    }

    pub fn create_writable(&self, width: u32, height: u32, r8: bool, label: &str) -> TexRef {
        self.try_create_writable(width, height, r8, label)
            .expect("creating Vita writable texture")
    }

    pub fn try_create_writable(
        &self,
        width: u32,
        height: u32,
        r8: bool,
        label: &str,
    ) -> Result<TexRef> {
        make_texture(
            &self.device,
            [width, height],
            [width, height],
            None,
            if r8 {
                TextureFormat::R8
            } else {
                TextureFormat::Rgba8
            },
            false,
            true,
            false,
        )
        .with_context(|| format!("creating writable {label}"))
    }

    /// Global native/JPEG allowance after collecting owners already retired.
    /// This is admission information, not an invitation to drain live GPU work.
    pub fn vita_cache_headroom(&self) -> Result<usize> {
        self.device.call(|g| g.available_cdram())
    }

    /// Retire CPU owners and collect only completed native allocations. No Finish.
    pub fn vita_reclaim_idle(&self) -> Result<()> {
        self.device.call(|g| g.reclaim_retired(false))
    }

    /// Admit a bounded optional render cache inside the existing native/JPEG
    /// limit. That limit already reserves the process's external CDRAM margin;
    /// this adds only a small internal allocation spare, not another 8 MiB.
    /// Keep every recorded/upcoming source and the latest submitted working set.
    /// Cold immutable GPU backings may be retired; their logical image survives.
    pub fn vita_prepare_cache_allocation(&self, encoder: &CommandEncoder,
        upcoming: &[Quad], bytes: usize) -> Result<bool>
    {
        self.check_encoder(encoder)?;
        ensure!(bytes <= knife_perf::memory_budget::GPU_GROUP_BYTES,
            "optional cache request exceeds its bounded group allowance");
        let wanted = bytes.checked_add(knife_perf::memory_budget::GPU_CACHE_SPARE_BYTES)
            .context("optional cache admission size overflow")?;
        let mut pinned = HashSet::new();
        for command in &encoder.commands {
            match command {
                Command::Begin { target, .. } => { pinned.insert(Rc::as_ptr(target)); }
                Command::Draw { textures, .. } => {
                    for source in textures.iter().flatten() { pinned.insert(Rc::as_ptr(source)); }
                }
                Command::Copy { src, dst, .. } => {
                    pinned.insert(Rc::as_ptr(src)); pinned.insert(Rc::as_ptr(dst));
                }
                Command::End | Command::GroupCut(_) => {}
            }
        }
        for q in upcoming {
            pinned.insert(Rc::as_ptr(&q.tex.native));
            if let Some(rule) = &q.rule { pinned.insert(Rc::as_ptr(&rule.tex.native)); }
        }
        self.device.call(|g| g.reclaim_retired(false))?;
        let mut available = self.vita_cache_headroom()?;
        if available >= wanted {
            knife_perf::add(Metric::GpuCacheAdmit, bytes as u64, 0);
            return Ok(true);
        }
        let deficit = wanted - available;
        let epoch = self.device.epoch.get();
        let mut victims: Vec<_> = self.device.deferred.borrow().iter().filter_map(Weak::upgrade)
            .filter(|source| source.id.borrow().is_some()
                && !pinned.contains(&Rc::as_ptr(source)) && source.last_used.get() < epoch)
            .collect();
        victims.sort_by_key(|source| (source.last_used.get(), source.rebuild_us.get()));
        let mut retired = 0usize;
        let mut count = 0u64;
        for source in victims {
            if available >= wanted || retired >= deficit { break; }
            let id = source.id.borrow_mut().take();
            if let Some(id) = id {
                let charge = source.allocation_bytes()?;
                let scope = Scope::new(Metric::DeferredEvict);
                self.device.retire(&id);
                self.device.drain()?;
                source.release_resident_charge()?;
                scope.finish_hot(source.deferred.as_ref().map_or("unknown", |s| &s.label), charge as u64, 0);
                knife_perf::add(Metric::GpuCacheEvict, charge as u64, 1);
                retired = retired.saturating_add(charge);
                count += 1;
                self.device.call(|g| g.reclaim_retired(false))?;
                available = self.vita_cache_headroom()?;
            }
        }
        // A still-running old reader may delay physical recovery. No Finish is
        // introduced for an optional cache; leave its original drawing path.
        let admitted = available >= wanted;
        knife_perf::add(if admitted { Metric::GpuCacheAdmit } else { Metric::GpuCacheBypass },
            bytes as u64, count);
        Ok(admitted)
    }

    /// Includes native pitch and the active pool/non-pool allocation alignment.
    pub fn vita_target_allocation_bytes(&self, target: &RenderTarget) -> Result<usize> {
        self.check_target(target)?;
        target.tex.native.allocation_bytes()
    }

    /// Record a fully cleared group producer and count its actual effect Draw
    /// commands after batching. The clear starts a new pass, so no command can
    /// merge into the portion preceding `first`.
    pub fn try_draw_group_cached_producer(&mut self, encoder: &mut CommandEncoder,
        target: &RenderTarget, quads: &[Quad]) -> Result<usize>
    {
        encoder.close_pass();
        let first = encoder.commands.len();
        self.try_draw(encoder, target, Some([0.0; 4]), quads)?;
        Ok(encoder.commands[first..].iter().filter(|command|
            matches!(command, Command::Draw { .. })).count())
    }

    /// Fully cleared static stage prefix, retaining the original clear alpha
    /// and counting actual effect commands after ordinary batching.
    pub fn try_draw_stage_prefix(&mut self, encoder: &mut CommandEncoder,
        target: &RenderTarget, clear: [f32; 4], quads: &[Quad]) -> Result<usize> {
        encoder.close_pass();
        let first = encoder.commands.len();
        self.try_draw(encoder, target, Some(clear), quads)?;
        Ok(encoder.commands[first..].iter().filter(|command|
            matches!(command, Command::Draw { .. })).count())
    }

    /// Actual recorded Begin commands, including unavoidable snapshot breaks.
    /// This is a submission counter, not an estimated GPU-time saving.
    pub fn vita_target_passes(&self, encoder: &CommandEncoder, target: &RenderTarget) -> Result<usize> {
        self.check_encoder(encoder)?; self.check_target(target)?;
        Ok(encoder.commands.iter().filter(|command| matches!(command,
            Command::Begin { target: native, .. } if Rc::ptr_eq(native, &target.tex.native))).count())
    }

    pub fn vita_group_fused_available(&self, target: &RenderTarget, blend: Blend) -> Result<bool> {
        self.check_target(target)?;
        if !knife_perf::optimizations::enabled(knife_perf::optimizations::GROUP_FUSED) { return Ok(false); }
        if let Blend::Ps(op) = blend { Ok(self.ps_fetch_for_target(target, op)?.is_some()) }
        else { self.device.call(|g| g.shader_available(15)) }
    }

    pub fn create_target(&self, width: u32, height: u32, label: &str) -> RenderTarget {
        self.try_create_target(width, height, label)
            .expect("creating Vita render target")
    }

    pub fn try_create_target(&self, width: u32, height: u32, label: &str) -> Result<RenderTarget> {
        self.create_target_sized([width, height], [width, height], label)
    }

    pub fn create_scene_target(&self, width: u32, height: u32, label: &str) -> RenderTarget {
        self.try_create_scene_target(width, height, label)
            .expect("creating Vita scene target")
    }

    pub fn try_create_scene_target(
        &self,
        width: u32,
        height: u32,
        label: &str,
    ) -> Result<RenderTarget> {
        self.create_target_sized([width, height], scene_size(width, height), label)
    }

    /// Required outputs can release game-owned optional caches after an actual
    /// allocation failure, even when a free-memory query looked sufficient.
    pub fn try_create_scene_target_reclaim(&self, width: u32, height: u32, label: &str,
        reclaim: impl FnOnce() -> usize) -> Result<RenderTarget>
    {
        self.create_required_target(|| self.try_create_scene_target(width, height, label), label, reclaim)
    }

    /// The UI composite target is the display's 960x540 content size, i.e. half of
    /// the 1920x1080 logical canvas. With the scene chain also at 960x540 the
    /// scene-to-canvas blit is 1:1. Script coordinates remain unchanged.
    pub fn create_composite_target(&self, width: u32, height: u32, label: &str) -> RenderTarget {
        self.try_create_composite_target(width, height, label)
            .expect("creating Vita UI composite target")
    }

    /// Fallible allocation for optional frame-output rings. Failure keeps the
    /// existing target usable; physical sizing matches the primary composite.
    pub fn try_create_composite_target(&self, width: u32, height: u32, label: &str) -> Result<RenderTarget> {
        self.create_target_sized([width, height], composite_physical(width, height), label)
    }

    pub fn try_create_composite_target_reclaim(&self, width: u32, height: u32, label: &str,
        reclaim: impl FnOnce() -> usize) -> Result<RenderTarget>
    {
        self.create_required_target(|| self.try_create_composite_target(width, height, label), label, reclaim)
    }

    fn create_required_target(&self, allocate: impl Fn() -> Result<RenderTarget>, label: &str,
        reclaim: impl FnOnce() -> usize) -> Result<RenderTarget>
    {
        let error = match allocate() { Ok(target) => return Ok(target), Err(error) => error };
        if error.downcast_ref::<TextureMemoryPressure>().is_none() { return Err(error); }
        let released = reclaim();
        tracing::warn!(target_label = label, released_bytes = released, error = %format!("{error:#}"),
            "Required render target allocation pressure");
        if released == 0 { return Err(error); }
        // Dropped owners may still be referenced by recorded commands or a GPU
        // fence. Drain actual retirements before one final bounded allocation.
        self.device.call(|g| g.reclaim_retired(true)).context(FramePreparationError)?;
        let result = allocate();
        if result.is_ok() {
            tracing::info!(target_label = label, released_bytes = released,
                "Required render target recovered after cache release");
        }
        result
    }

    fn create_target_sized(
        &self,
        logical: [u32; 2],
        physical: [u32; 2],
        label: &str,
    ) -> Result<RenderTarget> {
        let tex = make_texture(
            &self.device,
            logical,
            physical,
            None,
            TextureFormat::Rgba8,
            true,
            true,
            true,
        )
        .with_context(|| format!("creating render target {label}"))
        .context(FramePreparationError)?;
        #[cfg(target_os = "vita")]
        menu_draw_diagnostic::texture("LABEL", &tex.native, label);
        if label == "canvas" {
            tracing::info!(
                logical_width = logical[0],
                logical_height = logical[1],
                physical_width = physical[0],
                physical_height = physical[1],
                "Scene render canvas"
            );
        }
        #[cfg(target_os = "vita")]
        target_labels::register(Rc::as_ptr(&tex.native) as usize, label);
        Ok(RenderTarget { tex })
    }

    fn check_texture(&self, tex: &GpuTexture) -> Result<()> {
        ensure!(
            Rc::ptr_eq(&self.device, &tex.native.device),
            "texture belongs to another renderer"
        );
        ensure!(
            tex.width != 0 && tex.height != 0,
            "logical texture dimensions must be nonzero"
        );
        Ok(())
    }

    fn check_target(&self, target: &RenderTarget) -> Result<()> {
        self.check_texture(&target.tex)?;
        ensure!(
            target.tex.native.render_target,
            "texture was not created as a render target"
        );
        ensure!(
            target.tex.native.bpp == 4 && !target.tex.native.srgb,
            "Vita render targets must store native encoded UNORM8 pixels"
        );
        Ok(())
    }

    fn check_encoder(&self, encoder: &CommandEncoder) -> Result<()> {
        ensure!(
            Rc::ptr_eq(&self.device, &encoder.device),
            "encoder belongs to another renderer"
        );
        Ok(())
    }

    pub fn draw(
        &mut self,
        encoder: &mut CommandEncoder,
        target: &RenderTarget,
        clear: Option<[f32; 4]>,
        quads: &[Quad],
    ) {
        self.try_draw(encoder, target, clear, quads)
            .expect("recording Vita quad draw");
    }

    /// Bake text RGB with its real over-composited coverage. Ordinary Alpha
    /// intentionally preserves target alpha; the game's Over adds alpha, so a
    /// private native blend is required for a transparent text-cache surface.
    pub fn text_cache_available(&self) -> Result<bool> {
        // The bundled legacy Alter performs a different colour transform. Only
        // the runtime-validated encoded-UNORM passthrough is a cache compositor.
        // Bit 24 includes Text/RimText CoverageOver and passthrough Alpha
        // patch admission, so a cache hit cannot discover a new GPU program failure.
        self.device.call(|g| g.shader_available(24))
    }

    pub fn try_draw_text_cache(&mut self, encoder: &mut CommandEncoder,
        target: &RenderTarget, quads: &[Quad]) -> Result<usize> {
        ensure!(self.text_cache_available()?, "native text-cache compositor unavailable");
        ensure!(quads.iter().all(|q| matches!(q.shade, Shade::Text | Shade::RimText)
            && q.blend == Blend::Alpha && q.rule.is_none() && !q.masked
            && q.clip.is_none() && q.soft.is_none()), "invalid text cache source");
        encoder.close_pass();
        let first = encoder.commands.len();
        self.try_draw(encoder, target, Some([0.0; 4]), quads)?;
        let mut source_draws = 0;
        for command in &mut encoder.commands[first..] {
            if let Command::Draw { blend, .. } = command {
                *blend = EffectBlend::CoverageOver;
                source_draws += 1;
            }
        }
        Ok(source_draws)
    }

    /// Cached text is already premultiplied. fs_alter samples RGBA unchanged;
    /// running it through fs_sprite would multiply coverage a second time.
    /// Alpha keeps the canvas alpha exactly as the original glyph draws did.
    pub fn try_composite_text_cache(&mut self, encoder: &mut CommandEncoder,
        target: &RenderTarget, source: &RenderTarget, rect: [f32; 4]) -> Result<()> {
        ensure!(self.text_cache_available()?, "native text-cache compositor unavailable");
        self.check_encoder(encoder)?;
        self.check_target(target)?;
        self.check_target(source)?;
        ensure!(!Rc::ptr_eq(&source.tex.native, &target.tex.native)
            && rect.iter().all(|v| v.is_finite()) && rect[2] > 0.0 && rect[3] > 0.0,
            "invalid text cache composite");
        let [pw, ph] = target.tex.physical_size();
        let size = [target.tex.width as f32, target.tex.height as f32];
        let reuse = if knife_perf::optimizations::enabled(knife_perf::optimizations::COMPACT_STREAM) {
            self.vertex_reuse.borrow_mut().pop().unwrap_or_default()
        } else { Vec::new() };
        let vertices = quad_vertices_reusing(&Quad::rect(source.tex.clone(), rect, [1.0; 4]),
            size, [pw as f32 / size[0], ph as f32 / size[1]], reuse)?;
        let same_target = encoder.open.as_ref().is_some_and(|open|
            Rc::ptr_eq(open, &target.tex.native));
        if !same_target { encoder.close_pass(); }
        let draw = Command::Draw { work: executed_work::DrawWork::pending(), vertices, shader: EffectShader::Alter,
            blend: EffectBlend::Alpha,
            textures: [Some(source.tex.native.clone()), Some(self.white.native.clone()),
                Some(self.white.native.clone()), Some(self.soft_lut.clone())],
            params: [0.0; 8] };
        if same_target { encoder.append([draw])?; }
        else { encoder.append([Command::Begin { target: target.tex.native.clone(), clear: None }, draw])?; }
        encoder.open = Some(target.tex.native.clone());
        Ok(())
    }

    pub fn try_draw(
        &mut self,
        encoder: &mut CommandEncoder,
        target: &RenderTarget,
        clear: Option<[f32; 4]>,
        quads: &[Quad],
    ) -> Result<()> {
        let _record = Scope::new(Metric::DrawRecord);
        self.check_encoder(encoder)?;
        self.check_target(target)?;
        if let Some(clear) = clear {
            ensure!(clear.iter().all(|v| v.is_finite()), "nonfinite clear color");
        }
        ensure!(
            quads.len() <= MAX_FRAME_COMMANDS / 3,
            "too many quads for a bounded Vita frame"
        );
        let [pw, ph] = target.tex.physical_size();
        let [tw, th] = [target.tex.width as f32, target.tex.height as f32];
        let scale = [pw as f32 / tw, ph as f32 / th];
        let fetch_enabled = knife_perf::optimizations::enabled(knife_perf::optimizations::PS_FETCH);
        let fetch_general = fetch_enabled && self.device.call(|g| g.shader_available(17))?;
        let fetch_normal = fetch_enabled && self.device.call(|g| g.shader_available(18))?;
        let fetch_hard_light = fetch_enabled && self.device.call(|g| g.shader_available(20))?;
        let fetch_overlay = fetch_enabled && self.device.call(|g| g.shader_available(21))?;
        let fetch_darken = fetch_enabled && self.device.call(|g| g.shader_available(25))?;
        let fetch_dodge_alpha = fetch_enabled
            && knife_perf::optimizations::enabled(knife_perf::optimizations::PS_OP7)
            && self.device.call(|g| g.shader_available(29))?;
        let plain_material_enabled = fetch_enabled
            && knife_perf::optimizations::enabled(knife_perf::optimizations::PS_PLAIN_MATERIAL);
        let fetch_plain_add = plain_material_enabled && self.device.call(|g| g.shader_available(30))?;
        let fetch_plain_dodge = plain_material_enabled && self.device.call(|g| g.shader_available(31))?;
        let fetch_plain_op7 = fetch_enabled
            && knife_perf::optimizations::enabled(knife_perf::optimizations::PS_PLAIN_OP7)
            && self.device.call(|g| g.shader_available(48))?;
        let full_material_enabled = fetch_enabled
            && knife_perf::optimizations::enabled(knife_perf::optimizations::PS_FULL_MATERIAL);
        let fetch_full_add = full_material_enabled && self.device.call(|g| g.shader_available(54))?;
        let fetch_full_dodge = full_material_enabled && self.device.call(|g| g.shader_available(55))?;
        let fetch_full_overlay = full_material_enabled && self.device.call(|g| g.shader_available(56))?;
        let fetch_plain_normal = fetch_enabled && self.device.call(|g| g.shader_available(23))?;
        let operation_shaders = if fetch_enabled
            && knife_perf::optimizations::enabled(knife_perf::optimizations::PS_ALL_OPERATIONS) {
            self.device.call(|g| {
                let mut shaders = [None; 24];
                for (index, shader) in ALL_PS_SHADERS.into_iter().enumerate() {
                    if g.shader_available(shader as u32)? { shaders[index] = Some(shader); }
                }
                Ok(shaders)
            })?
        } else { [None; 24] };
        let half_shaders = if fetch_enabled
            && knife_perf::optimizations::enabled(knife_perf::optimizations::HALF_COLOR) {
            self.device.call(|g| {
                let mut shaders = [None; 4];
                for (index, shader) in [EffectShader::PsHalf1, EffectShader::PsHalf6,
                    EffectShader::PsHalf7, EffectShader::PsHalf13].into_iter().enumerate() {
                    if g.shader_available(shader as u32)? { shaders[index] = Some(shader); }
                }
                Ok(shaders)
            })?
        } else { [None; 4] };
        let blend_chain = fetch_enabled
            && knife_perf::optimizations::enabled(knife_perf::optimizations::BLEND_CHAIN)
            && !target.tex.native.srgb && self.device.call(|g| g.shader_available(27))?;
        let batched_rim = self.device.call(|g| g.shader_available(28))?;
        let plain_sprite = self.device.call(|g| g.shader_available(22))?;
        let plain_forcealpha = knife_perf::optimizations::enabled(knife_perf::optimizations::PLAIN_FORCEALPHA)
            && self.device.call(|g| g.shader_available(26))?;
        let material_shaders = if fetch_enabled
            && knife_perf::optimizations::enabled(knife_perf::optimizations::PS_MATERIAL_VARIANTS) {
            self.device.call(|g| {
                let mut ready = [false; 17];
                for (index, shader) in MATERIAL_PS_SHADERS.iter().enumerate() {
                    ready[index] = g.shader_available(*shader as u32)?;
                }
                Ok(ready)
            })?
        } else { [false; 17] };
        let base_ps_shader = |q: &Quad, op| {
            // The independently selected FP32 plain OP7 preserves the generic
            // operation exactly and wins over Half only for this material.
            if op == PsOp::DodgeAlpha && fetch_plain_op7 && plain_material_ps_source(q) {
                return Some(EffectShader::PsPlain7);
            }
            // Explicit hardware-test selection. Complex source processing and
            // group composites retain the complete FP32 source path.
            let half_index = match op { PsOp::Add => Some(0), PsOp::Dodge => Some(1),
                PsOp::DodgeAlpha => Some(2), PsOp::Overlay => Some(3), _ => None };
            if plain_material_ps_source(q)
                && let Some(shader) = half_index.and_then(|index| half_shaders[index]) {
                return Some(shader);
            }
            // ID29 keeps the complete PS source contract, including all material
            // inputs. Unlike the plain specializations it needs no source gate.
            if op == PsOp::Add && fetch_plain_add && plain_material_ps_source(q) {
                Some(EffectShader::PsFetch1)
            } else if op == PsOp::Dodge && fetch_plain_dodge && plain_material_ps_source(q) {
                Some(EffectShader::PsFetch6)
            } else if op == PsOp::Overlay && fetch_overlay && (plain_ps_source(q)
                || (plain_material_enabled && plain_material_ps_source(q))) {
                Some(EffectShader::PsFetch13)
            } else if op==PsOp::HardLight && fetch_hard_light && plain_ps_source(q) {
                Some(EffectShader::PsFetch11)
            } else if op==PsOp::Darken && fetch_darken && plain_ps_source(q) {
                Some(EffectShader::PsFetch2)
            } else if op==PsOp::Normal && fetch_plain_normal && plain_ps19_source(q) {
                Some(EffectShader::PsPlain19)
            } else if let Some(shader)=operation_shaders[op as usize-1] {
                Some(shader)
            } else if op == PsOp::DodgeAlpha && fetch_dodge_alpha {
                Some(EffectShader::PsFetch7)
            } else if op == PsOp::Add && fetch_full_add {
                Some(EffectShader::PsFull1)
            } else if op == PsOp::Dodge && fetch_full_dodge {
                Some(EffectShader::PsFull6)
            } else if op == PsOp::Overlay && fetch_full_overlay {
                Some(EffectShader::PsFull13)
            } else {
                select_ps_fetch(op, fetch_general, fetch_normal,
                    fetch_hard_light && plain_ps_source(q),
                    fetch_overlay && (plain_ps_source(q)
                        || (plain_material_enabled && plain_material_ps_source(q))),
                    fetch_darken && plain_ps_source(q))
            }
        };
        let ps_shader = |q: &Quad, op| {
            let base = base_ps_shader(q, op);
            base.and_then(|previous| material_variant(q, op, previous))
                .filter(|shader| material_shaders[*shader as usize - 88]).or(base)
        };
        let source_shader = |q: &Quad| {
            if q.shade == Shade::RimText && batched_rim { EffectShader::RimTextBatched }
            else if plain_sprite && plain_ps_source(q) { EffectShader::PlainSprite }
            else if plain_forcealpha && q.shade == Shade::ForceAlpha && plain_material(q) {
                EffectShader::PlainForceAlpha
            } else { effect_shader(q.shade) }
        };
        let has_ps = quads.iter().any(|q| matches!(q.blend, Blend::Ps(op)
            if !ps18_direct(q) && ps_shader(q, op).is_none()));
        let backdrop = if has_ps {
            if !self.backdrops.contains_key(&[pw, ph]) {
                let bd = self.create_target_sized(
                    [target.tex.width, target.tex.height],
                    [pw, ph],
                    "backdrop",
                )?;
                self.backdrops.insert([pw, ph], bd.tex.native.clone());
            }
            Some(self.backdrops[&[pw, ph]].clone())
        } else {
            None
        };
        // Consecutive draws into the same target share one pass: a pass costs
        // 12-17 ms on hardware regardless of how little it draws, so the number of
        // Begin/End pairs is what matters, not the quad count.
        let same_target = encoder
            .open
            .as_ref()
            .is_some_and(|open| Rc::ptr_eq(open, &target.tex.native));
        if !same_target || clear.is_some() {
            encoder.close_pass();
        }
        let mut commands = Vec::new();
        let mut open = same_target && clear.is_none();
        // Clear takes effect even for an empty/fully clipped list.
        if clear.is_some() {
            commands.push(Command::Begin {
                target: target.tex.native.clone(),
                clear,
            });
            open = true;
        }
        let mut skip = false;
        for (index,q) in quads.iter().enumerate() {
            if skip { skip = false; continue; }
            self.check_texture(&q.tex)?;
            ensure!(
                !Rc::ptr_eq(&q.tex.native, &target.tex.native),
                "a draw cannot sample its active target"
            );
            if let Some(mask) = &q.rule {
                self.check_texture(&mask.tex)?;
                ensure!(
                    !Rc::ptr_eq(&mask.tex.native, &target.tex.native),
                    "a rule mask cannot alias its target"
                );
                ensure!(
                    mask.progress.is_finite() && mask.soft.is_finite(),
                    "nonfinite rule parameters"
                );
            }
            // A quad whose colour alpha is zero cannot write anything on these blends: the
            // colour term is scaled by alpha and the target alpha is left or added to.
            if q.rule.is_none()
                && q.color[3] <= 0.0
                && matches!(q.blend, Blend::Alpha | Blend::Over | Blend::Footprint)
            {
                continue;
            }
            #[cfg(target_os = "vita")]
            {
                // Bucket by source texture area: only small images can share an atlas page.
                let area = u64::from(q.tex.width) * u64::from(q.tex.height);
                let bucket = if area <= 65_536 { 0 } else if area <= 262_144 { 1 } else { 2 };
                match bucket {
                    0 => { frame_metrics::SMALL_QUADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                    1 => { frame_metrics::MID_QUADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                    _ => { frame_metrics::LARGE_QUADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
                }
            }
            let reuse = if knife_perf::optimizations::enabled(knife_perf::optimizations::COMPACT_STREAM) {
                self.vertex_reuse.borrow_mut().pop().unwrap_or_default()
            } else { Vec::new() };
            let vertices = quad_vertices_reusing(q, [tw, th], scale, reuse)?;
            #[cfg(target_os = "vita")]
            menu_draw_diagnostic::quad(q, target, &vertices, scale);
            if blend_chain && let Some(next) = quads.get(index+1)
                && let (Blend::Ps(a),Blend::Ps(b)) = (q.blend,next.blend)
                && let (Some(sa),Some(sb)) = (ps_shader(q,a),ps_shader(next,b))
                && self.device.call(|g|g.chain_pair_available(a as u32,b as u32))?
            {
                self.check_texture(&next.tex)?;
                ensure!(!Rc::ptr_eq(&next.tex.native,&target.tex.native), "a chain cannot sample its active target");
                // Validate every original source before considering its replacement.
                // Invalid parameters must remain errors, rather than being hidden by
                // an optional optimization or a fully clipped intersection.
                let compact = knife_perf::optimizations::enabled(knife_perf::optimizations::COMPACT_STREAM);
                let reuse = if compact { self.vertex_reuse.borrow_mut().pop().unwrap_or_default() } else { Vec::new() };
                let next_vertices = quad_vertices_reusing(next,[tw,th],scale,reuse)?;
                let pair = blend_chain::pair(q,next,[sa,sb],[tw,th],[pw,ph],
                    &self.white.native,&self.soft_lut);
                if compact { self.vertex_reuse.borrow_mut().push(next_vertices); }
                if let Some(pair) = pair {
                    if compact { self.vertex_reuse.borrow_mut().push(vertices); }
                    if !open { commands.push(Command::Begin {target:target.tex.native.clone(),clear:None}); }
                    commands.extend(pair);open = true;skip = true;continue;
                }
            }
            if let Blend::Ps(op) = q.blend && !ps18_direct(q) {
                // A fully clipped PS quad must not snapshot or open a pass.
                if vertices.is_empty() {
                    continue;
                }
                let fetch_shader = ps_shader(q, op);
                let bd = if fetch_shader.is_some() {
                    if !open {
                        commands.push(Command::Begin { target: target.tex.native.clone(), clear: None });
                    }
                    // Destination comes from the tile's native color register.
                    self.white.native.clone()
                } else {
                    if open { commands.push(Command::End); }
                    let bd = backdrop.as_ref().expect("prepared Ps backdrop");
                    commands.push(Command::Copy { metric: Metric::CopyPs,
                        src: target.tex.native.clone(), dst: bd.clone() });
                    commands.push(Command::Begin { target: target.tex.native.clone(), clear: None });
                    bd.clone()
                };
                let (rule, progress, flags) = match &q.rule {
                    Some(mask) => (mask.tex.native.clone(), mask.progress, mask.flags()),
                    None => (self.white.native.clone(), -1.0, 0.0),
                };
                commands.push(Command::Draw {
                    work: executed_work::DrawWork::pending(),
                    vertices,
                    shader: fetch_shader.unwrap_or(EffectShader::Ps),
                    blend: EffectBlend::Opaque,
                    textures: [
                        Some(q.tex.native.clone()),
                        Some(bd.clone()),
                        Some(rule),
                        Some(self.soft_lut.clone()),
                    ],
                    params: [
                        op as u32 as f32,
                        u32::from(q.shade == Shade::Fill) as f32,
                        pw as f32,
                        ph as f32,
                        progress,
                        flags,
                        u32::from(q.shade == Shade::NoAlpha) as f32,
                        0.0,
                    ],
                });
                // The reopened pass keeps receiving any following quads.
                open = true;
            } else {
                // A clipped quad must not open an empty pass.
                if vertices.is_empty() {
                    continue;
                }
                if !open {
                    commands.push(Command::Begin {
                        target: target.tex.native.clone(),
                        clear: None,
                    });
                    open = true;
                }
                // PS NoAlpha (op18), without Fill, is exactly fs_noalpha:
                // its formula ignores destination, rule and all alpha terms.
                let direct_ps = ps18_direct(q);
                let (shader, rule, params) = if direct_ps {
                    (EffectShader::NoAlpha, None, [18.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -18.0])
                } else if q.masked {
                    // Mask arrives as tex2 and is sampled in canvas space by the shader.
                    match &q.rule {
                        Some(mask) => (EffectShader::SpriteMask, Some(mask.tex.native.clone()),
                            [0.0, 0.0, pw as f32, ph as f32, 0.0, 0.0, 0.0, 0.0]),
                        None => (source_shader(q), None, [0.0; 8]),
                    }
                } else { match &q.rule {
                    Some(mask) => (
                        EffectShader::Rule,
                        Some(mask.tex.native.clone()),
                        [
                            mask.progress,
                            mask.flags(),
                            pw as f32,
                            ph as f32,
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                        ],
                    ),
                    None => (source_shader(q), None, [0.0; 8]),
                } };
                commands.push(Command::Draw {
                    work: executed_work::DrawWork::pending(),
                    vertices,
                    shader,
                    blend: if direct_ps { EffectBlend::Opaque } else { effect_blend(q.blend) },
                    textures: [
                        Some(q.tex.native.clone()),
                        Some(self.white.native.clone()),
                        rule,
                        Some(self.soft_lut.clone()),
                    ],
                    params,
                });
            }
        }
        // Left open on purpose: the next draw into this target appends to it, and
        // close_pass() runs before copies, transitions, blurs and present.
        encoder.open = if open { Some(target.tex.native.clone()) } else { None };
        encoder.append(commands)
    }

    /// Compose a group using the original copy -> mask -> sample pipeline.
    /// The caller lends a target whose previous contents have no remaining
    /// reader, and must not overwrite it before this recorded sample finishes.
    /// Commands retain all four surfaces and native pass/copy fences order use.
    #[allow(clippy::too_many_arguments)]
    pub fn try_group_composite_via_scratch(&mut self, encoder: &mut CommandEncoder,
        target: &RenderTarget, pixel: &RenderTarget, mask: &RenderTarget,
        scratch: &RenderTarget, blend: Blend, alpha: f32) -> Result<()>
    {
        self.try_group_composite_via_scratch_region(
            encoder, target, pixel, mask, scratch, blend, alpha, CoverageBound::Full,
        )
    }

    /// Scratch fallback variant with the same conservative final-composite
    /// bound as the fused path. Pixel/mask producer work and the scratch copy
    /// remain full size; only the final target write may be narrowed.
    #[allow(clippy::too_many_arguments)]
    pub fn try_group_composite_via_scratch_region(&mut self, encoder: &mut CommandEncoder,
        target: &RenderTarget, pixel: &RenderTarget, mask: &RenderTarget,
        scratch: &RenderTarget, blend: Blend, alpha: f32, requested: CoverageBound) -> Result<()>
    {
        self.check_encoder(encoder)?;
        ensure!(alpha.is_finite(), "nonfinite group opacity");
        let requested = group_owner_coverage(encoder, pixel, mask, requested);
        let fused_layout = !target.tex.native.srgb && !pixel.tex.native.srgb && !mask.tex.native.srgb;
        let fused_program = if let Blend::Ps(op) = blend {
            self.ps_fetch_for_target(target, op)?.is_some()
        } else { self.device.call(|g| g.shader_available(15))? };
        if knife_perf::optimizations::enabled(knife_perf::optimizations::GROUP_FUSED)
            && fused_layout && fused_program {
            return self.try_group_composite_region(encoder, target, pixel, mask, blend, alpha, requested);
        }
        encoder.close_pass();
        let surfaces = [target, pixel, mask, scratch];
        for (index, surface) in surfaces.iter().enumerate() {
            self.check_target(surface)?;
            ensure!([surface.tex.width, surface.tex.height] == [target.tex.width, target.tex.height]
                && surface.tex.physical_size() == target.tex.physical_size(),
                "group surface layouts differ");
            for previous in &surfaces[..index] {
                ensure!(!Rc::ptr_eq(&surface.tex.native, &previous.tex.native),
                    "group target, persistent surfaces and scratch must be distinct");
            }
        }
        let full = [0.0, 0.0, target.tex.width as f32, target.tex.height as f32];
        self.try_copy_target(encoder, pixel, scratch)?;
        if let Some(Command::Copy { metric, .. }) = encoder.commands.last_mut() {
            *metric = Metric::CopyGroup;
        }
        self.try_draw(encoder, scratch, None, &[
            Quad { blend: Blend::MaskAlpha, ..Quad::rect(mask.tex.clone(), full, [1.0; 4]) },
        ])?;
        let requested = group_composite_coverage(group_owner_coverage(encoder, pixel, mask, requested), blend);
        if matches!(requested, CoverageBound::Rect { .. }) {
            encoder.close_pass();
            encoder.append([Command::GroupCut(coverage::GroupCut::new(
                [pixel.tex.native.clone(), mask.tex.native.clone()], [1.0, 1.0, 1.0, alpha]))])?;
        }
        let composite = group_composite_quad(scratch.tex.clone(), target.tex.width,
            target.tex.height, target.tex.physical_size(), requested, blend, alpha);
        self.try_draw(encoder, target, None, &[composite])
    }

    /// Runtime-compiled corrected program, selected only after native capability
    /// validation. The game keeps the scratch fallback if it is unavailable.
    /// Full-canvas layer-group composition. Pixel/mask surfaces retain their
    /// cross-frame state; alpha multiplication is fused into the final sample.
    /// Ps still needs the same per-draw destination snapshot as ordinary quads.
    pub fn group_composite(&mut self, encoder: &mut CommandEncoder, target: &RenderTarget,
        pixel: &RenderTarget, mask: &RenderTarget, blend: Blend, alpha: f32)
    {
        self.try_group_composite(encoder, target, pixel, mask, blend, alpha)
            .expect("recording Vita layer-group composite");
    }

    fn ps_fetch_for_target(&self, target: &RenderTarget, op: PsOp) -> Result<Option<EffectShader>> {
        if target.tex.native.srgb || !knife_perf::optimizations::enabled(knife_perf::optimizations::PS_FETCH) {
            return Ok(None);
        }
        if knife_perf::optimizations::enabled(knife_perf::optimizations::PS_GROUP_MATERIAL) {
            let group = match op as u32 {
                6 => Some(EffectShader::PsGroup6), 8 => Some(EffectShader::PsGroup8),
                14 => Some(EffectShader::PsGroup14), _ => None,
            };
            if let Some(shader) = group
                && self.device.call(|g|g.shader_available(shader as u32))? { return Ok(Some(shader)); }
        }
        if knife_perf::optimizations::enabled(knife_perf::optimizations::PS_ALL_OPERATIONS) {
            let shader=ALL_PS_SHADERS[op as usize-1];
            if self.device.call(|g|g.shader_available(shader as u32))? {return Ok(Some(shader));}
        }
        if op == PsOp::DodgeAlpha
            && knife_perf::optimizations::enabled(knife_perf::optimizations::PS_OP7)
            && self.device.call(|g| g.shader_available(29))? {
            return Ok(Some(EffectShader::PsFetch7));
        }
        if knife_perf::optimizations::enabled(knife_perf::optimizations::PS_FULL_MATERIAL) {
            let dedicated = match op {
                PsOp::Add => Some(EffectShader::PsFull1),
                PsOp::Dodge => Some(EffectShader::PsFull6),
                PsOp::Overlay => Some(EffectShader::PsFull13),
                _ => None,
            };
            if let Some(shader) = dedicated
                && self.device.call(|g| g.shader_available(shader as u32))? {
                return Ok(Some(shader));
            }
        }
        // This method is used only by full-layout group composites. Their
        // fixed vertices/params satisfy the independent group19 ABI.
        let group_normal = op == PsOp::Normal && self.device.call(|g| g.shader_available(19))?;
        let normal = !group_normal && op == PsOp::Normal && self.device.call(|g| g.shader_available(18))?;
        let general = !group_normal && !normal && self.device.call(|g| g.shader_available(17))?;
        Ok(select_group_ps_fetch(op, general, normal, group_normal))
    }

    pub fn try_group_composite(&mut self, encoder: &mut CommandEncoder, target: &RenderTarget,
        pixel: &RenderTarget, mask: &RenderTarget, blend: Blend, alpha: f32) -> Result<()>
    {
        self.try_group_composite_region(encoder, target, pixel, mask, blend, alpha, CoverageBound::Full)
    }

    /// Record the final group composite into a conservative physical-pixel
    /// region. Producer draws, clears and PS backdrop copies stay full-canvas;
    /// only the final composite geometry is narrowed. The optional path is
    /// gated by GROUP_BOUNDS and requires transparent source pixels to leave
    /// both destination colour and alpha unchanged for the selected blend.
    #[allow(clippy::too_many_arguments)]
    pub fn try_group_composite_region(&mut self, encoder: &mut CommandEncoder,
        target: &RenderTarget, pixel: &RenderTarget, mask: &RenderTarget,
        blend: Blend, alpha: f32, requested: CoverageBound) -> Result<()>
    {
        self.check_encoder(encoder)?;
        self.check_target(target)?;
        ensure!(alpha.is_finite(), "nonfinite group opacity");
        let logical = [target.tex.width, target.tex.height];
        let physical = target.tex.physical_size();
        for source in [pixel, mask] {
            self.check_target(source)?;
            ensure!(logical == [source.tex.width, source.tex.height]
                && physical == source.tex.physical_size(), "group surface layouts differ");
            ensure!(!Rc::ptr_eq(&target.tex.native, &source.tex.native),
                "group composite cannot sample its active target");
        }
        let requested = group_composite_coverage(group_owner_coverage(encoder, pixel, mask, requested), blend);
        let Some(vertices) = group_composite_vertices(requested, physical, [1., 1., 1., alpha]) else {
            return Ok(());
        };
        let same_target = encoder.open.as_ref().is_some_and(|open|
            Rc::ptr_eq(open, &target.tex.native));
        if !same_target { encoder.close_pass(); }
        let mut commands = Vec::with_capacity(4);
        let mut open = same_target;
        let (shader, native_blend, second, third, params) = if let Blend::Ps(op) = blend {
            let fetch_shader = self.ps_fetch_for_target(target, op)?;
            let bd = if fetch_shader.is_some() { self.white.native.clone() } else {
                if open { commands.push(Command::End); open = false; }
                if !self.backdrops.contains_key(&physical) {
                    let bd = self.create_target_sized(logical, physical, "backdrop")?;
                    self.backdrops.insert(physical, bd.tex.native.clone());
                }
                let bd = self.backdrops[&physical].clone();
                commands.push(Command::Copy { metric: Metric::CopyPs, src: target.tex.native.clone(), dst: bd.clone() });
                bd
            };
            (fetch_shader.unwrap_or(EffectShader::Ps), EffectBlend::Opaque, bd, mask.tex.native.clone(),
                [op as u32 as f32, 0., physical[0] as f32, physical[1] as f32, -1., 0., 0., 1.])
        } else {
            (EffectShader::GroupComposite, effect_blend(blend), mask.tex.native.clone(),
                self.white.native.clone(), [0.; 8])
        };
        if !open { commands.push(Command::Begin { target: target.tex.native.clone(), clear: None }); }
        if matches!(requested, CoverageBound::Rect { .. }) {
            commands.push(Command::GroupCut(coverage::GroupCut::new(
                [pixel.tex.native.clone(), mask.tex.native.clone()], [1.0, 1.0, 1.0, alpha])));
        }
        commands.push(Command::Draw {
            work: executed_work::DrawWork::pending(),
            vertices, shader, blend: native_blend,
            textures: [Some(pixel.tex.native.clone()), Some(second), Some(third), Some(self.soft_lut.clone())],
            params,
        });
        encoder.append(commands)?;
        encoder.open = Some(target.tex.native.clone());
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn transition(
        &mut self,
        encoder: &mut CommandEncoder,
        dst: &RenderTarget,
        old: &GpuTexture,
        new: &GpuTexture,
        rule: Option<&GpuTexture>,
        progress: f32,
        soft: f32,
    ) {
        self.try_transition(encoder, dst, old, new, rule, progress, soft)
            .expect("recording Vita transition");
    }

    #[allow(clippy::too_many_arguments)]
    pub fn try_transition(
        &mut self,
        encoder: &mut CommandEncoder,
        dst: &RenderTarget,
        old: &GpuTexture,
        new: &GpuTexture,
        rule: Option<&GpuTexture>,
        progress: f32,
        soft: f32,
    ) -> Result<()> {
        self.check_encoder(encoder)?;
        self.check_target(dst)?;
        self.check_texture(old)?;
        self.check_texture(new)?;
        encoder.close_pass();
        let selected_rule = rule.unwrap_or(&self.white);
        self.check_texture(selected_rule)?;
        for source in [old, new, selected_rule] {
            ensure!(
                !Rc::ptr_eq(&source.native, &dst.tex.native),
                "transition cannot sample its active target"
            );
        }
        ensure!(
            progress.is_finite() && soft.is_finite(),
            "nonfinite transition parameters"
        );
        encoder.append(vec![
            Command::Begin {
                target: dst.tex.native.clone(),
                clear: None,
            },
            Command::Draw {
                work: executed_work::DrawWork::pending(),
                vertices: fullscreen_verts([1.0; 4]).to_vec(),
                shader: EffectShader::Transition,
                blend: EffectBlend::Opaque,
                textures: [
                    Some(old.native.clone()),
                    Some(new.native.clone()),
                    Some(selected_rule.native.clone()),
                    Some(self.soft_lut.clone()),
                ],
                params: [
                    progress,
                    rule_flags(0, false, soft),
                    u32::from(rule.is_some()) as f32,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                ],
            },
            Command::End,
        ])
    }

    pub fn blur(
        &mut self,
        encoder: &mut CommandEncoder,
        src: &GpuTexture,
        dst: &RenderTarget,
        dir: [f32; 2],
        sigma: f32,
        mode: BlurMode,
    ) {
        self.try_blur(encoder, src, dst, dir, sigma, mode)
            .expect("recording Vita blur");
    }

    pub fn try_blur(
        &mut self,
        encoder: &mut CommandEncoder,
        src: &GpuTexture,
        dst: &RenderTarget,
        dir: [f32; 2],
        sigma: f32,
        mode: BlurMode,
    ) -> Result<()> {
        self.check_encoder(encoder)?;
        self.check_texture(src)?;
        self.check_target(dst)?;
        encoder.close_pass();
        ensure!(
            !Rc::ptr_eq(&src.native, &dst.tex.native),
            "blur requires distinct source and destination textures"
        );
        ensure!(
            sigma.is_finite() && sigma >= 0.0 && dir.iter().all(|v| v.is_finite()),
            "invalid blur parameters"
        );
        let [pw, ph] = src.physical_size();
        let params = [
            dir[0] * pw as f32 / src.width as f32,
            dir[1] * ph as f32 / src.height as f32,
            sigma,
            (sigma * 3.0).ceil(),
            mode as u32 as f32,
            0.0,
            0.0,
            0.0,
        ];
        ensure!(
            params.iter().all(|v| v.is_finite()) && params[3] < 2_147_483_648.0,
            "blur parameters overflow their native representation"
        );
        let shader = if knife_perf::optimizations::enabled(knife_perf::optimizations::BLUR_WEIGHTS)
            && params[3] <= 16.0 && self.device.call(|g| g.shader_available(53))? {
            EffectShader::BlurWeights
        } else { EffectShader::Blur };
        encoder.append(vec![
            Command::Begin {
                target: dst.tex.native.clone(),
                clear: None,
            },
            Command::Draw {
                work: executed_work::DrawWork::pending(),
                vertices: fullscreen_verts([1.0; 4]).to_vec(),
                shader,
                blend: EffectBlend::Opaque,
                textures: [
                    Some(src.native.clone()),
                    Some(self.white.native.clone()),
                    Some(self.white.native.clone()),
                    Some(self.soft_lut.clone()),
                ],
                params,
            },
            Command::End,
        ])
    }

    pub fn copy_target(
        &self,
        encoder: &mut CommandEncoder,
        src: &RenderTarget,
        dst: &RenderTarget,
    ) {
        self.try_copy_target(encoder, src, dst)
            .expect("recording Vita target copy");
    }

    pub fn blit_target(&mut self, encoder: &mut CommandEncoder, src: &RenderTarget, dst: &RenderTarget) {
        self.try_blit_target(encoder, src, dst).expect("recording Vita scaled target blit");
    }

    pub fn try_blit_target(&mut self, encoder: &mut CommandEncoder, src: &RenderTarget, dst: &RenderTarget) -> Result<()> {
        self.check_encoder(encoder)?;
        self.check_target(src)?;
        self.check_target(dst)?;
        ensure!(!Rc::ptr_eq(&src.tex.native, &dst.tex.native), "target blit source and destination alias");
        let quad = Quad { blend: Blend::Opaque, ..Quad::rect(src.tex.clone(),
            [0.0, 0.0, dst.tex.width as f32, dst.tex.height as f32], [1.0; 4]) };
        self.try_draw(encoder, dst, None, &[quad])
    }

    pub fn try_copy_target(
        &self,
        encoder: &mut CommandEncoder,
        src: &RenderTarget,
        dst: &RenderTarget,
    ) -> Result<()> {
        self.check_encoder(encoder)?;
        self.check_target(src)?;
        self.check_target(dst)?;
        ensure!(
            src.tex.physical_size() == dst.tex.physical_size(),
            "target copy dimensions differ"
        );
        ensure!(
            !Rc::ptr_eq(&src.tex.native, &dst.tex.native),
            "target copy source and destination alias"
        );
        encoder.close_pass();
        encoder.append(vec![Command::Copy {
            metric: Metric::CopyTarget,
            src: src.tex.native.clone(),
            dst: dst.tex.native.clone(),
        }])
    }

    pub fn encoder(&self) -> CommandEncoder {
        CommandEncoder {
            device: self.device.clone(),
            vertex_reuse: self.vertex_reuse.clone(),
            commands: if knife_perf::optimizations::enabled(knife_perf::optimizations::COMPACT_STREAM) {
                std::mem::take(&mut *self.command_reuse.borrow_mut())
            } else { Vec::new() },
            vertex_bytes: 0,
            open: None,
            coverage_id: coverage::encoder_id(),
            coverage_open: None,
        }
    }

    /// Submit pending draws and run the native letterboxed output blit.
    pub fn present(&mut self, encoder: CommandEncoder, canvas: &RenderTarget) {
        self.try_present(encoder, canvas)
            .expect("submitting Vita frame");
    }

    pub fn try_present(&mut self, encoder: CommandEncoder, canvas: &RenderTarget) -> Result<()> {
        self.try_present_with_admission(encoder, canvas, |reserve| reserve())
    }

    /// Prepare this frame's missing textures using the caller's existing idle
    /// image-cache admission policy. The reservation pins the complete frame;
    /// retrying it neither re-decodes the source nor submits partial game draws.
    /// Native upload/pass failures are propagated, never retried as pressure.
    pub fn try_present_with_admission(
        &mut self,
        mut encoder: CommandEncoder,
        canvas: &RenderTarget,
        mut admit: impl FnMut(&mut dyn FnMut() -> Result<()>) -> Result<()>,
    ) -> Result<()> {
        self.check_encoder(&encoder)?;
        self.check_target(canvas)?;
        encoder.close_pass();
        #[cfg(target_os = "vita")]
        {
            frame_metrics::QUADS.fetch_add((encoder.vertex_bytes / (std::mem::size_of::<Vertex>() * 6)) as u64,
                std::sync::atomic::Ordering::Relaxed);
            let (total, needing) = analyse_passes(&encoder.commands);
            frame_metrics::PASS_TOTAL.fetch_add(total, std::sync::atomic::Ordering::Relaxed);
            frame_metrics::PASS_NEEDING.fetch_add(needing, std::sync::atomic::Ordering::Relaxed);
        }
        #[cfg(target_os = "vita")]
        let exec_start = std::time::Instant::now();
        coverage::before_submit(&mut encoder.commands, encoder.coverage_id);
        if let Err(error) = execute_with_admission(&self.device, &encoder.commands, &mut admit) {
            #[cfg(target_os = "vita")]
            menu_draw_diagnostic::frame_pressure(&encoder.commands);
            return Err(error);
        }
        coverage::finish(&encoder.commands, encoder.coverage_id, true);
        #[cfg(target_os = "vita")]
        frame_metrics::add(&frame_metrics::EXEC_US, exec_start);
        if !self.hidden {
            let id = canvas.tex.native.resident_id()?;
            #[cfg(target_os = "vita")]
            let present_start = std::time::Instant::now();
            self.device.call(|g| g.present_canvas(&id))?;
            #[cfg(target_os = "vita")]
            frame_metrics::add(&frame_metrics::PRESENT_US, present_start);
        }
        #[cfg(target_os = "vita")]
        frame_metrics::FRAMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if knife_perf::optimizations::enabled(knife_perf::optimizations::COMPACT_STREAM) {
            // Native submission has copied vertices; resource owners are retired by native completion.
            let mut commands = std::mem::take(&mut encoder.commands);
            let mut vertices = self.vertex_reuse.borrow_mut();
            for command in commands.drain(..) {
                if let Command::Draw { vertices: buffer, .. } = command {
                    vertices.push(buffer);
                }
            }
            *self.command_reuse.borrow_mut() = commands;
        }
        Ok(())
    }

    pub fn read_target(&self, target: &RenderTarget) -> Vec<u8> {
        self.try_read_target(target)
            .expect("reading Vita render target")
    }

    pub fn try_read_target(&self, target: &RenderTarget) -> Result<Vec<u8>> {
        self.check_target(target)?;
        let id = target.tex.native.resident_id()?;
        let pixels = self.device.call(|g| g.read_texture(&id))?;
        let [w, h] = target.tex.physical_size();
        ensure!(
            pixels.len()
                == layout_bytes(w, h, 4, w.checked_mul(4).context("readback row overflow")?)?,
            "native target readback did not return tightly packed RGBA8"
        );
        Ok(pixels)
    }
}

#[allow(clippy::too_many_arguments)]
fn make_texture(
    device: &Rc<Device>,
    logical: [u32; 2],
    physical: [u32; 2],
    bytes: Option<&[u8]>,
    format: TextureFormat,
    srgb: bool,
    linear: bool,
    render_target: bool,
) -> Result<TexRef> {
    make_texture_inner(device, logical, physical, bytes, format, srgb, linear, render_target, true, &HashSet::new())
}

#[allow(clippy::too_many_arguments)]
fn make_texture_inner(
    device: &Rc<Device>,
    logical: [u32; 2],
    physical: [u32; 2],
    bytes: Option<&[u8]>,
    format: TextureFormat,
    _srgb: bool,
    linear: bool,
    render_target: bool,
    reclaim: bool,
    pinned: &HashSet<*const NativeTexture>,
) -> Result<TexRef> {
    let srgb = false;
    ensure!(
        logical.iter().all(|v| *v != 0),
        "logical texture dimensions must be nonzero"
    );
    let bpp = match format {
        TextureFormat::Rgba8 => 4,
        TextureFormat::R8 => 1,
    };
    ensure!(!srgb || bpp == 4, "R8 cannot be interpreted as sRGB");
    let [w, h] = physical;
    let stride = w.checked_mul(bpp).context("texture row size overflow")?;
    let length = layout_bytes(w, h, bpp, stride)?;
    if let Some(bytes) = bytes {
        ensure!(bytes.len() >= length, "texture source is truncated");
    }
    let allocation = native_allocation_bytes(physical, bpp)?;
    if reclaim {
        reserve_cdram(device, allocation, 0, pinned)?;
    }
    let id = allocate_texture_with_recovery(device, pinned,
        |g| g.create_texture_ex(bytes, w, h, stride, format, srgb, linear, render_target))?;
    let native = Rc::new(NativeTexture {
            device: device.clone(),
            id: RefCell::new(Some(id)),
            size: physical,
            bpp,
            srgb,
            render_target,
            deferred: None,
            last_used: Cell::new(0), content_version: Cell::new(0),
            coverage: Cell::new(coverage::Stamp::unknown(0)), rebuild_us: Cell::new(0),
        });
    #[cfg(target_os = "vita")]
    menu_draw_diagnostic::texture("CREATE", &native, "");
    Ok(Arc::new(GpuTexture {
        native,
        width: logical[0],
        height: logical[1],
        sparse_coverage: None,
    }))
}

// Existing checked-in NoAlpha GXP already implements PS op18's non-fill
// equation. This subset requires no runtime shader compiler.
fn select_ps_fetch(op: PsOp, general: bool, normal: bool,
    plain_hard_light: bool, plain_overlay: bool, plain_darken: bool) -> Option<EffectShader> {
    if op == PsOp::Normal && normal { Some(EffectShader::PsFetch19) }
    else if op == PsOp::HardLight && plain_hard_light { Some(EffectShader::PsFetch11) }
    else if op == PsOp::Overlay && plain_overlay { Some(EffectShader::PsFetch13) }
    else if op == PsOp::Darken && plain_darken { Some(EffectShader::PsFetch2) }
    else if general { Some(EffectShader::PsFetch) } else { None }
}

// Fixed variants keep the precision of the selected baseline. No scene/asset gate.
const MATERIAL_PS_SHADERS: [EffectShader;17] = [EffectShader::PsMaterial88,EffectShader::PsMaterial89,EffectShader::PsMaterial90,EffectShader::PsMaterial91,EffectShader::PsMaterial92,EffectShader::PsMaterial93,EffectShader::PsMaterial94,EffectShader::PsMaterial95,EffectShader::PsMaterial96,EffectShader::PsMaterial97,EffectShader::PsMaterial98,EffectShader::PsMaterial99,EffectShader::PsMaterial100,EffectShader::PsMaterial101,EffectShader::PsMaterial102,EffectShader::PsMaterial103,EffectShader::PsMaterial104];
fn material_variant(q: &Quad, op: PsOp, base: EffectShader) -> Option<EffectShader> {
    if !matches!(q.shade, Shade::Sprite | Shade::Fill | Shade::NoAlpha)
        || q.rule.is_some() || q.soft.is_some() || q.masked { return None; }
    let index = match op { PsOp::Add=>Some(0), PsOp::Dodge=>Some(1),
        PsOp::DodgeAlpha=>Some(2), PsOp::Overlay=>Some(3), _=>None };
    let plain=q.haze.iter().flatten().all(|value| *value==0.0);
    if let Some(index)=index {
        if !plain { return Some(MATERIAL_PS_SHADERS[8+index]); }
        let expected=[EffectShader::PsHalf1,EffectShader::PsHalf6,
            EffectShader::PsPlain7,EffectShader::PsHalf13][index];
        if base==expected {
            let shade=match q.shade { Shade::Fill=>0, Shade::NoAlpha=>1, _=>return None };
            return Some(MATERIAL_PS_SHADERS[index*2+shade]);
        }
    }
    if plain {
        for (index, operation) in [3u32,4,5,11,14].into_iter().enumerate() {
            if op as u32==operation && base as u32==63+operation {
                return Some(MATERIAL_PS_SHADERS[12+index]);
            }
        }
    }
    None
}

const ALL_PS_SHADERS:[EffectShader;24]=[EffectShader::PsAll1,EffectShader::PsAll2,EffectShader::PsAll3,EffectShader::PsAll4,EffectShader::PsAll5,EffectShader::PsAll6,EffectShader::PsAll7,EffectShader::PsAll8,EffectShader::PsAll9,EffectShader::PsAll10,EffectShader::PsAll11,EffectShader::PsAll12,EffectShader::PsAll13,EffectShader::PsAll14,EffectShader::PsAll15,EffectShader::PsAll16,EffectShader::PsAll17,EffectShader::PsAll18,EffectShader::PsAll19,EffectShader::PsAll20,EffectShader::PsAll21,EffectShader::PsAll22,EffectShader::PsAll23,EffectShader::PsAll24];

fn select_group_ps_fetch(op: PsOp, general: bool, normal: bool, group_normal: bool) -> Option<EffectShader> {
    if op == PsOp::Normal && group_normal { Some(EffectShader::PsGroup19) }
    else { select_ps_fetch(op, general, normal, false, false, false) }
}

fn plain_ps_source(q: &Quad) -> bool {
    q.shade == Shade::Sprite && plain_material(q)
}

fn plain_material_ps_source(q: &Quad) -> bool {
    matches!(q.shade, Shade::Sprite | Shade::Fill | Shade::NoAlpha) && plain_material(q)
}

fn plain_material(q: &Quad) -> bool {
    q.rule.is_none() && q.soft.is_none() && !q.masked
        // Include hx.w: dropping that flag would lose premultiplication.
        && q.haze.iter().flatten().all(|value| *value == 0.0)
}

// Group pixel preparation changes plain clipped layers to NoAlpha and can
// retain Fill. These two flags have short, explicit source equations too.
fn plain_ps19_source(q: &Quad) -> bool {
    matches!(q.shade, Shade::Sprite | Shade::NoAlpha | Shade::Fill)
        && q.rule.is_none() && q.soft.is_none() && !q.masked
        && q.haze.iter().flatten().all(|value| *value == 0.0)
}

fn ps18_direct(q: &Quad) -> bool {
    knife_perf::optimizations::enabled(knife_perf::optimizations::PS_FETCH)
        && matches!(q.blend, Blend::Ps(PsOp::NoAlpha)) && q.shade != Shade::Fill
}

/// Match the native linear texture's 8-pixel pitch and CDRAM memblock rounding,
/// including R8 atlas allocations. Validate before evicting any cached image.
fn native_allocation_bytes([width, height]: [u32; 2], bpp: u32) -> Result<usize> {
    ensure!(width != 0 && height != 0 && width <= 2048 && height <= 2048,
        "native texture dimensions are outside 1..=2048");
    ensure!(bpp == 1 || bpp == 4, "native texture allocation format is unsupported");
    let pitch = width.checked_add(7).context("native texture pitch overflow")? & !7;
    let bytes = u64::from(pitch).checked_mul(u64::from(height))
        .and_then(|v| v.checked_mul(u64::from(bpp))).context("native texture size overflow")?;
    Ok(Gxm::texture_charge(u32::try_from(bytes)
        .context("native texture allocation exceeds address space")?).map_err(anyhow::Error::msg)? as usize)
}

/// Reclaim idle atlases against both cache policy and measured native headroom.
/// Query again after each fenced native destruction: kernel pressure need not
/// equal our own accounting. The query is an observation, so an allocation can
/// still fail and must retain normal RAII rollback.
fn reserve_cdram(
    device: &Device,
    peak: usize,
    cache_growth: usize,
    pinned: &HashSet<*const NativeTexture>,
) -> Result<()> {
    let _reserve = Scope::new(Metric::DeferredReserve);
    let budget = device.deferred_budget.get();
    let fits_cache = || -> Result<bool> {
        Ok(device.resident_deferred_bytes.get().checked_add(cache_growth)
            .context("deferred cache size overflow")? <= budget)
    };
    let mut available = device.call(|g| g.available_cdram()).context("querying native CDRAM allowance")?;
    if fits_cache()? && available >= peak { return Ok(()); }
    if available < peak {
        // Logical eviction can precede physical completion. Wait only under
        // real memory pressure, then account the actually reclaimed storage.
        device.call(|g| g.reclaim_retired(true))?;
        available = device.call(|g| g.available_cdram())?;
        if fits_cache()? && available >= peak { return Ok(()); }
    }
    // Build and sort victims only under actual pressure. A scene with enough
    // headroom must not repeatedly walk every registered logical texture.
    #[cfg(test)]
    device.admission_scans.set(device.admission_scans.get() + 1);
    let mut victims = evictable_deferred(device, pinned).into_iter();
    loop {
        let fits_cache = fits_cache()?;
        if fits_cache && available >= peak { return Ok(()); }
        let Some(victim) = victims.next() else {
            if !fits_cache {
                return Err(anyhow!("deferred texture budget has no evictable atlas: {} + {cache_growth} > {budget}",
                    device.resident_deferred_bytes.get()).context(TextureMemoryPressure));
            }
            return Err(anyhow!("native CDRAM headroom has no evictable atlas: bake/allocation peak {peak} > available {available}")
                .context(TextureMemoryPressure));
        };
        evict_deferred(device, &victim, pinned.len())?;
        available = device.call(|g| g.available_cdram()).context("querying native CDRAM allowance")?;
        if available < peak {
            device.call(|g| g.reclaim_retired(true))?;
            available = device.call(|g| g.available_cdram())?;
        }
    }
}

fn evictable_deferred(device: &Device, pinned: &HashSet<*const NativeTexture>) -> Vec<Rc<NativeTexture>> {
    let mut victims: Vec<_> = device.deferred.borrow().iter().filter_map(Weak::upgrade)
        .filter(|texture| !pinned.contains(&Rc::as_ptr(texture)) && texture.id.borrow().is_some()).collect();
    if knife_perf::optimizations::enabled(knife_perf::optimizations::GRADE_CACHE) {
        let epoch = device.epoch.get();
        // LRU age bands limit cost preference to similarly recent resources.
        // A very stale expensive image cannot displace the current working set.
        victims.sort_by_key(|texture| {
            let age = epoch.saturating_sub(texture.last_used.get());
            let age_band = age / 8;
            (std::cmp::Reverse(age_band), texture.rebuild_us.get().min(2_000_000), texture.last_used.get())
        });
    } else { victims.sort_by_key(|texture| texture.last_used.get()); }
    victims
}

fn evict_deferred(device: &Device, victim: &NativeTexture, _pinned_count: usize) -> Result<()> {
    let id = victim.id.borrow_mut().take();
    if let Some(id) = id {
        let evict = Scope::new(Metric::DeferredEvict);
        let bytes = victim.allocation_bytes()?;
        #[cfg(target_os = "vita")]
        menu_draw_diagnostic::residency("EVICT", victim, _pinned_count);
        device.retire(&id);
        // An uncertain native retirement poisons subsequent allocation;
        // never pretend that a failed release recovered this memory.
        device.drain()?;
        victim.release_resident_charge()?;
        evict.finish_hot(victim.deferred.as_ref().map_or("unknown", |s| &s.label), bytes as u64, 0);
    }
    Ok(())
}

/// A free-memory query cannot reserve a contiguous kernel block. Recover only
/// from an explicit native allocation failure: one completion drain, then one
/// retry per idle residency victim. Inputs/decoded pixels stay borrowed, and
/// every texture required by the submitted frame stays pinned throughout.
fn allocate_texture_with_recovery(device: &Device, pinned: &HashSet<*const NativeTexture>,
    mut allocate: impl FnMut(&mut Gxm) -> std::result::Result<TextureId, String>) -> Result<TextureId> {
    let mut error = match device.call(&mut allocate) {
        Ok(id) => return Ok(id),
        Err(error) => error,
    };
    if !device.gxm.borrow().texture_allocation_failed() { return Err(error); }
    device.call(|g| g.reclaim_retired(true)).context("recovering native texture allocation")?;
    match device.call(&mut allocate) {
        Ok(id) => return Ok(id),
        Err(next) => error = next,
    }
    if !device.gxm.borrow().texture_allocation_failed() { return Err(error); }
    let mut attempts = 2;
    // A persistent driver failure must not churn the complete cache or stall
    // the main thread indefinitely. Ordinary successful allocations skip this.
    for victim in evictable_deferred(device, pinned).into_iter().take(8) {
        evict_deferred(device, &victim, pinned.len())?;
        device.call(|g| g.reclaim_retired(true)).context("retiring idle texture for allocation recovery")?;
        attempts += 1;
        match device.call(&mut allocate) {
            Ok(id) => return Ok(id),
            Err(next) => error = next,
        }
        if !device.gxm.borrow().texture_allocation_failed() { return Err(error); }
    }
    Err(error.context(format!("native texture allocation failed after {attempts} bounded attempts"))
        .context(TextureMemoryPressure))
}

fn layout_bytes(width: u32, height: u32, bpp: u32, stride: u32) -> Result<usize> {
    ensure!(
        width != 0 && height != 0,
        "texture dimensions must be nonzero"
    );
    let row = width
        .checked_mul(bpp)
        .context("texture row size overflow")?;
    ensure!(stride >= row, "texture stride is smaller than its row");
    let length = u64::from(height - 1) * u64::from(stride) + u64::from(row);
    Ok(usize::try_from(length).context("texture allocation exceeds address space")?)
}

/// Pin the complete command list before issuing any game render pass. The LRU
/// may evict idle atlas GPU storage even while its logical TexRef is still held.
/// Native destroy waits for outstanding sampling, and a later frame can reload.
fn prepare_deferred(device: &Device, commands: &[Command], admit: &mut FrameAdmission<'_>) -> Result<()> {
    let _prepare = Scope::new(Metric::DeferredPrepare);
    let mut required = Vec::new();
    let mut pinned = HashSet::new();
    let mut add = |texture: &Rc<NativeTexture>| {
        if texture.deferred.is_some() && pinned.insert(Rc::as_ptr(texture)) {
            required.push(texture.clone());
        }
    };
    for command in commands {
        match command {
            Command::Begin { target, .. } => add(target),
            Command::Draw { textures, .. } => {
                for texture in textures.iter().flatten() { add(texture); }
            }
            Command::Copy { src, dst, .. } => { add(src); add(dst); }
            Command::End | Command::GroupCut(_) => {},
        }
    }
    let mut required = required.into_iter().map(|texture| {
        Ok((texture.allocation_bytes()?, texture))
    }).collect::<Result<Vec<_>>>()?;
    let needed = required.iter().try_fold(0usize, |total, (allocation, _)| {
        total.checked_add(*allocation).context("deferred frame allocation overflow")
    })?;
    let budget = device.deferred_budget.get();
    ensure!(needed <= budget, "deferred texture budget exceeded by submitted frame: {needed} > {budget}");
    if required.is_empty() { return Ok(()); }
    let epoch = device.epoch.get().checked_add(1).context("deferred cache epoch overflow")?;
    device.epoch.set(epoch);
    #[cfg(target_os = "vita")]
    menu_draw_diagnostic::residency_stats(device);
    for (_, texture) in &required { texture.last_used.set(epoch); }
    if knife_perf::enabled() {
    for (allocation, texture) in &required {
        let gxt = matches!(texture.deferred.as_ref().map(|s| &s.kind), Some(DeferredKind::GxtAltered { .. }));
        let missing = texture.id.borrow().is_none();
        let metric = match (gxt, missing) { (true, true) => Metric::GxtMiss, (true, false) => Metric::GxtHit,
            (false, true) => Metric::ImageMiss, (false, false) => Metric::ImageHit };
        knife_perf::add(metric, *allocation as u64, 0);
    }
    }
    required.retain(|(_, texture)| texture.id.borrow().is_none());
    // Every decoded native image is uploaded once; there is no Alter source
    // plus destination peak and no framebuffer attached to a cached image.
    if required.len() > 1 {
        #[cfg(test)]
        device.preparation_sorts.set(device.preparation_sorts.get() + 1);
        required.sort_by_key(|(allocation, _)| std::cmp::Reverse(*allocation));
    }
    for (allocation, texture) in required {
        #[cfg(target_os = "vita")]
        let _item = deferred_metrics::Item::start();
        if texture.id.borrow().is_none() {
            let rebuild_started = std::time::Instant::now();
            let source = texture.deferred.as_ref().context("deferred source missing")?;
            #[cfg(target_os = "vita")]
            menu_draw_diagnostic::residency("RELOAD", &texture, pinned.len());
            match &source.kind {
            DeferredKind::GxtAltered { logical, info, extract } => {
                let id = source.bytes.with_bytes(Metric::GxtReload, &source.label, |bytes| {
                    let view = extract(bytes).context("reading reloaded GXT envelope")?;
                    ensure!(view.logical == *logical && view.physical == texture.size,
                        "deferred GXT envelope dimensions changed for {}", source.label);
                    let gxt = GxtTexture::parse(view.file).context("validating reloaded GXT")?;
                    ensure!(GxtDescriptor::from_texture(&gxt) == *info,
                        "deferred GXT descriptor changed for {}", source.label);
                    // Validate everything before reclaiming another texture.
                    admit(&mut || reserve_cdram(device, allocation, allocation, &pinned))
                        .with_context(|| format!("preparing GXT atlas {}", source.label))?;
                    let upload_start = std::time::Instant::now();
                    let uploaded = allocate_texture_with_recovery(device, &pinned,
                        |g| g.create_gxt_texture(view.file, false, true));
                    #[cfg(target_os = "vita")]
                    deferred_metrics::add(&deferred_metrics::UPLOAD_US, upload_start.elapsed());
                    uploaded.with_context(|| format!("uploading GXT atlas {}", source.label))
                })?;
                texture.commit_residency(id)?;
                let released = source.bytes.release_ready();
                device.encoded_bytes.set(device.encoded_bytes.get().saturating_sub(released));
                knife_perf::gauge_set(Gauge::EncodedBytes, device.encoded_bytes.get() as u64);
                #[cfg(target_os = "vita")]
                menu_draw_diagnostic::compressed_upload(&texture, *info);
            }
            DeferredKind::Alter { .. } | DeferredKind::Image { .. } => {
            let cached = match &source.kind {
                DeferredKind::Image { cached: Some(cached), .. } => cached(), _ => None,
            };
            let (width, height, pixels, srgb) = if let Some(cached) = cached { cached } else {
                source.bytes.with_bytes(Metric::ImageReload, &source.label, |bytes| {
                let decode = Scope::new(Metric::DeferredDecode);
                let result = match &source.kind {
                DeferredKind::Alter { decode, .. } => decode(bytes),
                DeferredKind::Image { decode, .. } => decode(bytes),
                _ => unreachable!(),
                };
                decode.finish(result.as_ref().map_or(0, |(_, _, pixels, _)| pixels.len() as u64), 0);
                result
            })
                .with_context(|| format!("decoding deferred atlas {}", source.label))?
            };
            ensure!([width, height] == texture.size, "deferred image dimensions changed for {}", source.label);
            let stride = width.checked_mul(4).context("deferred decode stride overflow")?;
            ensure!(pixels.len() == layout_bytes(width, height, 4, stride)?, "deferred RGBA byte length mismatch for {}", source.label);
            let _ = srgb; // Decoder metadata; native descriptors never apply gamma.
            admit(&mut || reserve_cdram(device, allocation, allocation, &pinned))
                .with_context(|| format!("preparing native atlas {}", source.label))?;
            let uploaded = make_texture_inner(&texture.device, texture.size, texture.size,
                Some(&pixels), TextureFormat::Rgba8, false, true, false, false, &pinned)
                .with_context(|| format!("uploading native atlas {}", source.label))?;
            drop(pixels);
            let id = uploaded.native.id.borrow_mut().take()
                .context("reloaded native image has no allocation")?;
            texture.commit_residency(id)?;
            }
            }
            texture.rebuild_us.set(rebuild_started.elapsed().as_micros().min(u64::MAX as u128) as u64);
        }
    }
    Ok(())
}

fn execute(device: &Device, commands: &[Command]) -> Result<()> {
    execute_with_admission(device, commands, &mut |reserve| reserve())
}

/// Frame submission breakdown: how much of render_us went into executing recorded
/// commands (uploads, passes, GPU waits) versus the present blit.
#[cfg(target_os = "vita")]
mod frame_metrics {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub static EXEC_US: AtomicU64 = AtomicU64::new(0);
    pub static PRESENT_US: AtomicU64 = AtomicU64::new(0);
    pub static FRAMES: AtomicU64 = AtomicU64::new(0);
    /// Quads recorded, to compare against the draw count (a draw per texture).
    pub static QUADS: AtomicU64 = AtomicU64::new(0);
    /// Quads bucketed by source texture pixel area, to size the atlas opportunity: a
    /// 960x540 layer fills a 1024x1024 page on its own and cannot share one.
    pub static SMALL_QUADS: AtomicU64 = AtomicU64::new(0);
    pub static MID_QUADS: AtomicU64 = AtomicU64::new(0);
    pub static LARGE_QUADS: AtomicU64 = AtomicU64::new(0);
    /// Passes recorded, and how many of them read a target written earlier in the same
    /// frame. That ratio is the payoff of a barrier system: only the latter need a Finish.
    pub static PASS_TOTAL: AtomicU64 = AtomicU64::new(0);
    pub static PASS_NEEDING: AtomicU64 = AtomicU64::new(0);
    pub fn add(counter: &AtomicU64, start: std::time::Instant) {
        counter.fetch_add(start.elapsed().as_micros() as u64, Ordering::Relaxed);
    }
    pub fn snapshot() -> (u64, u64, u64, u64, u64, u64) {
        (EXEC_US.load(Ordering::Relaxed), PRESENT_US.load(Ordering::Relaxed),
         FRAMES.load(Ordering::Relaxed), QUADS.load(Ordering::Relaxed),
         SMALL_QUADS.load(Ordering::Relaxed), MID_QUADS.load(Ordering::Relaxed))
    }
}

/// "needing:total": how many passes of this session would need a Finish under a minimal
/// barrier scheme, out of all passes recorded.
#[cfg(target_os = "vita")]
pub fn pass_need_summary() -> String {
    let needing = frame_metrics::PASS_NEEDING.load(std::sync::atomic::Ordering::Relaxed);
    let total = frame_metrics::PASS_TOTAL.load(std::sync::atomic::Ordering::Relaxed);
    format!("{needing}:{total}")
}

/// Count of quads drawn from large (>=512x512) textures, for the atlas decision.
#[cfg(target_os = "vita")]
pub fn large_quad_count() -> u64 {
    frame_metrics::LARGE_QUADS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Native textures carry no label, so target creation registers its address here.
#[cfg(target_os = "vita")]
mod target_labels {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    fn table() -> &'static Mutex<HashMap<usize, String>> {
        static TABLE: OnceLock<Mutex<HashMap<usize, String>>> = OnceLock::new();
        TABLE.get_or_init(|| Mutex::new(HashMap::new()))
    }
    pub fn register(address: usize, label: &str) {
        if let Ok(mut map) = table().lock() { map.insert(address, label.to_string()); }
    }
    pub fn get(address: usize) -> Option<String> {
        table().lock().ok().and_then(|map| map.get(&address).cloned())
    }
}

/// Per-pass GPU cost keyed by target label. Only for the performance log: a pass
/// is one Begin/End pair, and its cost is the CPU time blocked in GXM for it.
#[cfg(target_os = "vita")]
mod pass_metrics {
    use std::sync::Mutex;
    /// label, cumulative microseconds, passes, draws
    static TABLE: Mutex<Vec<(String, u64, u64, u64)>> = Mutex::new(Vec::new());
    fn entry<'a>(table: &'a mut Vec<(String, u64, u64, u64)>, label: &str)
        -> &'a mut (String, u64, u64, u64) {
        let index = match table.iter().position(|(name, ..)| name == label) {
            Some(index) => index,
            None => { table.push((label.to_string(), 0, 0, 0)); table.len() - 1 }
        };
        &mut table[index]
    }
    /// One recorded quad, so a pass that is expensive because it draws many quads can
    /// be told apart from one that is a single full-screen blit.
    pub fn count_draw(label: &str) {
        let Ok(mut table) = TABLE.lock() else { return };
        entry(&mut table, label).3 += 1;
    }
    /// One pass and the time the CPU blocked for it in GXM.
    pub fn record(label: &str, us: u64) {
        if us == 0 { return; }
        let Ok(mut table) = TABLE.lock() else { return };
        let row = entry(&mut table, label);
        row.1 += us;
        row.2 += 1;
    }
    /// "label:total_ms:passes:draws" for the most expensive labels.
    pub fn snapshot(top: usize) -> String {
        let Ok(table) = TABLE.lock() else { return String::new() };
        let mut rows = table.clone();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        rows.iter().take(top)
            .map(|(name, us, passes, draws)| {
                format!("{name}:{:.1}:{passes}:{draws}", *us as f64 / 1000.0)
            })
            .collect::<Vec<_>>().join(" ")
    }
}

fn scene_kind(target: &NativeTexture) -> u32 {
    #[cfg(target_os = "vita")]
    if let Some(label) = target_labels::get(target as *const NativeTexture as usize) {
        return match label.as_str() {
            "stage" | "stage ring" => 1, "group pixel" => 2, "group mask" => 3,
            "scene" | "canvas" | "canvas ring" => 5,
            _ if target.deferred.is_some() => 9,
            _ => 0,
        };
    }
    let _ = target;
    0
}

fn execute_with_admission(device: &Device, commands: &[Command], admit: &mut FrameAdmission<'_>) -> Result<()> {
    prepare_deferred(device, commands, admit).context(FramePreparationError)?;
    // Preparation may submit nested bake batches. Reserve this complete batch
    // afterwards, including the final display clear and six present vertices.
    let mut basic_quads = 2usize;
    let mut effect_vertices = 6usize;
    for command in commands {
        match command {
            Command::Begin { clear: Some(_), .. } | Command::Copy { .. } => {
                basic_quads = basic_quads.checked_add(1).context("basic batch size overflow")?;
            }
            Command::Draw { vertices, .. } => {
                effect_vertices = effect_vertices.checked_add(vertices.len())
                    .context("effect batch size overflow")?;
            }
            _ => {}
        }
    }
    device.call(|g| g.begin_batch(basic_quads, effect_vertices))?;
    let finish_flags = pass_finish_flags(commands);
    #[cfg(not(target_os = "vita"))]
    let _ = &finish_flags;
    let mut begin_index = 0usize;
    device.call(|g| {
        let mut open = false;
        #[cfg(target_os = "vita")]
        let mut diagnostic_target: Option<&NativeTexture> = None;
        #[cfg(target_os = "vita")]
        let mut last_pass_label: Option<String> = None;
        for (_command_index, command) in commands.iter().enumerate() {
            let outcome = (|| match command {
                Command::GroupCut(_) => Ok(()),
                Command::Begin { target, clear } => {
                    if open {
                        return Err("encoder attempted nested GXM render passes".into());
                    }
                    let id = target.resident_id().map_err(|e| e.to_string())?;
                    let mut reads = Vec::new();
                    let mut kind = scene_kind(target);
                    let mut seen = HashSet::new();
                    for next in &commands[_command_index + 1..] {
                        match next {
                            Command::End | Command::Begin { .. } | Command::Copy { .. } => break,
                            Command::GroupCut(_) => {},
                            Command::Draw { textures, shader, blend, .. } => {
                                if *shader == EffectShader::Alter { kind = 9; }
                                else if *blend == EffectBlend::MaskAlpha { kind = 4; }
                                for texture in textures.iter().flatten() {
                                    if seen.insert(Rc::as_ptr(texture)) {
                                        reads.push(texture.resident_id().map_err(|e| e.to_string())?);
                                    }
                                }
                            },
                        }
                    }
                    g.plan_scene(Some(&id), &reads, kind)?;
                    // Legacy mode drains here. Async mode supplies an explicit
                    // GPU scene dependency and preserves each arena until completion.
                    let wait_start = std::time::Instant::now();
                    let finish_before = { let _ = finish_flags.get(begin_index); true };
                    begin_index += 1;
                    let result = g.begin_target_flags(&id, *clear, finish_before);
                    #[cfg(target_os = "vita")]
                    if result.is_ok()
                        && let Some(name) = last_pass_label.as_deref()
                    {
                        pass_metrics::record(name, wait_start.elapsed().as_micros() as u64);
                    }
                    if result.is_ok() {
                        open = true;
                        #[cfg(target_os = "vita")]
                        { diagnostic_target = Some(target); }
                    }
                    result
                }
                Command::End => {
                    if !open {
                        return Err("encoder attempted to end a closed GXM pass".into());
                    }
                    // An end failure leaves native teardown responsible for partial state.
                    open = false;
                    #[cfg(target_os = "vita")]
                    let ending = diagnostic_target
                        .and_then(|target| target_labels::get(target as *const NativeTexture as usize));
                    #[cfg(target_os = "vita")]
                    let end_start = std::time::Instant::now();
                    let ended = g.end_target();
                    #[cfg(target_os = "vita")]
                    {
                        // EndScene submission time; later waits are CPU boundaries, not GPU time.
                        if let Some(name) = &ending {
                            pass_metrics::record(name, end_start.elapsed().as_micros() as u64);
                        }
                        last_pass_label = ending;
                        diagnostic_target = None;
                    }
                    ended
                }
                Command::Draw {
                    work,
                    vertices,
                    shader,
                    blend,
                    textures,
                    params,
                } => {
                    #[cfg(target_os = "vita")]
                    if let Some(name) = diagnostic_target.and_then(|target|
                        target_labels::get(target as *const NativeTexture as usize)) {
                        pass_metrics::count_draw(&name);
                    }
                    if !open {
                        return Err("encoder attempted a draw outside a GXM pass".into());
                    }
                    let mut ids = [None, None, None, None];
                    for (i, texture) in textures.iter().enumerate() {
                        if let Some(texture) = texture {
                            ids[i] = Some(texture.resident_id().map_err(|e| e.to_string())?);
                        }
                    }
                    #[cfg(target_os = "vita")]
                    let diagnostic_draw = menu_draw_diagnostic::draw_begin(device, _command_index,
                        vertices, *shader, *blend, textures, &ids, *params, diagnostic_target);
                    let result = g.draw_effect(
                        vertices,
                        *shader,
                        *blend,
                        std::array::from_fn(|i| ids[i].as_ref()),
                        *params,
                    );
                    if result.is_ok() {
                        executed_work::record_success(*work);
                    }
                    #[cfg(target_os = "vita")]
                    menu_draw_diagnostic::draw_end(diagnostic_draw, &result);
                    result
                }
                Command::Copy { src, dst, metric } => {
                    #[cfg(target_os = "vita")]
                    { last_pass_label = None; }
                    if open {
                        return Err("encoder attempted target copy inside a GXM pass".into());
                    }
                    let src_id = src.resident_id().map_err(|e| e.to_string())?;
                    let dst_id = dst.resident_id().map_err(|e| e.to_string())?;
                    let copy = Scope::new(*metric);
                    let kind = match metric { Metric::CopyPs => 6, Metric::CopyGroup => 7, _ => 4 };
                    g.plan_scene(Some(&dst_id), std::slice::from_ref(&src_id), kind)?;
                    let result = g.copy_texture(&src_id, &dst_id);
                    let bytes = u64::from(src.size[0]) * u64::from(src.size[1]) * u64::from(src.bpp);
                    #[cfg(target_os = "vita")]
                    if copy.active() {
                        let from = target_labels::get(Rc::as_ptr(src) as usize).unwrap_or_default();
                        let to = target_labels::get(Rc::as_ptr(dst) as usize).unwrap_or_default();
                        copy.finish_hot(&format!("{from} -> {to}"), if result.is_ok() { bytes } else { 0 }, 0);
                    }
                    result
                }
            })();
            if let Err(error) = outcome {
                if open {
                    if let Err(cleanup) = g.end_target() {
                        return Err(format!("{error}; ending failed pass: {cleanup}"));
                    }
                }
                return Err(error);
            }
        }
        if open {
            return Err("encoder ended with an open GXM pass".into());
        }
        Ok(())
    })
}

fn effect_shader(shade: Shade) -> EffectShader {
    match shade {
        Shade::Sprite => EffectShader::Sprite,
        Shade::Fill => EffectShader::Fill,
        Shade::Text => EffectShader::Text,
        Shade::RimText => EffectShader::RimText,
        Shade::Mono => EffectShader::Mono,
        Shade::Nega => EffectShader::Nega,
        Shade::NoAlpha => EffectShader::NoAlpha,
        Shade::ForceAlpha => EffectShader::ForceAlpha,
    }
}

fn effect_blend(blend: Blend) -> EffectBlend {
    match blend {
        Blend::Alpha => EffectBlend::Alpha,
        Blend::Add => EffectBlend::Add,
        Blend::Sub => EffectBlend::Sub,
        Blend::Multiply => EffectBlend::Multiply,
        Blend::Screen => EffectBlend::Screen,
        Blend::Opaque | Blend::Ps(_) => EffectBlend::Opaque,
        Blend::Over => EffectBlend::Over,
        Blend::Footprint => EffectBlend::Footprint,
        Blend::MaskAlpha => EffectBlend::MaskAlpha,
    }
}

#[cfg(test)]
fn quad_vertices(q: &Quad, size: [f32; 2], scale: [f32; 2]) -> Result<Vec<Vertex>> {
    quad_vertices_reusing(q, size, scale, Vec::new())
}

fn quad_vertices_reusing(q: &Quad, size: [f32; 2], scale: [f32; 2], reuse: Vec<Vertex>) -> Result<Vec<Vertex>> {
    let build = Scope::new(Metric::QuadBuild);
    let result = quad_vertices_inner(q,size,scale,reuse);
    build.finish(result.as_ref().map_or(0, |v| (v.len()*std::mem::size_of::<Vertex>()) as u64),0);
    result
}

fn quad_vertices_inner(q: &Quad, [tw, th]: [f32; 2], scale: [f32; 2], mut vertices: Vec<Vertex>) -> Result<Vec<Vertex>> {
    ensure!(
        q.pos
            .iter()
            .flatten()
            .chain(q.uv.iter().flatten())
            .chain(q.color.iter())
            .chain(q.haze.iter().flatten())
            .all(|v| v.is_finite()),
        "quad contains nonfinite coordinates or parameters"
    );
    if let Some(clip) = &q.clip {
        ensure!(
            clip.iter().flatten().all(|v| v.is_finite()),
            "nonfinite clipping polygon"
        );
    }
    let soft = q.soft.map_or([[0.0; 4]; 3], |m| m.vertex_data(scale));
    ensure!(
        soft.iter().flatten().all(|v| v.is_finite()),
        "nonfinite soft clipping parameters"
    );
    if q.soft.is_some() {
        ensure!(
            soft[2][1] >= 0.0
                && soft[2][1] < SOFT_LUT_ROWS as f32
                && soft[2][2] >= 0.0
                && soft[2][2] < SOFT_LUT_W as f32,
            "soft clipping LUT coordinates are out of bounds"
        );
    }
    let corners: [[f32; 4]; 4] = core::array::from_fn(|i|
        [q.pos[i][0], q.pos[i][1], q.uv[i][0], q.uv[i][1]]);
    let clipped;
    let poly: &[[f32; 4]] = if let Some(clip) = &q.clip {
        clipped = clip_poly(corners.to_vec(), clip);
        &clipped
    } else { &corners };
    let mk = |p: [f32; 4]| Vertex {
        pos: [p[0] / tw * 2.0 - 1.0, 1.0 - p[1] / th * 2.0],
        uv: [p[2], p[3]],
        color: q.color,
        haze: q.haze,
        soft,
    };
    vertices.clear();
    if knife_perf::optimizations::enabled(knife_perf::optimizations::SPARSE_COVERAGE)
        && q.tex.sparse_coverage.is_some()
    {
        if let Some(rectangles) = sparse_coverage::rectangles(q) {
            vertices.try_reserve(rectangles.len() * 6)
                .context("reserving sparse quad vertices")?;
            let [w, h] = sparse_coverage::IMG0436_PHYSICAL.map(|v| v as f32);
            for &[x0, y0, x1, y1] in rectangles {
                let uv = [[f32::from(x0) / w, f32::from(y0) / h],
                    [f32::from(x1) / w, f32::from(y0) / h],
                    [f32::from(x1) / w, f32::from(y1) / h],
                    [f32::from(x0) / w, f32::from(y1) / h]];
                let p = uv.map(|uv| mk(sparse_coverage::point(q, uv)));
                vertices.extend_from_slice(&[p[0], p[1], p[2], p[0], p[2], p[3]]);
            }
            ensure!(vertices.iter().all(|v| v.pos.iter().chain(v.uv.iter()).all(|x| x.is_finite())),
                "sparse interpolation overflowed its native coordinates");
            knife_perf::add(Metric::SparseCoverageHit,
                (vertices.len() * std::mem::size_of::<Vertex>()) as u64, vertices.len() as u64);
            knife_perf::add(Metric::SparseCoverageTiles, 0, rectangles.len() as u64);
            return Ok(vertices);
        }
        knife_perf::add(Metric::SparseCoverageFallback, 0, 1);
    }
    vertices
        .try_reserve(poly.len().saturating_sub(2) * 3)
        .context("reserving clipped quad vertices")?;
    for k in 1..poly.len().saturating_sub(1) {
        vertices.extend_from_slice(&[mk(poly[0]), mk(poly[k]), mk(poly[k + 1])]);
    }
    ensure!(
        vertices
            .iter()
            .all(|v| v.pos.iter().chain(v.uv.iter()).all(|x| x.is_finite())),
        "quad interpolation overflowed its native coordinates"
    );
    Ok(vertices)
}

/// Sutherland-Hodgman with all four carried coordinates interpolated at crossings.
fn clip_poly(subject: Vec<[f32; 4]>, clip: &[[f32; 2]; 4]) -> Vec<[f32; 4]> {
    let area: f32 = (0..4)
        .map(|i| {
            let (a, b) = (clip[i], clip[(i + 1) % 4]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum();
    let sign = if area >= 0.0 { 1.0 } else { -1.0 };
    let mut out = subject;
    for i in 0..4 {
        if out.is_empty() {
            break;
        }
        let (a, b) = (clip[i], clip[(i + 1) % 4]);
        let side =
            |p: &[f32; 4]| sign * ((b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]));
        let input = std::mem::take(&mut out);
        for k in 0..input.len() {
            let cur = input[k];
            let prev = input[(k + input.len() - 1) % input.len()];
            let (dc, dp) = (side(&cur), side(&prev));
            if dc >= 0.0 {
                if dp < 0.0 {
                    let t = dp / (dp - dc);
                    out.push(std::array::from_fn(|j| prev[j] + (cur[j] - prev[j]) * t));
                }
                out.push(cur);
            } else if dp >= 0.0 {
                let t = dp / (dp - dc);
                out.push(std::array::from_fn(|j| prev[j] + (cur[j] - prev[j]) * t));
            }
        }
    }
    out
}

/// Native asset resolution is fixed in code. The 540p game build renders
/// every full scene and composite at 960x540 in 1920x1080 script coordinates.
/// No runtime density state or configuration file can lower these targets.
pub const fn scene_divisor() -> u32 { 1 }

fn composite_physical(width: u32, height: u32) -> [u32; 2] {
    [width.div_ceil(2), height.div_ceil(2)]
}

fn scene_size(width: u32, height: u32) -> [u32; 2] {
    // RENDER_W is selected by the asset profile at compile time, never by a
    // runtime setting. The release 540p profile has a fixed divisor of two.
    let divisor = CANVAS_W / RENDER_W;
    [width.div_ceil(divisor).max(1), height.div_ceil(divisor).max(1)]
}

fn group_owner_coverage(encoder: &CommandEncoder, pixel: &RenderTarget, mask: &RenderTarget,
    requested: CoverageBound) -> CoverageBound
{
    if !knife_perf::optimizations::enabled(knife_perf::optimizations::GROUP_BOUNDS) { return CoverageBound::Full; }
    let owner_bound = coverage::intersection(pixel.tex.native.coverage_for(encoder.coverage_id),
        mask.tex.native.coverage_for(encoder.coverage_id));
    // Full is the legacy caller's request, so it automatically gets the known
    // producer coverage. Explicit Unknown always retains its full fallback.
    match requested {
        CoverageBound::Full => owner_bound,
        CoverageBound::Unknown => CoverageBound::Unknown,
        _ if matches!(owner_bound, CoverageBound::Unknown) => CoverageBound::Unknown,
        _ => coverage::intersection(requested, owner_bound),
    }
}

fn group_composite_coverage(requested: CoverageBound, blend: Blend) -> CoverageBound {
    // Bounds describe the producer's nonzero source footprint. Opaque writes
    // transparent black outside it; MaskAlpha clears destination alpha there.
    // PS modes have their own opacity/alpha semantics, so keep their complete
    // original write. This applies to Empty as well as Rect.
    let neutral_transparent_source = matches!(blend,
        Blend::Alpha | Blend::Add | Blend::Sub | Blend::Multiply | Blend::Screen
        | Blend::Over | Blend::Footprint);
    if knife_perf::optimizations::enabled(knife_perf::optimizations::GROUP_BOUNDS)
        && neutral_transparent_source && !matches!(requested, CoverageBound::Empty) {
        requested
    } else {
        // An elided draw has no retained fallback command to restore if a source
        // is mutated after recording. Keep Empty's complete final write for now.
        CoverageBound::Full
    }
}

/// Build the final composite geometry with the original canvas-to-source UV
/// mapping. Invalid or degenerate rectangles deliberately use the full geometry
/// so a bad optional bound cannot erase a group's output.
fn group_composite_vertices(
    coverage: CoverageBound,
    physical: [u32; 2],
    color: [f32; 4],
) -> Option<Vec<Vertex>> {
    let CoverageBound::Rect { x, y, width, height } = coverage else {
        return (!matches!(coverage, CoverageBound::Empty))
            .then(|| fullscreen_verts(color).to_vec());
    };
    let [canvas_w, canvas_h] = physical;
    let Some(x_end) = x.checked_add(width) else {
        return Some(fullscreen_verts(color).to_vec());
    };
    let Some(y_end) = y.checked_add(height) else {
        return Some(fullscreen_verts(color).to_vec());
    };
    if canvas_w == 0 || canvas_h == 0 || x >= canvas_w || y >= canvas_h {
        return Some(fullscreen_verts(color).to_vec());
    }
    let x_end = x_end.min(canvas_w);
    let y_end = y_end.min(canvas_h);
    if x >= x_end || y >= y_end {
        return Some(fullscreen_verts(color).to_vec());
    }
    let x0 = (x as f32 / canvas_w as f32) * 2.0 - 1.0;
    let x1 = (x_end as f32 / canvas_w as f32) * 2.0 - 1.0;
    let y0 = 1.0 - (y as f32 / canvas_h as f32) * 2.0;
    let y1 = 1.0 - (y_end as f32 / canvas_h as f32) * 2.0;
    let u0 = x as f32 / canvas_w as f32;
    let u1 = x_end as f32 / canvas_w as f32;
    let v0 = y as f32 / canvas_h as f32;
    let v1 = y_end as f32 / canvas_h as f32;
    let mut vertices = fullscreen_verts(color).to_vec();
    for vertex in &mut vertices {
        let left = vertex.pos[0] < 0.0;
        let top = vertex.pos[1] > 0.0;
        vertex.pos[0] = if left { x0 } else { x1 };
        vertex.pos[1] = if top { y0 } else { y1 };
        // Preserve the source's full-canvas mapping: this destination region
        // samples its corresponding source rectangle instead of stretching
        // the entire source into the clipped write area.
        vertex.uv = [if left { u0 } else { u1 }, if top { v0 } else { v1 }];
    }
    Some(vertices)
}

fn group_composite_quad(
    tex: TexRef,
    logical_w: u32,
    logical_h: u32,
    physical: [u32; 2],
    coverage: CoverageBound,
    blend: Blend,
    alpha: f32,
) -> Quad {
    let [pw, ph] = physical;
    let (x, y, w, h, u0, v0, u1, v1) = match coverage {
        CoverageBound::Rect { x, y, width, height }
            if pw != 0 && ph != 0 && width > 0 && height > 0 && x < pw && y < ph
                && x.checked_add(width).is_some() && y.checked_add(height).is_some() => {
                let x1 = x.checked_add(width).unwrap().min(pw);
                let y1 = y.checked_add(height).unwrap().min(ph);
                if x1 <= x || y1 <= y {
                    (0.0, 0.0, logical_w as f32, logical_h as f32, 0.0, 0.0, 1.0, 1.0)
                } else {
                    (x as f32 * logical_w as f32 / pw as f32,
                     y as f32 * logical_h as f32 / ph as f32,
                     (x1 - x) as f32 * logical_w as f32 / pw as f32,
                     (y1 - y) as f32 * logical_h as f32 / ph as f32,
                     x as f32 / pw as f32, y as f32 / ph as f32,
                     x1 as f32 / pw as f32, y1 as f32 / ph as f32)
                }
            }
        _ => (0.0, 0.0, logical_w as f32, logical_h as f32, 0.0, 0.0, 1.0, 1.0),
    };
    let mut quad = Quad::rect(tex, [x, y, w, h], [1.0, 1.0, 1.0, alpha])
        .with_uv_rect(u0, v0, u1, v1);
    quad.blend = blend;
    quad
}

fn fullscreen_verts(color: [f32; 4]) -> [Vertex; 6] {
    let v = |x, y, u, w| Vertex {
        pos: [x, y],
        uv: [u, w],
        color,
        haze: [[0.0; 4]; 2],
        soft: [[0.0; 4]; 3],
    };
    [
        v(-1.0, 1.0, 0.0, 0.0),
        v(1.0, 1.0, 1.0, 0.0),
        v(1.0, -1.0, 1.0, 1.0),
        v(-1.0, 1.0, 0.0, 0.0),
        v(1.0, -1.0, 1.0, 1.0),
        v(-1.0, -1.0, 0.0, 1.0),
    ]
}

#[cfg(all(test, not(target_os = "vita")))]
#[path = "vita_test_backend.rs"]
mod test_backend;

#[cfg(all(test, not(target_os = "vita")))]
#[path = "vita_tests.rs"]
mod tests;
#[cfg(all(test, not(target_os = "vita")))]
#[path = "vita_group_bounds_tests.rs"]
mod group_bounds_tests;
#[cfg(all(test, not(target_os = "vita")))]
#[path = "vita_blur_weights_tests.rs"]
mod blur_weights_tests;
#[cfg(all(test, not(target_os = "vita")))]
#[path = "vita_work_tests.rs"]
mod work_tests;

/// Actual fixed physical scene and composite dimensions used by the renderer.
#[cfg(any(target_os = "vita", test))]
pub fn performance_dimensions() -> ([u32; 2], [u32; 2]) {
    (scene_size(CANVAS_W, CANVAS_H), composite_physical(CANVAS_W, CANVAS_H))
}

#[cfg(all(test, not(target_os = "vita")))]
#[path = "vita_full_ps_tests.rs"]
mod full_ps_tests;

#[cfg(all(test,not(target_os="vita")))]
#[path="vita_all_ps_tests.rs"]
mod all_ps_tests;
