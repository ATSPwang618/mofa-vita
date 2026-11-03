//! Strict CPU recorder for facade contract tests. It does not execute shaders,
//! model GPU timing, or provide evidence of GXM rendering correctness.

use knife_render_api::Vertex;
use super::{GxtFormat, GxtTexture};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureFormat {
    Rgba8,
    R8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectShader {
    Sprite,
    NoAlpha,
    ForceAlpha,
    Fill,
    Text,
    RimText,
    Mono,
    Nega,
    Transition,
    Rule,
    Blur,
    Alter,
    Present,
    PresentSrgb,
    Ps,
    GroupComposite,
    SpriteMask,
    PsFetch,
    PsFetch19,
    PsGroup19,
    PsFetch11,
    PsFetch13,
    PlainSprite,
    PsPlain19,
    PsFetch2 = 25,
    PlainForceAlpha = 26,
    PsChain2 = 27,
    RimTextBatched = 28,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectBlend {
    Alpha,
    Add,
    Sub,
    Multiply,
    Screen,
    Opaque,
    Over,
    Footprint,
    MaskAlpha,
    CoverageOver,
}
#[derive(Clone)]
pub struct TextureId(pub usize);

#[derive(Clone, Debug)]
pub enum Event {
    Create {
        id: usize,
        size: [u32; 2],
        format: TextureFormat,
        srgb: bool,
        linear: bool,
        target: bool,
    },
    CreateCompressed {
        id: usize,
        size: [u32; 2],
        format: GxtFormat,
        payload_bytes: usize,
        srgb: bool,
        linear: bool,
    },
    Update {
        id: usize,
        origin: [u32; 2],
        size: [u32; 2],
        stride: u32,
    },
    Begin {
        id: usize,
        clear: Option<[f32; 4]>,
    },
    End,
    Seal(usize),
    Draw {
        shader: EffectShader,
        blend: EffectBlend,
        textures: [Option<usize>; 4],
        params: [f32; 8],
        vertices: Vec<Vertex>,
    },
    Copy {
        src: usize,
        dst: usize,
    },
    Present(usize),
    Read(usize),
    Destroy(usize),
    Close,
}

#[derive(Default)]
pub struct Trace {
    pub events: Vec<Event>,
    pub scene_plans: Vec<(Option<usize>, Vec<usize>, u32)>,
    pub fail_next_draw: bool,
    /// Explicit runtime compiler/patch capabilities; unavailable by default.
    pub shader_capabilities: u32,
    pub wrong_readback_length: bool,
    pub active: Option<usize>,
    /// Optional native allocation ceiling, including ordinary targets/fonts.
    pub cdram_limit: Option<usize>,
    /// Memory owned by another native subsystem, outside this recorder.
    pub external_cdram_bytes: usize,
    pub fail_next_retirement: bool,
    pub fail_next_compressed_create: bool,
    /// Driver allocation failures despite a successful free-memory query.
    pub fail_texture_allocations: usize,
    pub texture_allocation_attempts: usize,
    pub pressure_waits: usize,
    pub fail_next_seal: bool,
    /// Test a finite native render-target budget independently of pixel bytes.
    pub render_target_limit: Option<usize>,
}

pub type Control = Rc<RefCell<Trace>>;
struct Texture {
    size: [u32; 2],
    bpp: u32,
    target: bool,
    pixels: Vec<u8>,
    allocation_bytes: usize,
}

pub struct Gxm {
    pub control: Control,
    textures: HashMap<usize, Texture>,
    next_id: usize,
    /// Last barrier decision, so tests can assert the model runs.
    pub last_finish_before: bool,
    last_texture_allocation_failed: bool,
}

impl Gxm {
    pub fn begin_batch(&mut self, _basic_quads: usize, _effect_vertices: usize) -> Result<(), String> { Ok(()) }
    pub fn reclaim_retired(&mut self, wait: bool) -> Result<(), String> {
        if wait { self.control.borrow_mut().pressure_waits += 1; }
        Ok(())
    }
    pub fn texture_allocation_failed(&self) -> bool { self.last_texture_allocation_failed }
    fn allocate_texture(&mut self, bytes: usize) -> Result<(), String> {
        self.control.borrow_mut().texture_allocation_attempts += 1;
        if bytes > self.available_cdram()? {
            self.last_texture_allocation_failed = true;
            return Err("mock native CDRAM allocation budget exceeded".into());
        }
        let mut control = self.control.borrow_mut();
        if control.fail_texture_allocations != 0 {
            control.fail_texture_allocations -= 1;
            self.last_texture_allocation_failed = true;
            return Err("injected native CDRAM allocation failure".into());
        }
        Ok(())
    }
    pub fn texture_charge(bytes: u32) -> Result<u32, String> {
        let granule = if knife_perf::optimizations::enabled(knife_perf::optimizations::TEXTURE_POOL)
            && bytes < 256 * 1024 { 64 } else { 256 * 1024 };
        bytes.checked_add(granule - 1).map(|v| v & !(granule - 1)).ok_or("texture charge overflow".into())
    }
    pub fn plan_scene(&mut self, target: Option<&TextureId>, reads: &[TextureId], kind: u32) -> Result<(), String> {
        self.control.borrow_mut().scene_plans.push((target.map(|t| t.0), reads.iter().map(|t| t.0).collect(), kind));
        Ok(())
    }
    pub fn shader_available(&self, shader: u32) -> Result<bool, String> {
        Ok(shader < 32 && self.control.borrow().shader_capabilities & (1u32 << shader) != 0)
    }
    pub fn chain_pair_available(&self, first: u32, second: u32) -> Result<bool, String> {
        let ops = [2, 11, 13, 19];
        Ok(self.shader_available(27)? && ops.contains(&first) && ops.contains(&second))
    }
    pub fn live_render_targets(&self) -> usize {
        self.textures.values().filter(|texture| texture.target).count()
    }
    pub fn available_cdram(&self) -> Result<usize, String> {
        let control = self.control.borrow();
        let used: usize = self.textures.values().map(|t| t.allocation_bytes).sum();
        control.cdram_limit.unwrap_or(48 * 1024 * 1024)
            .checked_sub(used + control.external_cdram_bytes)
            .ok_or_else(|| "mock CDRAM accounting exceeds native limit".into())
    }
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            control: Rc::new(RefCell::new(Trace::default())),
            textures: HashMap::new(),
            next_id: 0,
            last_finish_before: true,
            last_texture_allocation_failed: false,
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub fn create_texture_ex(
        &mut self,
        data: Option<&[u8]>,
        width: u32,
        height: u32,
        stride: u32,
        format: TextureFormat,
        srgb: bool,
        linear: bool,
        render_target: bool,
    ) -> Result<TextureId, String> {
        self.last_texture_allocation_failed = false;
        if width == 0 || width > 2048 || height == 0 || height > 2048 {
            return Err("mock texture dimensions".into());
        }
        let bpp = if format == TextureFormat::R8 { 1 } else { 4 };
        let row = width * bpp;
        if stride < row || (srgb && bpp != 4) || (render_target && bpp != 4) {
            return Err("mock texture format".into());
        }
        let charge = Self::texture_charge(((width + 7) & !7) * height * bpp)? as usize;
        self.allocate_texture(charge)?;
        if render_target && self.control.borrow().render_target_limit.is_some_and(|limit|
            self.textures.values().filter(|texture| texture.target).count() >= limit) {
            return Err("mock native render-target budget exceeded".into());
        }
        let mut pixels = vec![0; (row * height) as usize];
        if let Some(data) = data {
            if data.len() < ((height - 1) * stride + row) as usize {
                return Err("mock source too short".into());
            }
            for y in 0..height as usize {
                pixels[y * row as usize..(y + 1) * row as usize].copy_from_slice(
                    &data[y * stride as usize..y * stride as usize + row as usize],
                );
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        self.textures.insert(
            id,
            Texture {
                size: [width, height],
                bpp,
                target: render_target,
                pixels,
                allocation_bytes: charge,
            },
        );
        self.control.borrow_mut().events.push(Event::Create {
            id,
            size: [width, height],
            format,
            srgb,
            linear,
            target: render_target,
        });
        Ok(TextureId(id))
    }
    pub fn create_gxt_texture(&mut self, file: &[u8], srgb: bool, linear: bool) -> Result<TextureId, String> {
        self.last_texture_allocation_failed = false;
        let gxt = GxtTexture::parse(file).map_err(|e| e.to_string())?;
        let payload_bytes = gxt.data.len();
        let allocation_bytes = Self::texture_charge(u32::try_from(payload_bytes).map_err(|_| "GXT allocation overflow")?)? as usize;
        self.allocate_texture(allocation_bytes)?;
        if std::mem::take(&mut self.control.borrow_mut().fail_next_compressed_create) {
            return Err("injected native compressed create failure".into());
        }
        let id = self.next_id;
        self.next_id += 1;
        let size = [u32::from(gxt.width), u32::from(gxt.height)];
        self.textures.insert(id, Texture {
            size, bpp: 0, target: false, pixels: Vec::new(), allocation_bytes,
        });
        self.control.borrow_mut().events.push(Event::CreateCompressed {
            id, size, format: gxt.format, payload_bytes, srgb, linear,
        });
        Ok(TextureId(id))
    }
    pub fn update_texture(
        &mut self,
        id: &TextureId,
        origin: [u32; 2],
        size: [u32; 2],
        data: &[u8],
        stride: u32,
    ) -> Result<(), String> {
        let texture = self.textures.get_mut(&id.0).ok_or("mock texture missing")?;
        if texture.bpp == 0 { return Err("mock nonlinear texture is immutable".into()); }
        let [x, y] = origin;
        let [w, h] = size;
        if x + w > texture.size[0]
            || y + h > texture.size[1]
            || data.len() < ((h - 1) * stride + w * texture.bpp) as usize
        {
            return Err("mock update bounds".into());
        }
        for row in 0..h as usize {
            let start =
                ((y as usize + row) * texture.size[0] as usize + x as usize) * texture.bpp as usize;
            let length = (w * texture.bpp) as usize;
            texture.pixels[start..start + length]
                .copy_from_slice(&data[row * stride as usize..row * stride as usize + length]);
        }
        self.control.borrow_mut().events.push(Event::Update {
            id: id.0,
            origin,
            size,
            stride,
        });
        Ok(())
    }
    /// The mock has no GPU in flight, so the barrier flag only has to be accepted.
    pub fn begin_target_flags(&mut self, id: &TextureId, clear: Option<[f32; 4]>, finish_before: bool)
        -> Result<(), String> {
        self.last_finish_before = finish_before;
        self.begin_target(id, clear)
    }
    pub fn begin_target(&mut self, id: &TextureId, clear: Option<[f32; 4]>) -> Result<(), String> {
        if self.control.borrow().active.is_some() {
            return Err("mock nested pass".into());
        }
        if !self
            .textures
            .get(&id.0)
            .ok_or("mock target missing")?
            .target
        {
            return Err("mock non-target".into());
        }
        let mut control = self.control.borrow_mut();
        control.active = Some(id.0);
        control.events.push(Event::Begin { id: id.0, clear });
        Ok(())
    }
    pub fn end_target(&mut self) -> Result<(), String> {
        let mut control = self.control.borrow_mut();
        if control.active.take().is_none() {
            return Err("mock no pass".into());
        }
        control.events.push(Event::End);
        Ok(())
    }
    pub fn seal_texture(&mut self, id: &TextureId) -> Result<(), String> {
        if self.control.borrow().active.is_some() {
            return Err("mock sealing inside pass".into());
        }
        let texture = self.textures.get_mut(&id.0).ok_or("mock texture missing")?;
        if texture.bpp != 4 { return Err("mock cannot seal this texture format".into()); }
        if std::mem::take(&mut self.control.borrow_mut().fail_next_seal) {
            return Err("injected native seal failure".into());
        }
        texture.target = false;
        self.control.borrow_mut().events.push(Event::Seal(id.0));
        Ok(())
    }
    pub fn draw_effect(
        &mut self,
        vertices: &[Vertex],
        shader: EffectShader,
        blend: EffectBlend,
        textures: [Option<&TextureId>; 4],
        params: [f32; 8],
    ) -> Result<(), String> {
        let mut control = self.control.borrow_mut();
        let target = control.active.ok_or("mock draw outside pass")?;
        if control.fail_next_draw {
            control.fail_next_draw = false;
            return Err("injected native draw failure".into());
        }
        for id in textures.iter().flatten() {
            if !self.textures.contains_key(&id.0) {
                return Err("mock retired sampled texture".into());
            }
            if id.0 == target {
                return Err("mock texture feedback".into());
            }
        }
        if vertices.is_empty() || vertices.len() % 3 != 0 {
            return Err("mock triangle count".into());
        }
        if vertices.iter().any(|v| v.haze != vertices[0].haze) {
            return Err("mock per-quad uniforms differ".into());
        }
        if matches!(shader, EffectShader::PlainSprite | EffectShader::PsPlain19 | EffectShader::PsFetch2 | EffectShader::PlainForceAlpha) {
            if control.shader_capabilities & (1u32 << shader as u32) == 0 {
                return Err("mock unavailable plain shader".into());
            }
            if vertices.iter().any(|v| v.haze.iter().flatten().chain(v.soft.iter().flatten()).any(|x| *x != 0.0)) {
                return Err("mock plain shader cannot discard haze/soft".into());
            }
            if matches!(shader, EffectShader::PlainSprite | EffectShader::PlainForceAlpha) && params != [0.0; 8] {
                return Err("mock plain sprite params".into());
            }
            if shader == EffectShader::PsPlain19 && (blend != EffectBlend::Opaque || params[0] != 19.0
                || (params[1] != 0.0 && params[1] != 1.0) || params[2] <= 0.0 || params[3] <= 0.0 || params[4] >= 0.0
                || (params[6] != 0.0 && params[6] != 1.0) || params[7] != 0.0) {
                return Err("mock plain PS19 ABI".into());
            }
            if shader == EffectShader::PsFetch2 && (blend != EffectBlend::Opaque || params[0] != 2.0
                || params[1] != 0.0 || params[2] <= 0.0 || params[3] <= 0.0 || params[4] >= 0.0
                || params[6] != 0.0 || params[7] != 0.0) {
                return Err("mock plain PS2 ABI".into());
            }
        }
        control.events.push(Event::Draw {
            shader,
            blend,
            textures: textures.map(|id| id.map(|id| id.0)),
            params,
            vertices: vertices.to_vec(),
        });
        Ok(())
    }
    pub fn copy_texture(&mut self, src: &TextureId, dst: &TextureId) -> Result<(), String> {
        if self.control.borrow().active.is_some() {
            return Err("mock copy inside pass".into());
        }
        let source = self.textures.get(&src.0).ok_or("mock source missing")?;
        let pixels = source.pixels.clone();
        let (size, bpp) = (source.size, source.bpp);
        let destination = self
            .textures
            .get_mut(&dst.0)
            .ok_or("mock destination missing")?;
        if bpp == 0 || destination.bpp == 0 || destination.size != size || destination.bpp != bpp {
            return Err("mock copy formats".into());
        }
        destination.pixels = pixels;
        self.control.borrow_mut().events.push(Event::Copy {
            src: src.0,
            dst: dst.0,
        });
        Ok(())
    }
    pub fn present_canvas(&mut self, id: &TextureId) -> Result<(), String> {
        if self.control.borrow().active.is_some() {
            return Err("mock present inside pass".into());
        }
        if !self.textures.contains_key(&id.0) {
            return Err("mock canvas missing".into());
        }
        self.control.borrow_mut().events.push(Event::Present(id.0));
        Ok(())
    }
    pub fn read_texture(&mut self, id: &TextureId) -> Result<Vec<u8>, String> {
        let texture = self
            .textures
            .get(&id.0)
            .ok_or("mock read texture missing")?;
        if texture.bpp == 0 { return Err("mock nonlinear texture is not CPU-readable".into()); }
        let mut pixels = texture.pixels.clone();
        let mut control = self.control.borrow_mut();
        if control.wrong_readback_length {
            pixels.pop();
        }
        control.events.push(Event::Read(id.0));
        Ok(pixels)
    }
    pub fn destroy_texture(&mut self, id: &TextureId) -> Result<(), String> {
        if std::mem::take(&mut self.control.borrow_mut().fail_next_retirement) {
            return Err("mock native retirement failed".into());
        }
        if self.control.borrow().active.is_some() {
            return Err("mock destroy inside pass".into());
        }
        self.textures
            .remove(&id.0)
            .ok_or("mock texture double retirement")?;
        self.control.borrow_mut().events.push(Event::Destroy(id.0));
        Ok(())
    }
}

impl Drop for Gxm {
    fn drop(&mut self) {
        self.control.borrow_mut().events.push(Event::Close);
    }
}
