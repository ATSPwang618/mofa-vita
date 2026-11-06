//! Thread-bound direct GXM ownership, textures, offscreen passes and game effects.
use knife_render_api::Vertex;
use knife_render_api::experiments::{BlurWeightCache, BlurWeightResult};
use crate::gxt::{GxtFormat, GxtTexture};
use std::{ffi::{c_char, c_void, CStr}, marker::PhantomData, ptr::NonNull, rc::Rc};

unsafe extern "C" {
    fn kv_gxm_create(out: *mut *mut c_void, error: *mut c_char, capacity: u32) -> i32;
    fn kv_gxm_shader_available(g: *mut c_void, shader: u32) -> i32;
    fn kv_gxm_chain_pair_available(g: *mut c_void, first: u32, second: u32) -> i32;
    fn kv_gxm_set_blur_weights(g: *mut c_void, sigma: f32, half_taps: u32,
        weights: *const f32, count: u32) -> i32;
    fn kv_gxm_texture_create(g: *mut c_void, rgba: *const u8, width: u32, height: u32,
        stride: u32, out: *mut *mut c_void) -> i32;
    fn kv_gxm_texture_destroy(g: *mut c_void, texture: *mut c_void) -> i32;
    fn kv_gxm_movie_texture_create(g: *mut c_void, frame: *const u8, width: u32,
        height: u32, out: *mut *mut c_void) -> i32;
    fn kv_gxm_movie_texture_rebind(g: *mut c_void, texture: *mut c_void,
        frame: *const u8, width: u32, height: u32) -> i32;
    fn kv_gxm_movie_gpu_finish(g: *mut c_void) -> i32;
    fn kv_gxm_texture_create_ex(g: *mut c_void, data: *const u8, width: u32, height: u32,
        stride: u32, format: u32, srgb: u32, linear: u32, render_target: u32, out: *mut *mut c_void) -> i32;
    fn kv_gxm_texture_create_compressed(g: *mut c_void, data: *const u8, length: u32,
        width: u32, height: u32, format: u32, srgb: u32, linear: u32, out: *mut *mut c_void) -> i32;
    fn kv_gxm_texture_update(g: *mut c_void, texture: *mut c_void, x: u32, y: u32,
        width: u32, height: u32, data: *const u8, stride: u32) -> i32;
    fn kv_gxm_begin_target(g: *mut c_void, texture: *mut c_void, clear: *const f32, finish_before: i32) -> i32;
    fn kv_gxm_end_target(g: *mut c_void) -> i32;
    fn kv_gxm_texture_seal(g: *mut c_void, texture: *mut c_void) -> i32;
    fn kv_gxm_draw_effect(g: *mut c_void, vertices: *const Vertex, count: u32,
        shader: u32, blend: u32, textures: *const *const c_void, params: *const f32) -> i32;
    fn kv_gxm_copy_texture(g: *mut c_void, source: *const c_void, destination: *mut c_void) -> i32;
    fn kv_gxm_present_canvas(g: *mut c_void, canvas: *const c_void) -> i32;
    fn kv_gxm_texture_read(g: *mut c_void, texture: *const c_void, data: *mut u8, capacity: u32) -> i32;
    fn kv_gxm_begin_batch(g: *mut c_void, basic_quads: u32, effect_vertices: u32) -> i32;
    fn kv_gxm_scene_plan(g: *mut c_void, target: *mut c_void,
        reads: *const *const c_void, count: u32, kind: u32) -> i32;
    fn kv_gxm_texture_charge(payload_bytes: u32, out: *mut u32) -> i32;
    fn kv_gxm_reclaim(g: *mut c_void, wait: u32) -> i32;
    fn kv_gxm_begin(g: *mut c_void, clear: *const f32) -> i32;
    fn kv_gxm_draw_quad(g: *mut c_void, texture: *const c_void, rect: *const f32,
        uv: *const f32, color: *const f32, alpha: u32) -> i32;
    fn kv_gxm_present(g: *mut c_void) -> i32;
    fn kv_gxm_dev_scanout_target(g: *mut c_void, texture: *mut c_void) -> i32;
    fn kv_gxm_dev_swap_display_target(g: *mut c_void, texture: *mut c_void) -> i32;
    fn kv_gxm_dev_swap_display_surface(g: *mut c_void, texture: *mut c_void) -> i32;
    fn kv_gxm_readback(g: *mut c_void, pixels: *mut u8, capacity: u32) -> i32;
    fn kv_gxm_stats(g: *const c_void, stats: *mut GxmStats) -> i32;
    fn kv_gxm_timing(g: *const c_void, timing: *mut GxmTiming) -> i32;
    fn kv_gxm_cdram_available(g: *const c_void, available: *mut u32) -> i32;
    fn kv_gxm_texture_allocation_failed(g: *const c_void) -> u32;
    fn kv_gxm_error(g: *const c_void) -> *const c_char;
    fn kv_gxm_destroy(g: *mut c_void) -> i32;
}

#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GxmStats {
    pub main_mapped_bytes: u32,
    pub cdram_mapped_bytes: u32,
    pub parameter_reserved_bytes: u32,
    pub frames: u32,
    pub draws: u32,
    pub program_checks: u32,
    pub display_width: u32,
    pub display_height: u32,
    pub content_width: u32,
    pub content_height: u32,
    pub content_y: u32,
    pub display_stride: u32,
}
const _: () = assert!(std::mem::size_of::<GxmStats>() == 48);


/// Cumulative GPU-sync accounting. A CPU thread can only observe how long it was
/// blocked here; it is not a GPU execution-time measurement.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GxmTiming {
    pub finish_us: u64,
    pub finish_calls: u64,
    pub end_scene_us: u64,
    pub end_scene_calls: u64,
    pub display_finish_us: u64,
    pub display_finish_calls: u64,
    pub target_create_us: u64,
    pub target_create_calls: u64,
    pub begin_target_us: u64,
    pub begin_target_calls: u64,
    pub clear_us: u64,
    pub draw_cpu_us: u64,
    pub draw_calls: u64,
    pub draw_gxm_us: u64,
}

/// A handle cannot expose a C pointer or outlive/refer to a different backend unnoticed.
#[derive(Clone)]
pub struct TextureId { slot: usize, owner: Rc<()> }

impl std::fmt::Debug for TextureId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextureId").field("slot", &self.slot)
            .field("owner", &Rc::as_ptr(&self.owner)).finish()
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub enum TextureFormat { Rgba8 = 0, R8 = 1 }
impl TextureFormat {
    fn bytes_per_pixel(self) -> u32 { match self { Self::Rgba8 => 4, Self::R8 => 1 } }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum EffectShader {
    Sprite = 0, NoAlpha = 1, ForceAlpha = 2, Fill = 3, Text = 4, RimText = 5,
    Mono = 6, Nega = 7, Transition = 8, Rule = 9, Blur = 10, Alter = 11,
    Present = 12, PresentSrgb = 13, Ps = 14, GroupComposite = 15,
    /// Sprite multiplied by a layer group's mask surface (tex2), replacing the composite pass.
    SpriteMask = 16,
    /// Destination COLOR input is checked on the compiled Vita runtime program.
    PsFetch = 17,
    /// Specialized op19 target-color entry, admitted independently from generic fetch.
    PsFetch19 = 18,
    /// Fullscreen group op19, with the native no-haze/no-soft/no-rule contract.
    PsGroup19 = 19,
    /// Plain Sprite only; no haze, soft clip, rule, group, Fill or forced alpha.
    PsFetch11 = 20, PsFetch13 = 21,
    PlainSprite = 22, PsPlain19 = 23,
    PsFetch2 = 25,
    PlainForceAlpha = 26,
    /// Two consecutive plain PS layers with an intermediate encoded UNORM8 pack.
    PsChain2 = 27,
    /// Bounds and rim steps/strength are per triangle; RGB24 remains a shared uniform.
    RimTextBatched = 28,
    /// Specialized op7 with the complete PS source/material and group ABI.
    PsFetch7 = 29,
    /// Plain PsOp1 material source (Sprite/Fill/NoAlpha), native destination ABI.
    PsFetch1 = 30,
    /// Plain PsOp6 material source (Sprite/Fill/NoAlpha), native destination ABI.
    PsFetch6 = 31,
    /// Compiled experimental local-color-half entries; GPU diff admission is separate.
    PsPlain7 = 48,
    PsHalf1 = 49, PsHalf6 = 50, PsHalf7 = 51, PsHalf13 = 52,
    /// Exact tap order and F32 accumulation with a separate bounded weight uniform.
    BlurWeights = 53,
    /// Complete PS source contract, with only the operation dispatcher removed.
    PsFull1 = 54, PsFull6 = 55, PsFull13 = 56,
    // Fixed-operation full-material shaders; capability bank 1.
    PsAll1 = 64,
    PsAll2 = 65,
    PsAll3 = 66,
    PsAll4 = 67,
    PsAll5 = 68,
    PsAll6 = 69,
    PsAll7 = 70,
    PsAll8 = 71,
    PsAll9 = 72,
    PsAll10 = 73,
    PsAll11 = 74,
    PsAll12 = 75,
    PsAll13 = 76,
    PsAll14 = 77,
    PsAll15 = 78,
    PsAll16 = 79,
    PsAll17 = 80,
    PsAll18 = 81,
    PsAll19 = 82,
    PsAll20 = 83,
    PsAll21 = 84,
    PsAll22 = 85,
    PsAll23 = 86,
    PsAll24 = 87,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum EffectBlend { Alpha = 0, Add = 1, Sub = 2, Multiply = 3, Screen = 4,
    Opaque = 5, Over = 6, Footprint = 7, MaskAlpha = 8,
    /// Transparent text-cache coverage; existing game Over keeps additive alpha.
    CoverageOver = 9 }
#[derive(Clone, Copy)]
struct TextureRecord { pointer: NonNull<c_void>, width: u32, height: u32, storage: TextureStorage }
#[derive(Clone, Copy, PartialEq, Eq)]
enum TextureStorage { Linear(u32), Movie, Compressed }

pub struct Gxm {
    pointer: Option<NonNull<c_void>>,
    closing: bool,
    last_texture_allocation_failed: bool,
    textures: Vec<Option<TextureRecord>>,
    scene_reads: Vec<*const c_void>,
    blur_weights: BlurWeightCache,
    owner: Rc<()>,
    // The creating thread owns all GXM operations, including Drop.
    _thread_bound: PhantomData<Rc<()>>,
}

impl Gxm {
    pub fn new() -> Result<Self, String> {
        let mut pointer = std::ptr::null_mut();
        let mut error = [0 as c_char; 256];
        // SAFETY: both out parameters have their full C ABI size and outlive this call.
        let status = unsafe { kv_gxm_create(&mut pointer, error.as_mut_ptr(), error.len() as u32) };
        if status < 0 {
            // SAFETY: C always terminates this nonempty error buffer.
            let message = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy();
            let message = format!("GXM initialization: {message} ({status:#010x})");
            if let Some(pointer) = NonNull::new(pointer) {
                // C retained a partial owner. Keep an explicit cleanup guard and
                // retry its teardown; it must never become a renderable owner.
                let _cleanup = Self { pointer: Some(pointer), closing: true, last_texture_allocation_failed: false, textures: Vec::new(), scene_reads: Vec::new(), blur_weights: BlurWeightCache::default(),
                    owner: Rc::new(()), _thread_bound: PhantomData };
            }
            return Err(message);
        }
        let pointer = NonNull::new(pointer).ok_or("GXM returned a null owner")?;
        Ok(Self { pointer: Some(pointer), closing: false, last_texture_allocation_failed: false, textures: Vec::new(), scene_reads: Vec::new(), blur_weights: BlurWeightCache::default(), owner: Rc::new(()), _thread_bound: PhantomData })
    }
    fn raw(&self) -> Result<*mut c_void, String> {
        if self.closing { return Err("GXM owner is closing; only cleanup may be retried".into()); }
        self.pointer.map(NonNull::as_ptr).ok_or_else(|| "GXM owner is closed".into())
    }
    fn check(&self, status: i32) -> Result<(), String> {
        if status >= 0 { return Ok(()); }
        let raw = self.pointer.ok_or("GXM owner is closed")?.as_ptr();
        // SAFETY: native owner keeps its fixed, terminated error buffer alive.
        let message = unsafe { CStr::from_ptr(kv_gxm_error(raw)) }.to_string_lossy();
        Err(format!("GXM: {message} ({status:#010x})"))
    }
    pub fn create_texture(&mut self, rgba: &[u8], width: u32, height: u32,
        stride: u32) -> Result<TextureId, String> {
        self.last_texture_allocation_failed = false;
        if width == 0 || width > 960 || height == 0 || height > 1080
            || stride < width * 4 || stride > 3840 {
            return Err("invalid RGBA texture dimensions or stride".into());
        }
        let needed = u64::from(stride) * u64::from(height - 1) + u64::from(width) * 4;
        if needed > rgba.len() as u64 { return Err("RGBA texture buffer is shorter than its layout".into()); }
        let raw = self.raw()?;
        let mut texture = std::ptr::null_mut();
        // SAFETY: checked dimensions/stride cover every source read; C copies synchronously.
        let status = unsafe { kv_gxm_texture_create(raw, rgba.as_ptr(), width, height, stride, &mut texture) };
        if status < 0 {
            // Failed rollback may retain an unmapped allocation in C. Restrict
            // this owner to cleanup even when the original failure was harmless.
            self.closing = true;
            self.check(status)?;
        }
        let texture = NonNull::new(texture).ok_or("GXM returned a null texture")?;
        let slot = self.textures.len();
        self.textures.push(Some(TextureRecord { pointer: texture, width, height, storage: TextureStorage::Linear(4) }));
        Ok(TextureId { slot, owner: self.owner.clone() })
    }
    /// # Safety
    /// Frame points to a live GPU-mapped AvPlayer YVU420P2 allocation,
    /// kept alive until the GPU has finished every submission using this texture.
    pub unsafe fn create_movie_texture(&mut self, frame: *const u8, width: u32, height: u32) -> Result<TextureId, String> {
        if frame.is_null() || width == 0 || height == 0 || width > 1920 || height > 1088
            || width % 2 != 0 || height % 2 != 0 {
            return Err("invalid native movie frame pointer or dimensions".into());
        }
        let mut texture = std::ptr::null_mut();
        // SAFETY: caller validates mapping/lifetime; C validates pointer alignment and descriptor.
        self.check(unsafe { kv_gxm_movie_texture_create(self.raw()?, frame, width, height, &mut texture) })?;
        let pointer = NonNull::new(texture).ok_or("GXM returned a null movie texture")?;
        let slot = self.textures.len();
        self.textures.push(Some(TextureRecord { pointer, width, height, storage: TextureStorage::Movie }));
        Ok(TextureId { slot, owner: self.owner.clone() })
    }
    /// # Safety
    /// The new GPU-mapped frame stays alive until all sampling submissions finish.
    pub unsafe fn rebind_movie_texture(&mut self, id: &TextureId, frame: *const u8, width: u32, height: u32) -> Result<(), String> {
        let texture = self.texture(id)?;
        let record = self.textures[id.slot].unwrap();
        if frame.is_null() || record.storage != TextureStorage::Movie || record.width != width || record.height != height {
            return Err("movie texture ownership or frame dimensions mismatch".into());
        }
        // SAFETY: owner and dimensions match; caller owns mapped frame through finish.
        self.check(unsafe { kv_gxm_movie_texture_rebind(self.raw()?, texture.as_ptr(), frame, width, height) })
    }
    pub fn finish_movie_sampling(&mut self) -> Result<(), String> {
        // SAFETY: native checks creator thread and an idle scene, then waits for GPU.
        self.check(unsafe { kv_gxm_movie_gpu_finish(self.raw()?) })
    }
    fn texture(&self, id: &TextureId) -> Result<NonNull<c_void>, String> {
        if !Rc::ptr_eq(&self.owner, &id.owner) { return Err("texture belongs to a different GXM owner".into()); }
        self.textures.get(id.slot).copied().flatten().map(|r| r.pointer)
            .ok_or_else(|| "texture has been retired".into())
    }
    pub fn destroy_texture(&mut self, id: &TextureId) -> Result<(), String> {
        let texture = self.texture(id)?;
        // SAFETY: checked live slot belongs to this owner; C waits for GPU completion.
        let raw = self.raw()?;
        // Retire the Rust handle before a possibly partial native release.
        self.textures[id.slot] = None;
        let status = unsafe { kv_gxm_texture_destroy(raw, texture.as_ptr()) };
        if status < 0 { self.closing = true; }
        self.check(status)
    }
    /// Read the optional shader capability admitted completely during startup.
    pub fn shader_available(&self, shader: u32) -> Result<bool, String> {
        let status = unsafe { kv_gxm_shader_available(self.raw()?, shader) };
        self.check(status)?;
        Ok(status != 0)
    }
    /// Startup admitted the exact compile-time material variant and its cost.
    pub fn chain_pair_available(&self, first: u32, second: u32) -> Result<bool, String> {
        let status = unsafe { kv_gxm_chain_pair_available(self.raw()?, first, second) };
        self.check(status)?;
        Ok(status != 0)
    }
    /// Describe actual sampled resources before a scene begins; native checks
    /// RAW/WAR/WAW and consumes this reusable pointer array synchronously.
    pub fn plan_scene(&mut self, target: Option<&TextureId>, reads: &[TextureId], kind: u32) -> Result<(), String> {
        if reads.len() > 262144 { return Err("scene read plan exceeds native limit".into()); }
        let target = target.map(|id| self.texture(id)).transpose()?.map_or(std::ptr::null_mut(), NonNull::as_ptr);
        self.scene_reads.clear();
        for id in reads {
            let pointer = self.texture(id)?.as_ptr() as *const c_void;
            self.scene_reads.push(pointer);
        }
        let count = self.scene_reads.len() as u32;
        self.check(unsafe { kv_gxm_scene_plan(self.raw()?, target, self.scene_reads.as_ptr(), count, kind) })
    }
    /// Preflight one command batch before its first scene/copy. Native slots
    /// retain mapped storage and texture owners through actual GPU completion.
    pub fn begin_batch(&mut self, basic_quads: usize, effect_vertices: usize) -> Result<(), String> {
        let basic = u32::try_from(basic_quads).map_err(|_| "basic vertex count exceeds native bound")?;
        let effect = u32::try_from(effect_vertices).map_err(|_| "effect vertex count exceeds native bound")?;
        self.check(unsafe { kv_gxm_begin_batch(self.raw()?, basic, effect) })
    }
    /// Logical slice charge. Physical page slack and pending retirement remain
    /// covered by the independent native CDRAM admission query.
    pub fn texture_charge(payload_bytes: u32) -> Result<u32, String> {
        let mut charge = 0;
        let status = unsafe { kv_gxm_texture_charge(payload_bytes, &mut charge) };
        if status < 0 { Err("invalid native texture charge".into()) } else { Ok(charge) }
    }
    /// Poll retired native texture owners, optionally waiting under explicit
    /// memory pressure. Ordinary frame submission only polls at batch begin.
    pub fn reclaim_retired(&mut self, wait: bool) -> Result<(), String> {
        self.check(unsafe { kv_gxm_reclaim(self.raw()?, u32::from(wait)) })
    }
    pub fn begin(&mut self, clear: [f32; 4]) -> Result<(), String> {
        // SAFETY: fixed four-float input lives through the synchronous call.
        self.check(unsafe { kv_gxm_begin(self.raw()?, clear.as_ptr()) })
    }
    /// Source bytes are borrowed only until C has copied the checked layout.
    #[allow(clippy::too_many_arguments)]
    pub fn create_texture_ex(&mut self, data: Option<&[u8]>, width: u32, height: u32,
        stride: u32, format: TextureFormat, srgb: bool, linear: bool,
        render_target: bool) -> Result<TextureId, String> {
        self.last_texture_allocation_failed = false;
        let bpp = format.bytes_per_pixel();
        if width == 0 || height == 0 || width > 2048 || height > 2048
            || (matches!(format, TextureFormat::R8) && (srgb || render_target)) {
            return Err("invalid native texture layout or format".into());
        }
        if let Some(data) = data { Self::source_layout(data, width, height, stride, bpp)?; }
        let mut texture = std::ptr::null_mut();
        // SAFETY: C validates the same dimensions and copies borrowed bytes synchronously.
        let status = unsafe { kv_gxm_texture_create_ex(self.raw()?, data.map_or(std::ptr::null(), |b| b.as_ptr()),
            width, height, stride, format as u32, u32::from(srgb), u32::from(linear),
            u32::from(render_target), &mut texture) };
        // SAFETY: the owner remains alive after a failed creation/rollback.
        self.last_texture_allocation_failed = status < 0
            && unsafe { kv_gxm_texture_allocation_failed(self.raw()?) } != 0;
        self.check(status)?;
        let pointer = NonNull::new(texture).ok_or("GXM returned a null extended texture")?;
        let slot = self.textures.len();
        self.textures.push(Some(TextureRecord { pointer, width, height, storage: TextureStorage::Linear(bpp) }));
        Ok(TextureId { slot, owner: self.owner.clone() })
    }
    /// Validates a complete single-texture GXT and copies its already swizzled
    /// PVRTC4/DXT1/DXT5 payload into immutable, budgeted native storage.
    /// No runtime decompression or RGBA staging allocation is performed.
    pub fn create_gxt_texture(&mut self, file: &[u8], srgb: bool, linear: bool) -> Result<TextureId, String> {
        self.last_texture_allocation_failed = false;
        let gxt = GxtTexture::parse(file).map_err(|e| e.to_string())?;
        let format = match gxt.format {
            GxtFormat::Pvrtc4 => 0x8100_0000,
            GxtFormat::Dxt1 => 0x8500_0000,
            GxtFormat::Dxt5 => 0x8700_0000,
        };
        let mut texture = std::ptr::null_mut();
        // SAFETY: the borrowed view validates dimensions and every payload byte;
        // C independently validates length/format and copies before returning.
        let status = unsafe { kv_gxm_texture_create_compressed(self.raw()?, gxt.data.as_ptr(),
            gxt.data.len() as u32, u32::from(gxt.width), u32::from(gxt.height), format,
            u32::from(srgb), u32::from(linear), &mut texture) };
        // SAFETY: C owns the failure record, including rollback safety.
        self.last_texture_allocation_failed = status < 0
            && unsafe { kv_gxm_texture_allocation_failed(self.raw()?) } != 0;
        self.check(status)?;
        let pointer = NonNull::new(texture).ok_or("GXM returned a null compressed texture")?;
        let slot = self.textures.len();
        self.textures.push(Some(TextureRecord { pointer, width: u32::from(gxt.width),
            height: u32::from(gxt.height), storage: TextureStorage::Compressed }));
        Ok(TextureId { slot, owner: self.owner.clone() })
    }
    /// True only for an allocation failure with a clean, usable native owner.
    pub fn texture_allocation_failed(&self) -> bool { self.last_texture_allocation_failed }

    fn source_layout(data: &[u8], width: u32, height: u32, stride: u32, bpp: u32) -> Result<(), String> {
        if width == 0 || height == 0 || stride < width * bpp || stride > 8192 {
            return Err("invalid source texture stride or dimensions".into());
        }
        let needed = u64::from(stride) * u64::from(height - 1) + u64::from(width * bpp);
        if needed > data.len() as u64 { return Err("texture source is shorter than its layout".into()); }
        Ok(())
    }
    pub fn update_texture(&mut self, id: &TextureId, origin: [u32;2], size: [u32;2],
        data: &[u8], stride: u32) -> Result<(), String> {
        let pointer = self.texture(id)?;
        let record = self.textures[id.slot].unwrap();
        let TextureStorage::Linear(bpp) = record.storage else {
            return Err("only linear pixel textures support CPU updates".into());
        };
        if origin[0] > record.width || origin[1] > record.height
            || size[0] > record.width - origin[0] || size[1] > record.height - origin[1] {
            return Err("texture update lies outside the allocation".into());
        }
        Self::source_layout(data,size[0],size[1],stride,bpp)?;
        // SAFETY: ownership, bounds, and the complete source layout have been checked.
        self.check(unsafe { kv_gxm_texture_update(self.raw()?,pointer.as_ptr(),origin[0],origin[1],
            size[0],size[1],data.as_ptr(),stride) })
    }
    /// Always drains first; used by the probes and any caller without a barrier model.
    pub fn begin_target(&mut self, id: &TextureId, clear: Option<[f32;4]>) -> Result<(), String> {
        self.begin_target_flags(id, clear, true)
    }
    pub fn begin_target_flags(&mut self, id: &TextureId, clear: Option<[f32;4]>, finish_before: bool)
        -> Result<(), String> {
        let texture = self.texture(id)?;
        // SAFETY: owned live texture; optional clear lives through this call.
        self.check(unsafe { kv_gxm_begin_target(self.raw()?,texture.as_ptr(),
            clear.as_ref().map_or(std::ptr::null(),|v| v.as_ptr()), finish_before as i32) })
    }
    pub fn end_target(&mut self) -> Result<(), String> {
        // SAFETY: native owner validates that an offscreen scene is open.
        let status = unsafe { kv_gxm_end_target(self.raw()?) };
        if status < 0 { self.closing = true; }
        self.check(status)
    }
    /// Complete the bake and release its framebuffer attachments, keeping the
    /// owned sampled texture alive. The native owner retains failed teardown.
    pub fn seal_texture(&mut self, id: &TextureId) -> Result<(), String> {
        let texture = self.texture(id)?;
        // SAFETY: live thread-bound texture; C checks pass state and storage.
        self.check(unsafe { kv_gxm_texture_seal(self.raw()?, texture.as_ptr()) })
    }
    pub fn draw_effect(&mut self, vertices: &[Vertex], shader: EffectShader, blend: EffectBlend,
        textures: [Option<&TextureId>;4], params: [f32;8]) -> Result<(), String> {
        if vertices.is_empty() || vertices.len() > 32760 || vertices.len() % 3 != 0 {
            return Err("effects draw requires a bounded triangle list".into());
        }
        let mut native = [std::ptr::null();4];
        for (i,id) in textures.iter().enumerate() {
            if let Some(id) = id { native[i] = self.texture(id)?.as_ptr().cast_const(); }
        }
        if shader == EffectShader::BlurWeights {
            let n = params[3];
            if !n.is_finite() || n < 0.0 || n > 16.0 || n.trunc() != n {
                return Err("blur weight draw exceeds bounded kernel".into());
            }
            let sigma = params[2].max(0.01);
            let BlurWeightResult::Cached(weights) = self.blur_weights.get_or_compute(sigma, n as usize) else {
                return Err("invalid blur weight group".into());
            };
            // SAFETY: native synchronously copies this exact bounded F32 group;
            // draw scope below uploads it into the shader's separate uniform.
            self.check(unsafe { kv_gxm_set_blur_weights(self.raw()?, sigma, n as u32,
                weights.as_ptr(), weights.len() as u32) })?;
        }
        // SAFETY: the shared repr(C) ABI is asserted in knife-render-api and C;
        // all handles are live, the complete slice/params live through synchronous submission.
        self.check(unsafe { kv_gxm_draw_effect(self.raw()?,vertices.as_ptr(),vertices.len() as u32,
            shader as u32,blend as u32,native.as_ptr(),params.as_ptr()) })
    }
    pub fn copy_texture(&mut self, source: &TextureId, destination: &TextureId) -> Result<(), String> {
        let (source,destination) = (self.texture(source)?,self.texture(destination)?);
        // SAFETY: both live slots belong to this device. C validates formats and dimensions.
        self.check(unsafe { kv_gxm_copy_texture(self.raw()?,source.as_ptr(),destination.as_ptr()) })
    }
    pub fn present_canvas(&mut self, canvas: &TextureId) -> Result<(), String> {
        let canvas = self.texture(canvas)?;
        // SAFETY: owned texture, native submission checks scene and display lifetime.
        let status = unsafe { kv_gxm_present_canvas(self.raw()?,canvas.as_ptr()) };
        if status < 0 { self.closing = true; }
        self.check(status)
    }
    pub fn read_texture(&mut self, id: &TextureId) -> Result<Vec<u8>, String> {
        let texture = self.texture(id)?;
        let record = self.textures[id.slot].unwrap();
        let TextureStorage::Linear(bpp) = record.storage else {
            return Err("only linear pixel textures support CPU reads".into());
        };
        let length = record.width as usize * record.height as usize * bpp as usize;
        let mut pixels = vec![0;length];
        // SAFETY: complete checked allocation size; native waits before reading.
        self.check(unsafe { kv_gxm_texture_read(self.raw()?,texture.as_ptr(),pixels.as_mut_ptr(),length as u32) })?;
        Ok(pixels)
    }
    pub fn draw(&mut self, texture: Option<&TextureId>, rect: [f32; 4], uv: [f32; 4],
        color: [f32; 4], alpha: bool) -> Result<(), String> {
        let texture = texture.map(|id| self.texture(id)).transpose()?;
        // SAFETY: all fixed arrays are valid; optional texture is a live owned slot.
        self.check(unsafe { kv_gxm_draw_quad(self.raw()?, texture.map_or(std::ptr::null(), |p| p.as_ptr()),
            rect.as_ptr(), uv.as_ptr(), color.as_ptr(), u32::from(alpha)) })
    }
    pub fn present(&mut self) -> Result<(), String> {
        // SAFETY: live thread-bound owner, no borrowed data is retained by Rust.
        let status = unsafe { kv_gxm_present(self.raw()?) };
        if status < 0 { self.closing = true; }
        self.check(status)
    }
    pub fn readback(&mut self) -> Result<Vec<u8>, String> {
        let mut pixels = vec![0; 960 * 544 * 4];
        // SAFETY: destination covers the entire completed front buffer and C copies synchronously.
        self.check(unsafe { kv_gxm_readback(self.raw()?, pixels.as_mut_ptr(), pixels.len() as u32) })?;
        Ok(pixels)
    }
    /// DEV probe only: display an owned full-screen RT without texture sampling.
    pub(crate) fn dev_scanout_target(&mut self, id: &TextureId) -> Result<(), String> {
        let texture = self.texture(id)?;
        // SAFETY: the owned live texture remains in C's resource list. Native
        // code rejects destruction while scanout references this allocation.
        self.check(unsafe { kv_gxm_dev_scanout_target(self.raw()?, texture.as_ptr()) })
    }
    pub(crate) fn dev_swap_display_target(&mut self, id: &TextureId) -> Result<(), String> {
        let texture = self.texture(id)?;
        // SAFETY: native checks equal dimensions and drains all in-flight work;
        // both native owners retain exactly one target after the exchange.
        self.check(unsafe { kv_gxm_dev_swap_display_target(self.raw()?, texture.as_ptr()) })
    }
    pub(crate) fn dev_swap_display_surface(&mut self, id: &TextureId) -> Result<(), String> {
        let texture = self.texture(id)?;
        // SAFETY: native requires equal layouts, an unused back buffer and no
        // active scene; it drains GPU/display work and preserves both owners.
        self.check(unsafe { kv_gxm_dev_swap_display_surface(self.raw()?, texture.as_ptr()) })
    }
    pub fn timing(&self) -> Result<GxmTiming, String> {
        let mut timing = GxmTiming::default();
        self.check(unsafe { kv_gxm_timing(self.raw()?, &mut timing) })?;
        Ok(timing)
    }

    pub fn stats(&self) -> Result<GxmStats, String> {
        let mut stats = GxmStats::default();
        // SAFETY: repr(C) report has the same statically asserted 48-byte layout.
        self.check(unsafe { kv_gxm_stats(self.raw()?, &mut stats) })?;
        Ok(stats)
    }
    /// Current kernel CDRAM headroom, capped by the native owner's allocation
    /// policy and parameter reserve. This observation is not an OS reservation.
    pub fn available_cdram(&self) -> Result<usize, String> {
        let mut available = 0u32;
        // SAFETY: the thread-bound owner is live and the ABI writes one u32.
        self.check(unsafe { kv_gxm_cdram_available(self.raw()?, &mut available) })?;
        usize::try_from(available).map_err(|_| "CDRAM allowance exceeds address space".into())
    }
    pub fn close(&mut self) -> Result<(), String> {
        if let Some(pointer) = self.pointer {
            self.closing = true;
            self.textures.clear();
            // SAFETY: unique native owner; C drains GPU/display jobs before freeing it.
            self.check(unsafe { kv_gxm_destroy(pointer.as_ptr()) })?;
            self.pointer = None;
        }
        Ok(())
    }
}
impl Drop for Gxm {
    fn drop(&mut self) {
        if let Err(error) = self.close() { eprintln!("GXM resources retained after cleanup failure: {error}"); }
    }
}
