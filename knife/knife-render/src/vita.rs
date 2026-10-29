//! Native GXM implementation of the renderer facade.
//!
//! Encoders retain resources and defer commands until submission, as on desktop.
//! Each quad is a separate draw because GXM binds the WGSL flat haze fields as
//! fragment uniforms. The native owner handles GPU fences and sRGB surfaces.

#![cfg_attr(not(target_os = "vita"), allow(dead_code))]

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

// The native 960x540 variant renders the scene/effect chain at the display's
// content size and the present pass places it inside the 960x544 scanout (2 px
// letterbox top and bottom). The 270p variant keeps the quarter-density chain so
// rebuilding KNIF00270 does not silently inherit native targets. Script
// coordinates remain the 1920x1080 logical canvas in both.
#[cfg(feature = "assets-540p")]
pub const RENDER_W: u32 = 960;
#[cfg(feature = "assets-540p")]
pub const RENDER_H: u32 = 540;
#[cfg(not(feature = "assets-540p"))]
pub const RENDER_W: u32 = 480;
#[cfg(not(feature = "assets-540p"))]
pub const RENDER_H: u32 = 270;

// Bound recorded CPU work independently of native texture/vertex ring budgets.
// Exceeding either limit is a reported error, never a partial frame submission.
const MAX_FRAME_COMMANDS: usize = 32_768;
const MAX_FRAME_VERTEX_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEFERRED_RETIREMENTS: usize = 256;
// Compressed atlas bytes stay in main RAM; only textures used by a submitted
// frame occupy the cache. Leave CDRAM for canvas, scene targets, fonts and baking.
const DEFERRED_TEXTURE_BYTES: usize = 32 * 1024 * 1024;
const DEFERRED_SOURCE_BYTES: usize = 32 * 1024 * 1024;
/// Upper bound on one merged draw's vertex count (16-bit indices into the ring).
const MERGED_DRAW_VERTICES: usize = 16_000;
const CDRAM_GRANULE: usize = 256 * 1024;

// A game-owned idle image cache may participate in the same native reservation.
// The callback is invoked for a validated cache miss, before its native upload.
type FrameAdmission<'a> = dyn FnMut(&mut dyn FnMut() -> Result<()>) -> Result<()> + 'a;

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
    static BAKES: AtomicU32 = AtomicU32::new(0);
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
                Command::Copy { src, dst } => { add(src); add(dst); }
                Command::Draw { textures, .. } => textures.iter().flatten().for_each(&mut add),
                Command::End => {}
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

    pub(super) fn bake_begin(texture: &NativeTexture, source: &NativeTexture,
        target: &NativeTexture, lut: &NativeTexture) -> Option<u32>
    {
        if !enabled() || !is_menu(texture) { return None; }
        let index = BAKES.fetch_add(1, Ordering::Relaxed);
        if index >= 2 { return None; }
        record(format_args!(
            "MENU_RENDER BAKE_BEGIN index={index} cache_epoch={} label={:?} physical={:?} source_id={:?} source_srgb={} target_id={:?} target_srgb={} target_rt={} lut_id={:?} shader=Alter blend=Opaque sampler_request=linear_clamp gamma_request=source_srgb destination=sRGB_RGBA8 native_memblock_uid=unobserved",
            texture.device.epoch.get(), texture.deferred.as_ref().map(|source| source.label.as_str()), texture.size, source.id.borrow(), source.srgb,
            target.id.borrow(), target.srgb, target.render_target, lut.id.borrow()));
        Some(index)
    }

    pub(super) fn bake_end(index: Option<u32>, result: &Result<()>, resident: &NativeTexture) {
        if let Some(index) = index {
            record(format_args!("MENU_RENDER BAKE_END index={index} result={result:?} resident_id={:?}",
                resident.id.borrow()));
        }
    }

    pub(super) fn compressed_upload(texture: &NativeTexture, info: GxtDescriptor) {
        if !enabled() || COMPRESSED_UPLOADS.fetch_add(1, Ordering::Relaxed) >= 6 { return; }
        record(format_args!(
            "GXT_UPLOAD label={} physical={:?} backing={:?} format={:?} payload_bytes={} cdram_bytes={:?} resident_id={:?} source_srgb=true sampler_request=linear_clamp runtime_decode=false runtime_alter=false",
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

/// The output curve of the original swapchain blit.
pub fn output_level(v: u8) -> u8 {
    output(f32::from(v) / 255.0)
}

/// Display level for an sRGB-encoded render target byte.
pub fn target_level(e: u8) -> u8 {
    let e = f32::from(e) / 255.0;
    output(if e <= 0.040_45 {
        e / 12.92
    } else {
        ((e + 0.055) / 1.055).powf(2.4)
    })
}

fn output(v: f32) -> u8 {
    let l = v.powf(2.2);
    let e = if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (e * 255.0).round().clamp(0.0, 255.0) as u8
}

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
}

pub type DecodeDeferred = fn(&[u8]) -> Result<(u32, u32, Vec<u8>, bool)>;
pub type DecodeImageReload = Rc<dyn Fn(&[u8]) -> Result<(u32, u32, Vec<u8>, bool)>>;

/// Borrow a complete GXT from its archive envelope without decoding pixels.
/// Layout dimensions belong to KIMG; physical dimensions belong to both KIMG
/// and GXT. Reloads must agree with the descriptor checked at registration.
pub struct DeferredGxt<'a> {
    pub logical: [u32; 2],
    pub physical: [u32; 2],
    pub file: &'a [u8],
}

pub type ExtractDeferredGxt = for<'a> fn(&'a [u8]) -> Result<DeferredGxt<'a>>;

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

    fn allocation_bytes(self) -> Result<usize> {
        Ok(self.payload_bytes.checked_add(CDRAM_GRANULE - 1)
            .context("compressed texture allocation overflow")? & !(CDRAM_GRANULE - 1))
    }
}

enum DeferredBytes {
    Retained(Arc<[u8]>),
    Reload {
        encoded_len: usize,
        load: Rc<dyn Fn() -> Result<Vec<u8>>>,
    },
}

impl DeferredBytes {
    fn encoded_len(&self) -> usize {
        match self {
            Self::Retained(bytes) => bytes.len(),
            Self::Reload { encoded_len, .. } => *encoded_len,
        }
    }

    fn retained_len(&self) -> usize {
        match self {
            Self::Retained(bytes) => bytes.len(),
            Self::Reload { .. } => 0,
        }
    }

    fn with_bytes<T>(&self, f: impl FnOnce(&[u8]) -> Result<T>) -> Result<T> {
        match self {
            Self::Retained(bytes) => f(bytes),
            Self::Reload { encoded_len, load } => {
                let bytes = load().context("reloading deferred image bytes")?;
                ensure!(bytes.len() == *encoded_len && bytes.len() <= DEFERRED_SOURCE_BYTES,
                    "deferred reload byte length changed: {} != {encoded_len}", bytes.len());
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
    Alter { decode: DecodeDeferred, soft_lut: Rc<NativeTexture> },
    Image { decode: DecodeImageReload, altered: bool, srgb: bool, soft_lut: Rc<NativeTexture> },
    GxtAltered { logical: [u32; 2], info: GxtDescriptor, extract: ExtractDeferredGxt },
}

impl NativeTexture {
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
        }
    }
}

pub struct GpuTexture {
    native: Rc<NativeTexture>,
    pub width: u32,
    pub height: u32,
}

pub type TexRef = Arc<GpuTexture>;

impl GpuTexture {
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

/// A pending frame. Dropping an unsubmitted encoder discards its commands.
/// Texture references stay alive until all commands have returned from GXM.
pub struct CommandEncoder {
    device: Rc<Device>,
    commands: Vec<Command>,
    vertex_bytes: usize,
    /// Target whose render pass is still open at the end of `commands`. Consecutive
    /// draws into the same target append to it instead of opening another pass.
    open: Option<Rc<NativeTexture>>,
}

/// Simulates a minimal barrier scheme: a pass needs a Finish only when it samples a
/// target that an earlier pass in this frame wrote. Returns (passes, passes_needing).
#[cfg(target_os = "vita")]
pub(crate) fn analyse_passes(commands: &[Command]) -> (u64, u64) {
    let flags = pass_finish_flags(commands);
    (flags.len() as u64, flags.iter().filter(|f| **f).count() as u64)
}

/// One flag per pass, in submission order: does this pass sample a target that an earlier
/// pass in the same frame wrote? A Finish is only needed for those.
pub(crate) fn pass_finish_flags(commands: &[Command]) -> Vec<bool> {
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

#[cfg(any(target_os = "vita", test))]
pub(crate) fn analyse_passes_unused(commands: &[Command]) -> (u64, u64) {
    let mut written: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let (mut total, mut needing) = (0u64, 0u64);
    let mut i = 0;
    while i < commands.len() {
        let Command::Begin { target, .. } = &commands[i] else { i += 1; continue };
        total += 1;
        let mut j = i + 1;
        let mut need = false;
        while j < commands.len() {
            match &commands[j] {
                Command::End => break,
                Command::Draw { textures, .. } => {
                    if textures.iter().flatten().any(|t| written.contains(&(Rc::as_ptr(t) as usize))) {
                        need = true;
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
            needing += 1;
            // A Finish drains everything submitted so far.
            written.clear();
        }
        written.insert(Rc::as_ptr(target) as usize);
        i = j;
    }
    (total, needing)
}


impl CommandEncoder {
    /// Close an open pass. Every command that is not a draw into the same target
    /// must call this first: GXM copies and passes cannot interleave.
    fn close_pass(&mut self) {
        if self.open.take().is_some() {
            self.commands.push(Command::End);
        }
    }
}

enum Command {
    Begin {
        target: Rc<NativeTexture>,
        clear: Option<[f32; 4]>,
    },
    End,
    Draw {
        vertices: Vec<Vertex>,
        shader: EffectShader,
        blend: EffectBlend,
        textures: [Option<Rc<NativeTexture>>; 4],
        params: [f32; 8],
    },
    Copy {
        src: Rc<NativeTexture>,
        dst: Rc<NativeTexture>,
    },
}

impl CommandEncoder {
    fn append(&mut self, mut commands: Vec<Command>) -> Result<()> {
        let command_count = self
            .commands
            .len()
            .checked_add(commands.len())
            .context("frame command count overflow")?;
        ensure!(
            command_count <= MAX_FRAME_COMMANDS,
            "frame exceeds {MAX_FRAME_COMMANDS} GXM commands"
        );
        let mut vertex_bytes = self.vertex_bytes;
        for command in &commands {
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
            .try_reserve(commands.len())
            .context("reserving frame command memory")?;
        // Coalesce a run of draws that share every bound state into one submission, the
        // way the reference Vita renderer batches quads: the driver cost is per draw, and
        // a heavy frame issues 60-95 of them.
        for command in commands {
            if let Command::Draw { vertices, shader, blend, textures, params } = &command
                && let Some(Command::Draw {
                    vertices: last_vertices,
                    shader: last_shader,
                    blend: last_blend,
                    textures: last_textures,
                    params: last_params,
                }) = self.commands.last_mut()
                && *last_shader == *shader
                && *last_blend == *blend
                && last_params == params
                // The shader reads haze of the first vertex as a uniform, so a merged run
                // must not cross a haze change.
                && last_vertices.first().map(|v| v.haze) == vertices.first().map(|v| v.haze)
                && last_vertices.len() + vertices.len() <= MERGED_DRAW_VERTICES
                && last_textures.iter().zip(textures.iter()).all(|(a, b)| match (a, b) {
                    (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                })
            {
                last_vertices.extend_from_slice(vertices);
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
    soft_lut: Rc<NativeTexture>,
    pub bc_supported: bool,
    pub white: TexRef,
    pub hidden: bool,
}

impl Renderer {
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
                deferred: None, last_used: Cell::new(0),
            }),
            width, height,
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
        self.device.call(|g| unsafe { g.rebind_movie_texture(&id, frame, width, height) })
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
            .with_context(|| format!("uploading baked {label}"))?;
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
        let (length, bpp) = match &data {
            Upload::Rgba8(bytes) => (bytes.len(), 4),
            Upload::R8(bytes) => (bytes.len(), 1),
            Upload::Bc1(_) | Upload::Bc3(_) | Upload::Bc7(_) =>
                bail!("raw BC alteration uploads are unsupported on Vita; supply decoded pixels"),
        };
        let stride = width.checked_mul(bpp).context("alteration source row size overflow")?;
        ensure!(length >= layout_bytes(width, height, bpp, stride)?,
            "alteration source is truncated");
        // Both textures are live during Alter. Admit their combined native
        // allocations before creating either, including R8's smaller source.
        self.try_reserve_image_upload(width, height, bpp, true)
            .with_context(|| format!("reserving alteration peak for {label}"))?;
        let source = self
            .upload_raw(width, height, data, srgb)
            .with_context(|| format!("uploading alteration source {label}"))?;
        let target = self.try_create_target(width, height, label)?;
        let commands = vec![
            Command::Begin {
                target: target.tex.native.clone(),
                clear: Some([0.0; 4]),
            },
            Command::Draw {
                vertices: fullscreen_verts([1.0; 4]).to_vec(),
                shader: EffectShader::Alter,
                blend: EffectBlend::Opaque,
                textures: [
                    Some(source.native.clone()),
                    None,
                    None,
                    Some(self.soft_lut.clone()),
                ],
                params: [0.0; 8],
            },
            Command::End,
        ];
        // Desktop upload_altered submits its own pass immediately, independently
        // of any frame encoder that the caller is still recording.
        execute(&self.device, &commands)
            .with_context(|| format!("baking altered texture {label}"))?;
        Ok(target.tex)
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
        altered: bool,
    ) -> Result<()> {
        let source = native_allocation_bytes([width, height], bpp)?;
        let peak = if altered {
            source.checked_add(native_allocation_bytes([width, height], 4)?)
                .context("image alteration peak overflow")?
        } else { source };
        reserve_cdram(&self.device, peak, 0, &HashSet::new())
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
        ensure!(self.owns_texture(&uploaded) && uploaded.native.deferred.is_none(),
            "CG adoption requires an ordinary texture from this renderer");
        ensure!(Arc::strong_count(&uploaded) == 1 && Rc::strong_count(&uploaded.native) == 1,
            "CG adoption must precede publishing the texture or recording draws");
        ensure!(uploaded.native.bpp == 4, "CG reload requires RGBA8 pixels");
        let replacement = self.defer_source([uploaded.width, uploaded.height], uploaded.native.size,
            DeferredBytes::Reload { encoded_len, load }, label,
            DeferredKind::Image { decode, altered, srgb: uploaded.native.srgb,
                soft_lut: self.soft_lut.clone() })?;
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
        let view = extract(encoded).with_context(|| format!("reading GXT envelope {label}"))?;
        ensure!(view.logical == logical && view.physical == physical,
            "GXT envelope dimensions disagree for {label}");
        let gxt = GxtTexture::parse(view.file).with_context(|| format!("validating GXT {label}"))?;
        let info = GxtDescriptor::from_texture(&gxt);
        ensure!(info.physical == physical, "GXT physical dimensions disagree for {label}");
        self.defer_source(logical, physical, DeferredBytes::Reload { encoded_len: encoded.len(), load },
            label, DeferredKind::GxtAltered { logical, info, extract })
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
            DeferredKind::Alter { decode, soft_lut: self.soft_lut.clone() })
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
        // Two bilingual source strips are 960x1080; other compact atlases fit
        // within 960x540. This is an input extent bound, not a display size.
        let max_size = if matches!(&kind, DeferredKind::Image { .. }) { [2048, 2048] } else { [960, 1080] };
        ensure!(physical[0] != 0 && physical[1] != 0
            && physical[0] <= max_size[0] && physical[1] <= max_size[1], "deferred atlas dimensions exceed compact bounds");
        ensure!(bytes.encoded_len() != 0, "deferred image has no encoded bytes");
        ensure!(bytes.encoded_len() <= DEFERRED_SOURCE_BYTES, "deferred image exceeds encoded entry bound");
        let total = self.device.encoded_bytes.get().checked_add(bytes.retained_len())
            .context("deferred source accounting overflow")?;
        ensure!(total <= DEFERRED_SOURCE_BYTES, "deferred encoded source budget exceeded");
        let native = Rc::new(NativeTexture {
            device: self.device.clone(), id: RefCell::new(None), size: physical,
            bpp: if matches!(&kind, DeferredKind::GxtAltered { .. }) { 0 } else { 4 },
            srgb: match &kind { DeferredKind::Image { srgb, .. } => *srgb, _ => true }, render_target: false,
            deferred: Some(DeferredSource { bytes, label: label.into(), kind }),
            last_used: Cell::new(0),
        });
        self.device.encoded_bytes.set(total);
        let mut cache = self.device.deferred.borrow_mut();
        cache.retain(|weak| weak.strong_count() != 0);
        cache.push(Rc::downgrade(&native));
        Ok(Arc::new(GpuTexture { native, width: logical[0], height: logical[1] }))
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
        self.device.call(|g| g.update_texture(&id, [x, y], [w, h], data, stride))
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

    /// The UI composite target is the display's 960x540 content size, i.e. half of
    /// the 1920x1080 logical canvas. With the scene chain also at 960x540 the
    /// scene-to-canvas blit is 1:1. Script coordinates remain unchanged.
    pub fn create_composite_target(&self, width: u32, height: u32, label: &str) -> RenderTarget {
        self.create_target_sized([width, height], composite_physical(width, height), label)
            .expect("creating Vita UI composite target")
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
        .with_context(|| format!("creating render target {label}"))?;
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
            target.tex.native.bpp == 4 && target.tex.native.srgb,
            "render targets must store sRGB RGBA8 pixels"
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

    pub fn try_draw(
        &mut self,
        encoder: &mut CommandEncoder,
        target: &RenderTarget,
        clear: Option<[f32; 4]>,
        quads: &[Quad],
    ) -> Result<()> {
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
        let has_ps = quads.iter().any(|q| matches!(q.blend, Blend::Ps(_)));
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
        for q in quads {
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
            // colour term is scaled by alpha and the target alpha is left or added to. The
            // reference renderer culls the same case from its upload-time alpha tiles.
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
            let vertices = quad_vertices(q, [tw, th], scale)?;
            #[cfg(target_os = "vita")]
            menu_draw_diagnostic::quad(q, target, &vertices, scale);
            if let Blend::Ps(op) = q.blend {
                // A fully clipped PS quad must not snapshot or open a pass.
                if vertices.is_empty() {
                    continue;
                }
                if open {
                    commands.push(Command::End);
                    open = false;
                }
                let bd = backdrop.as_ref().expect("prepared Ps backdrop");
                commands.push(Command::Copy {
                    src: target.tex.native.clone(),
                    dst: bd.clone(),
                });
                commands.push(Command::Begin {
                    target: target.tex.native.clone(),
                    clear: None,
                });
                let (rule, progress, flags) = match &q.rule {
                    Some(mask) => (mask.tex.native.clone(), mask.progress, mask.flags()),
                    None => (self.white.native.clone(), -1.0, 0.0),
                };
                commands.push(Command::Draw {
                    vertices,
                    shader: EffectShader::Ps,
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
                let (shader, rule, params) = if q.masked {
                    // Mask arrives as tex2 and is sampled in canvas space by the shader.
                    match &q.rule {
                        Some(mask) => (EffectShader::SpriteMask, Some(mask.tex.native.clone()),
                            [0.0, 0.0, pw as f32, ph as f32, 0.0, 0.0, 0.0, 0.0]),
                        None => (effect_shader(q.shade), None, [0.0; 8]),
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
                    None => (effect_shader(q.shade), None, [0.0; 8]),
                } };
                commands.push(Command::Draw {
                    vertices,
                    shader,
                    blend: effect_blend(q.blend),
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
        self.check_encoder(encoder)?;
        encoder.close_pass();
        ensure!(alpha.is_finite(), "nonfinite group opacity");
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
        self.try_draw(encoder, scratch, None, &[
            Quad { blend: Blend::MaskAlpha, ..Quad::rect(mask.tex.clone(), full, [1.0; 4]) },
        ])?;
        self.try_draw(encoder, target, None, &[
            Quad { blend, ..Quad::rect(scratch.tex.clone(), full, [1.0, 1.0, 1.0, alpha]) },
        ])
    }

    /// Experimental DEV candidate, not used by the game: the displayed
    /// old/fused differential has not passed exact pixel comparison.
    /// Full-canvas layer-group composition. Pixel/mask surfaces retain their
    /// cross-frame state; alpha multiplication is fused into the final sample.
    /// Ps still needs the same per-draw destination snapshot as ordinary quads.
    pub fn group_composite(&mut self, encoder: &mut CommandEncoder, target: &RenderTarget,
        pixel: &RenderTarget, mask: &RenderTarget, blend: Blend, alpha: f32)
    {
        self.try_group_composite(encoder, target, pixel, mask, blend, alpha)
            .expect("recording Vita layer-group composite");
    }

    pub fn try_group_composite(&mut self, encoder: &mut CommandEncoder, target: &RenderTarget,
        pixel: &RenderTarget, mask: &RenderTarget, blend: Blend, alpha: f32) -> Result<()>
    {
        self.check_encoder(encoder)?;
        self.check_target(target)?;
        encoder.close_pass();
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
        let mut commands = Vec::with_capacity(4);
        let (shader, native_blend, second, third, params) = if let Blend::Ps(op) = blend {
            if !self.backdrops.contains_key(&physical) {
                let bd = self.create_target_sized(logical, physical, "backdrop")?;
                self.backdrops.insert(physical, bd.tex.native.clone());
            }
            let bd = self.backdrops[&physical].clone();
            commands.push(Command::Copy { src: target.tex.native.clone(), dst: bd.clone() });
            (EffectShader::Ps, EffectBlend::Opaque, bd, mask.tex.native.clone(),
                [op as u32 as f32, 0., physical[0] as f32, physical[1] as f32, -1., 0., 0., 1.])
        } else {
            (EffectShader::GroupComposite, effect_blend(blend), mask.tex.native.clone(),
                self.white.native.clone(), [0.; 8])
        };
        commands.push(Command::Begin { target: target.tex.native.clone(), clear: None });
        commands.push(Command::Draw {
            vertices: fullscreen_verts([1., 1., 1., alpha]).to_vec(), shader, blend: native_blend,
            textures: [Some(pixel.tex.native.clone()), Some(second), Some(third), Some(self.soft_lut.clone())],
            params,
        });
        commands.push(Command::End);
        encoder.append(commands)
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
        encoder.append(vec![
            Command::Begin {
                target: dst.tex.native.clone(),
                clear: None,
            },
            Command::Draw {
                vertices: fullscreen_verts([1.0; 4]).to_vec(),
                shader: EffectShader::Blur,
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
            src: src.tex.native.clone(),
            dst: dst.tex.native.clone(),
        }])
    }

    pub fn encoder(&self) -> CommandEncoder {
        CommandEncoder {
            device: self.device.clone(),
            commands: Vec::new(),
            vertex_bytes: 0,
            open: None,
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
        if let Err(error) = execute_with_admission(&self.device, &encoder.commands, &mut admit) {
            #[cfg(target_os = "vita")]
            menu_draw_diagnostic::frame_pressure(&encoder.commands);
            return Err(error);
        }
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
    make_texture_inner(device, logical, physical, bytes, format, srgb, linear, render_target, true)
}

#[allow(clippy::too_many_arguments)]
fn make_texture_inner(
    device: &Rc<Device>,
    logical: [u32; 2],
    physical: [u32; 2],
    bytes: Option<&[u8]>,
    format: TextureFormat,
    srgb: bool,
    linear: bool,
    render_target: bool,
    reclaim: bool,
) -> Result<TexRef> {
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
        reserve_cdram(device, allocation, 0, &HashSet::new())?;
    }
    let id = device
        .call(|g| g.create_texture_ex(bytes, w, h, stride, format, srgb, linear, render_target))?;
    let native = Rc::new(NativeTexture {
            device: device.clone(),
            id: RefCell::new(Some(id)),
            size: physical,
            bpp,
            srgb,
            render_target,
            deferred: None,
            last_used: Cell::new(0),
        });
    #[cfg(target_os = "vita")]
    menu_draw_diagnostic::texture("CREATE", &native, "");
    Ok(Arc::new(GpuTexture {
        native,
        width: logical[0],
        height: logical[1],
    }))
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
    let bytes = usize::try_from(bytes).context("native texture allocation exceeds address space")?;
    Ok(bytes.checked_add(CDRAM_GRANULE - 1).context("native texture allocation overflow")?
        & !(CDRAM_GRANULE - 1))
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
    let budget = device.deferred_budget.get();
    let fits_cache = || -> Result<bool> {
        Ok(device.resident_deferred_bytes.get().checked_add(cache_growth)
            .context("deferred cache size overflow")? <= budget)
    };
    let mut available = device.call(|g| g.available_cdram()).context("querying native CDRAM allowance")?;
    if fits_cache()? && available >= peak { return Ok(()); }
    // Build and sort victims only under actual pressure. A scene with enough
    // headroom must not repeatedly walk every registered logical texture.
    #[cfg(test)]
    device.admission_scans.set(device.admission_scans.get() + 1);
    let mut victims: Vec<_> = device.deferred.borrow().iter().filter_map(Weak::upgrade)
        .filter(|texture| !pinned.contains(&Rc::as_ptr(texture)) && texture.id.borrow().is_some()).collect();
    victims.sort_by_key(|texture| texture.last_used.get());
    let mut victims = victims.into_iter();
    loop {
        let fits_cache = fits_cache()?;
        if fits_cache && available >= peak { return Ok(()); }
        let Some(victim) = victims.next() else {
            if !fits_cache {
                bail!("deferred texture budget has no evictable atlas: {} + {cache_growth} > {budget}",
                    device.resident_deferred_bytes.get());
            }
            bail!("native CDRAM headroom has no evictable atlas: bake/allocation peak {peak} > available {available}");
        };
        let id = victim.id.borrow_mut().take();
        if let Some(id) = id {
            #[cfg(target_os = "vita")]
            menu_draw_diagnostic::residency("EVICT", &victim, pinned.len());
            device.retire(&id);
            // An uncertain native retirement poisons subsequent allocation;
            // never pretend that a failed release recovered this memory.
            device.drain()?;
            victim.release_resident_charge()?;
            available = device.call(|g| g.available_cdram()).context("querying native CDRAM allowance")?;
        }
    }
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
            Command::Copy { src, dst } => { add(src); add(dst); }
            Command::End => {},
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
    required.retain(|(_, texture)| texture.id.borrow().is_none());
    // An RGBA Alter bake holds one temporary source; a pre-altered GXT holds
    // only its final allocation. Schedule by temporary headroom first so a
    // large compressed image cannot consume a later bake's working space.
    // Draw order and the complete-frame pins remain unchanged.
    if required.len() > 1 {
        #[cfg(test)]
        device.preparation_sorts.set(device.preparation_sorts.get() + 1);
        required.sort_by_key(|(allocation, texture)| {
            let temporary = if matches!(texture.deferred.as_ref().map(|s| &s.kind),
                Some(DeferredKind::Alter { .. } | DeferredKind::Image { altered: true, .. })) { *allocation } else { 0 };
            std::cmp::Reverse((temporary, *allocation))
        });
    }
    for (allocation, texture) in required {
        #[cfg(target_os = "vita")]
        let _item = deferred_metrics::Item::start();
        if texture.id.borrow().is_none() {
            let source = texture.deferred.as_ref().context("deferred source missing")?;
            #[cfg(target_os = "vita")]
            menu_draw_diagnostic::residency("RELOAD", &texture, pinned.len());
            match &source.kind {
            DeferredKind::GxtAltered { logical, info, extract } => {
                let id = source.bytes.with_bytes(|bytes| {
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
                    let uploaded = device.call(|g| g.create_gxt_texture(view.file, true, true));
                    #[cfg(target_os = "vita")]
                    deferred_metrics::add(&deferred_metrics::UPLOAD_US, upload_start.elapsed());
                    uploaded.with_context(|| format!("uploading GXT atlas {}", source.label))
                })?;
                texture.commit_residency(id)?;
                #[cfg(target_os = "vita")]
                menu_draw_diagnostic::compressed_upload(&texture, *info);
            }
            DeferredKind::Alter { .. } | DeferredKind::Image { .. } => {
            let (width, height, pixels, srgb) = source.bytes.with_bytes(|bytes| match &source.kind {
                DeferredKind::Alter { decode, .. } => decode(bytes),
                DeferredKind::Image { decode, .. } => decode(bytes),
                _ => unreachable!(),
            })
                .with_context(|| format!("decoding deferred atlas {}", source.label))?;
            ensure!([width, height] == texture.size, "deferred image dimensions changed for {}", source.label);
            let stride = width.checked_mul(4).context("deferred decode stride overflow")?;
            ensure!(pixels.len() == layout_bytes(width, height, 4, stride)?, "deferred RGBA byte length mismatch for {}", source.label);
            let (altered, soft_lut) = match &source.kind {
                DeferredKind::Alter { soft_lut, .. } => (true, soft_lut),
                DeferredKind::Image { altered, soft_lut, srgb: expected, .. } => {
                    ensure!(*altered || srgb == *expected, "CG reload colour space changed for {}", source.label);
                    (*altered, soft_lut)
                },
                _ => unreachable!(),
            };
            let peak = allocation.checked_mul(if altered { 2 } else { 1 }).context("deferred bake peak overflow")?;
            admit(&mut || reserve_cdram(device, peak, allocation, &pinned)).with_context(|| format!(
                "preparing deferred atlas {} ({}x{}, allocation {allocation}, frame total {needed})",
                source.label, width, height))?;
            // The preflight covers source and destination together. A second
            // reclaim with an empty pin set could evict this frame's textures.
            let source_texture = make_texture_inner(&texture.device, texture.size, texture.size,
                Some(&pixels), TextureFormat::Rgba8, srgb, true, false, false)
                .with_context(|| format!("uploading deferred atlas {}", source.label))?;
            // Release the decoded CPU allocation before submitting native work.
            drop(pixels);
            if !altered {
                let id = source_texture.native.id.borrow_mut().take().context("reloaded CG has no native allocation")?;
                texture.commit_residency(id)?;
                continue;
            }
            let target = make_texture_inner(&texture.device, texture.size, texture.size,
                None, TextureFormat::Rgba8, true, true, true, false)
                .with_context(|| format!("allocating deferred alteration {}", source.label))?;
            let bake = [
                Command::Begin { target: target.native.clone(), clear: Some([0.0; 4]) },
                Command::Draw {
                    vertices: fullscreen_verts([1.0; 4]).to_vec(), shader: EffectShader::Alter,
                    blend: EffectBlend::Opaque,
                    textures: [Some(source_texture.native.clone()), None, None, Some(soft_lut.clone())],
                    params: [0.0; 8],
                },
                Command::End,
            ];
            #[cfg(target_os = "vita")]
            let diagnostic_bake = menu_draw_diagnostic::bake_begin(&texture,
                &source_texture.native, &target.native, soft_lut);
            let bake_result = execute(&texture.device, &bake)
                .with_context(|| format!("baking deferred atlas {}", source.label));
            #[cfg(target_os = "vita")]
            if bake_result.is_err() {
                menu_draw_diagnostic::bake_end(diagnostic_bake, &bake_result, &texture);
            }
            bake_result?;
            // Cached UI/CG pixels remain sampled images. Drop their temporary
            // framebuffer attachments now so the cache cannot exhaust native
            // render-target objects independently of its pixel-byte budget.
            let baked_id = target.native.resident_id()?;
            texture.device.call(|g| g.seal_texture(&baked_id))
                .with_context(|| format!("sealing deferred atlas {}", source.label))?;
            // Commit only after both the pass and attachment teardown succeed.
            let id = target.native.id.borrow_mut().take().context("baked target has no native texture")?;
            texture.commit_residency(id)?;
            #[cfg(target_os = "vita")]
            menu_draw_diagnostic::bake_end(diagnostic_bake, &Ok(()), &texture);
            }
            }
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

fn execute_with_admission(device: &Device, commands: &[Command], admit: &mut FrameAdmission<'_>) -> Result<()> {
    prepare_deferred(device, commands, admit)?;
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
                Command::Begin { target, clear } => {
                    if open {
                        return Err("encoder attempted nested GXM render passes".into());
                    }
                    let id = target.resident_id().map_err(|e| e.to_string())?;
                    // kv_gxm_begin_target finishes all prior work, so this wait is the
                    // tail of the pass that just ended: attribute it there.
                    let wait_start = std::time::Instant::now();
                    // Barrier disabled: skipping the pass drain black-screened on hardware even
                    // with RAW and WAR modelled, so some dependency here is still unaccounted
                    // for. The flags are still computed and logged as pass_need.
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
                        // The Finish inside end_target is this pass's GPU cost.
                        if let Some(name) = &ending {
                            pass_metrics::record(name, end_start.elapsed().as_micros() as u64);
                        }
                        last_pass_label = ending;
                        diagnostic_target = None;
                    }
                    ended
                }
                Command::Draw {
                    vertices,
                    shader,
                    blend,
                    textures,
                    params,
                } => {
                    #[cfg(target_os = "vita")]
                    if let Some(name) = last_pass_label.as_deref() {
                        pass_metrics::count_draw(name);
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
                    #[cfg(target_os = "vita")]
                    menu_draw_diagnostic::draw_end(diagnostic_draw, &result);
                    result
                }
                Command::Copy { src, dst } => {
                    if open {
                        return Err("encoder attempted target copy inside a GXM pass".into());
                    }
                    let src_id = src.resident_id().map_err(|e| e.to_string())?;
                    let dst_id = dst.resident_id().map_err(|e| e.to_string())?;
                    g.copy_texture(&src_id, &dst_id)
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

fn quad_vertices(q: &Quad, [tw, th]: [f32; 2], scale: [f32; 2]) -> Result<Vec<Vertex>> {
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
    let corners = (0..4)
        .map(|i| [q.pos[i][0], q.pos[i][1], q.uv[i][0], q.uv[i][1]])
        .collect();
    let poly = match &q.clip {
        Some(clip) => clip_poly(corners, clip),
        None => corners,
    };
    let mk = |p: [f32; 4]| Vertex {
        pos: [p[0] / tw * 2.0 - 1.0, 1.0 - p[1] / th * 2.0],
        uv: [p[2], p[3]],
        color: q.color,
        haze: q.haze,
        soft,
    };
    let mut vertices = Vec::new();
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

/// Scene-chain density. 1 = native (960x540 with the 540p assets), 2 = half
/// (480x270, upscaled into the same 960x540 composite). The GPU fill rate is the
/// binding constraint on hardware, so this stays switchable at runtime instead of
/// requiring a rebuild to A/B a scene.
/// The renderer itself starts native; the Vita entry point selects the shipped
/// default (half) so tests and other users of this crate keep native coordinates.
static SCENE_DIVISOR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);


pub fn set_scene_divisor(divisor: u32) {
    SCENE_DIVISOR.store(divisor.clamp(1, 4), std::sync::atomic::Ordering::Relaxed);
}

pub fn scene_divisor() -> u32 {
    SCENE_DIVISOR.load(std::sync::atomic::Ordering::Relaxed)
}

/// Canvas/composite scale as a fraction (num/den). 1/1 keeps the native 960x540
/// composite; 3/8 renders the whole composite at 720x405 and lets the present blit
/// upscale it, which cuts the biggest single pass (measured 24-28 ms) by ~40%.
/// Packed (numerator << 16) | denominator; 1/1 is the native composite.
static CANVAS_SCALE: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new((1 << 16) | 1);

pub fn set_canvas_scale(numerator: u32, denominator: u32) {
    let denominator = denominator.max(1);
    let packed = (numerator.max(1) << 16) | denominator;
    CANVAS_SCALE.store(packed, std::sync::atomic::Ordering::Relaxed);
}

pub fn canvas_scale() -> (u32, u32) {
    let packed = CANVAS_SCALE.load(std::sync::atomic::Ordering::Relaxed);
    ((packed >> 16).max(1), (packed & 0xFFFF).max(1))
}

/// Physical composite size for a logical 1920x1080 canvas at the selected scale.
fn composite_physical(width: u32, height: u32) -> [u32; 2] {
    let (num, den) = canvas_scale();
    if (num, den) == (1, 1) {
        return [width.div_ceil(2), height.div_ceil(2)];
    }
    [
        (width * num / den).max(2) & !1,
        (height * num / den).max(2) & !1,
    ]
}

fn scene_size(width: u32, height: u32) -> [u32; 2] {
    // The scene chain follows the divisor only: the composite scale is independent, so a
    // cheaper canvas does not force the whole chain back up in resolution.
    let base = CANVAS_W / RENDER_W;
    let divisor = base * scene_divisor();
    [
        width.div_ceil(divisor).max(1),
        height.div_ceil(divisor).max(1),
    ]
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
