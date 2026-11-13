//! Game data access: archives, exe tables, scripts, sound archive and a GPU texture cache.

use knife_perf::{Metric, Scope};
#[cfg(target_os = "vita")]
use knife_perf::Gauge;
use crate::grade::Bake;
#[cfg(target_os = "vita")]
use std::rc::Rc;
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use knife_audio::{Pcm, bfsar::SoundArchive, codec};
use knife_core::{
    exe::{Build, ExeTables, ImageInfo, Nso},
    format::{
        archive::Archive,
        bntx::{self, TexFormat},
        bntx_decode, compact,
        script::{SceneArchive, ScriptText},
        sfnt,
    },
    vm::parse::Scene,
};
use knife_render::{Renderer, TexRef, Upload};
#[cfg(target_os = "vita")]
use knife_render::ByteView;
#[cfg(target_os = "vita")]
#[path = "atlas_cache.rs"]
mod atlas_cache;
#[cfg(any(target_os = "vita", test))]
#[path = "source_failure.rs"]
mod source_failure;
#[cfg(target_os = "vita")]
#[path = "vita_atlas_index.rs"]
mod atlas_index;
#[cfg(target_os = "vita")]
#[path = "graded_cache.rs"]
mod graded_cache;
#[cfg(target_os="vita")]
#[path="image_needed.rs"]
mod image_needed;
#[cfg(target_os="vita")]
#[path="sound_needed.rs"]
mod sound_needed;
#[cfg(target_os="vita")]
pub use sound_needed::SoundNeeded;
#[cfg(any(target_os = "vita", test))]
#[path = "sparse_source.rs"]
mod sparse_source;
#[cfg(target_os="vita")]
pub use image_needed::ImageNeeded;

/// Image name prefix that loads the save-list thumbnail copy of an image (`Assets::thumb_name`).
pub const THUMB: &str = "THUMB:";

/// A texture together with its display metrics.
pub struct Image {
    pub tex: TexRef,
    /// Display size and centre in 640x362 units.
    pub width: f32,
    pub height: f32,
    pub center_x: f32,
    pub center_y: f32,
    /// ImageInfo +70 scale (`ImageInfo::level_scale`)
    pub level: f32,
    /// ImageInfo +65 bit 1: not moved by the screen shake
    pub no_shake: bool,
}

impl Image {
    /// The stage anchor cmd_STCH_impl gives a layer showing this image: its centre cut to whole units (stage
    /// float 221 + layer = (int)centre, shift word 2923 + 9 * layer = (int)-centre), what the draw zooms and
    /// turns about. Witch on the Holy Night's STCH (jp_60840) takes the exact centre.
    pub fn anchor(&self) -> [f32; 2] {
        if witch() { [self.center_x, self.center_y] } else { [self.center_x.trunc(), self.center_y.trunc()] }
    }
}

/// Decoded pixels of an image, ready to upload.
pub enum Pixels {
    /// As stored; `true`: still to go through pre_alter (`Renderer::upload_altered`)
    Bntx(bntx::Texture, bool),
    /// RGBA8; `Some(srgb)`: still to go through pre_alter, sampled through an sRGB view when `srgb`
    Rgba(u32, u32, Vec<u8>, Option<bool>),
    /// RGBA8 CPU grade; native encoded UNORM on Vita, legacy sRGB storage on desktop
    Baked(u32, u32, Vec<u8>),
    #[cfg(target_os = "vita")]
    SharedBaked(u32, u32, knife_render::PixelView, u64),
    /// Exclusive C allocation, moved from worker to renderer with its C destructor.
    #[cfg(target_os = "vita")]
    Native { width: u32, height: u32, owner: knife_platform_vita::image::DecodedImage,
        range: std::ops::Range<usize>, baked: bool },
    /// Reduced physical pixels, with the original image dimensions used by scripts and layout.
    Scaled(Box<Pixels>, [u32; 2]),
    /// Reuse the worker's first decode, but let hidden CG layers release GPU
    /// residency. Retain an exact archive entry locator, never a second RGBA copy.
    #[cfg(target_os = "vita")]
    Reloadable(Box<Pixels>, ImageReload),
    /// Native encoded UNORM Vita GXT (asset codec9). The worker payload moves to first-use
    /// preparation, with a bounded locator retained for a later GPU eviction.
    #[cfg(target_os = "vita")]
    GxtNative {
        encoded: ByteView, source: ByteView, archive: Arc<Archive>, index: usize, first_use:bool,
        logical: [u32; 2], physical: [u32; 2],
    },
}

#[cfg(target_os = "vita")]
struct NativeSlice {
    owner: knife_platform_vita::image::DecodedImage,
    range: std::ops::Range<usize>,
}
#[cfg(target_os = "vita")]
impl AsRef<[u8]> for NativeSlice {
    fn as_ref(&self) -> &[u8] { &self.owner.pixels()[self.range.clone()] }
}

#[cfg(target_os = "vita")]
pub struct ImageReload {
    archive: Arc<Archive>,
    index: usize,
    encoded_len: usize,
    grade: Bake,
    strip: Option<(usize, usize)>,
    initial: Option<ByteView>,
}

#[cfg(target_os = "vita")]
impl Pixels {
    /// Native allocation plan, before consuming the CPU result. The BNTX
    /// fallback decodes colour textures to RGBA8; raw R8 masks stay one byte.
    fn vita_upload_layout(&self) -> Result<(u32, u32, u32, bool)> {
        let (width, height, bpp, altered, bytes) = match self {
            Self::Scaled(pixels, _) => return pixels.vita_upload_layout(),
            Self::Reloadable(pixels, _) => return pixels.vita_upload_layout(),
            Self::GxtNative { .. } => anyhow::bail!("GXT block upload uses deferred allocation"),
            Self::Bntx(texture, _) => (
                texture.width, texture.height,
                if texture.format == TexFormat::R8Unorm { 1 } else { 4 },
                false, None,
            ),
            Self::Rgba(w, h, px, _) => (*w, *h, 4, false, Some(px.len())),
            Self::Baked(w, h, px) => (*w, *h, 4, false, Some(px.len())),
            Self::SharedBaked(w, h, px, _) => (*w, *h, 4, false, Some(px.len())),
            Self::Native { width, height, owner, range, .. } =>
                (*width, *height, owner.bytes_per_pixel(), false, Some(range.len())),
        };
        anyhow::ensure!(width > 0 && height > 0 && width <= 2048 && height <= 2048,
            "image upload dimensions are outside 1..=2048");
        if let Some(bytes) = bytes {
            let expected = u64::from(width) * u64::from(height) * u64::from(bpp);
            anyhow::ensure!(bytes as u64 >= expected, "decoded image pixels are truncated");
        }
        Ok((width, height, bpp, altered))
    }
}

/// An image `Assets::decode_async` read on a worker thread.
pub struct Decoded {
    #[cfg(target_os="vita")]
    demanded:bool,
    key: String,
    name: String,
    strip: Option<(usize, usize)>,
    pixels: Result<Pixels>,
}

#[cfg(target_os = "vita")]
struct ImageJob {
    initial:Option<(ByteView,Arc<Archive>,usize)>,
    demanded:bool,
    rebake:bool,
    motion:bool,
    key: String,
    name: String,
    grade: Bake,
    strip: Option<(usize, usize)>,
}

// Checked before archive inflation / codec output allocation. The native tier
// stores 960x540 payloads: 2,073,600 B as RGBA8 and typically ~1 MiB as PNG, so
// the old 1 MiB allowance silently excluded every full-screen native image from
// prefetch and pushed all of them onto the demand path. Bilingual strips
// (960x1080) still exceed the RGBA allowance and stay demand-loaded by design.
#[cfg(target_os = "vita")]
const PREFETCH_ENCODED_CAP: usize = 4 * 1024 * 1024;
#[cfg(target_os = "vita")]
const PREFETCH_RGBA_CAP: usize = 3 * 1024 * 1024;

#[cfg(target_os = "vita")]
#[derive(Clone, Copy, Default)]
pub struct ImagePerf {
    pub demand_decodes: u64,
    pub demand_us: u64,
    pub worker_reused: u64,
    pub demand_wait_us: u64,
    pub uploads: u64,
    pub upload_us: u64,
    pub max_upload_us: u64,
    pub skipped: u64,
    /// Images served from a pre-baked GXT payload (no decode, no alter bake).
    pub gxt_images: u64,
    /// Images that still needed PNG/JPEG decode plus an alter bake.
    pub decoded_images: u64,
}

impl Decoded {
    /// The image's cache key (`Assets::image_key`).
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Bounded staging cache for archive reads started on a worker thread. A deferred
/// texture that has to re-read its bytes from the memory card blocked the main thread
/// for 300-700 ms per image; this lets the read overlap the frames before it is sampled.
#[cfg(target_os = "vita")]
#[derive(Default)]
pub struct ByteStage {
    entries: std::sync::Mutex<std::collections::VecDeque<(u64, ByteView)>>,
    in_flight: std::sync::Mutex<std::collections::HashSet<u64>>,
    bytes: std::sync::atomic::AtomicUsize,
    failures: std::sync::Mutex<std::collections::VecDeque<(u64, String)>>,
}

#[cfg(target_os = "vita")]
impl ByteStage {
    const CAP: usize = 24 * 1024 * 1024;

    /// Queue a read unless it is already staged or in flight.
    pub fn request(self: &Arc<Self>, key: u64, archive: Arc<Archive>, index: usize,
        offset: usize, len: usize) {
        self.request_kind(key, archive, index, offset, len, false);
    }

    fn request_kind(self: &Arc<Self>, key: u64, archive: Arc<Archive>, index: usize,
        offset: usize, len: usize, direct: bool) {
        knife_perf::add(Metric::StageRequest, len as u64, 0);
        {
            let entries = self.entries.lock().unwrap();
            if entries.iter().any(|(k, _)| *k == key) {
                knife_perf::add(Metric::StageSkipReady, len as u64, 0);
                return;
            }
        }
        {
            let mut in_flight = self.in_flight.lock().unwrap();
            if !in_flight.insert(key) {
                knife_perf::add(Metric::StageSkipFlight, len as u64, 0);
                return;
            }
        }
        knife_perf::gauge_add(Gauge::StageInflight, 1);
        knife_perf::gauge_add(Gauge::StageRequestedBytes, len as u64);
        let stage = Arc::clone(self);
        let spawned = std::thread::Builder::new().name("byte stage".into()).spawn(move || {
            knife_perf::add(Metric::StageStart, len as u64, 0);
            let read = if len > Self::CAP {
                Err(anyhow::anyhow!("range output {len} exceeds byte-stage budget"))
            } else if direct { archive.read_direct_range(index, offset, len) }
            else { archive.read_uncompressed_range(index, offset, len) };
            match read { Ok(bytes) => {
                let copy = Scope::new(Metric::StageVecArc);
                let arc = ByteView::owned(bytes);
                copy.finish(arc.len() as u64, 0);
                knife_perf::add(Metric::StageComplete, arc.len() as u64, 0);
                let size = arc.len();
                let mut entries = stage.entries.lock().unwrap();
                entries.push_back((key, arc));
                let mut total = stage.bytes.fetch_add(size, std::sync::atomic::Ordering::Relaxed) + size;
                while total > Self::CAP {
                    let Some((_, old)) = entries.pop_front() else { break };
                    knife_perf::add(Metric::StageEvict, old.len() as u64, 0);
                    total -= old.len();
                    stage.bytes.fetch_sub(old.len(), std::sync::atomic::Ordering::Relaxed);
                }
                knife_perf::gauge_set(Gauge::StageReadyBytes, total as u64);
                stage.failures.lock().unwrap_or_else(|e| e.into_inner()).retain(|(old, _)| *old != key);
            }, Err(read_error) => {
                let error = format!("{}:{index} offset={offset} len={len} direct={direct}: {read_error:#}",
                    archive.name);
                tracing::warn!(%error, "bounded range read failed");
                let mut errors = stage.failures.lock().unwrap_or_else(|e| e.into_inner());
                errors.retain(|(old, _)| *old != key);
                while errors.len() >= 64 { errors.pop_front(); }
                errors.push_back((key, error));
                knife_perf::add(Metric::StageFailed, len as u64, 0);
            }}
            stage.in_flight.lock().unwrap().remove(&key);
            knife_perf::gauge_sub(Gauge::StageInflight, 1);
            knife_perf::gauge_sub(Gauge::StageRequestedBytes, len as u64);
        });
        if spawned.is_err() {
            knife_perf::add(Metric::StageSpawnFailed, len as u64, 0);
            knife_perf::gauge_sub(Gauge::StageInflight, 1);
            knife_perf::gauge_sub(Gauge::StageRequestedBytes, len as u64);
            self.in_flight.lock().unwrap().remove(&key);
        }
    }

    pub fn trim_lru(&self, need:usize) -> usize {
        if need==0 {return 0;}
        let Ok(mut entries)=self.entries.try_lock()else{return 0};let mut reclaimed=0;let mut i=0;
        while i<entries.len() && reclaimed<need {
            if entries[i].1.is_unpinned() {
                let (_,old)=entries.remove(i).unwrap();reclaimed+=old.len();
                self.bytes.fetch_sub(old.len(),std::sync::atomic::Ordering::Relaxed);
            }else{i+=1;}
        }
        reclaimed
    }

    /// Legacy owned-byte reload: join its one existing background request.
    /// A deadline never clears in_flight or spawns a duplicate card read.
    pub fn load_bounded(self:&Arc<Self>,key:u64,archive:Arc<Archive>,index:usize,offset:usize,len:usize)->Result<ByteView> {
        self.load_kind(key,archive,index,offset,len,false)
    }

    fn load_direct_bounded(self:&Arc<Self>,key:u64,archive:Arc<Archive>,index:usize,offset:usize,len:usize)->Result<ByteView> {
        self.load_kind(key,archive,index,offset,len,true)
    }

    fn load_kind(self:&Arc<Self>,key:u64,archive:Arc<Archive>,index:usize,offset:usize,len:usize,direct:bool)->Result<ByteView> {
        self.request_kind(key,archive,index,offset,len,direct);
        let started=std::time::Instant::now();
        loop {
            if let Some(bytes)=self.take(key){return Ok(bytes);}
            let active=self.in_flight.lock().map_err(|_|anyhow::anyhow!("byte-stage lock poisoned"))?.contains(&key);
            if !active {
                if let Some(bytes)=self.take(key){return Ok(bytes);}
                if let Some((_, error))=self.failures.lock().unwrap_or_else(|e|e.into_inner()).iter().find(|(old,_)|*old==key) {
                    anyhow::bail!("byte-stage read failed: {error}");
                }
                anyhow::bail!("byte-stage read failed key={key:016x} entry={index}");
            }
            anyhow::ensure!(started.elapsed()<std::time::Duration::from_secs(10),
                "byte-stage wait deadline (10s) key={key:016x} entry={index}; active read retained");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// Take staged bytes, if the worker finished them.
    pub fn take(&self, key: u64) -> Option<ByteView> {
        let mut entries = self.entries.lock().ok()?;
        let Some(index) = entries.iter().position(|(k, _)| *k == key) else {
            knife_perf::add(Metric::StageMiss, 0, 0);
            if knife_perf::enabled() && self.in_flight.lock().ok().is_some_and(|keys| keys.contains(&key)) {
                knife_perf::add(Metric::StageMissFlight, 0, 0);
            }
            return None;
        };
        let (_, bytes) = entries.remove(index)?;
        let left = self.bytes.fetch_sub(bytes.len(), std::sync::atomic::Ordering::Relaxed) - bytes.len();
        knife_perf::gauge_set(Gauge::StageReadyBytes, left as u64);
        knife_perf::add(Metric::StageHit, bytes.len() as u64, 0);
        Some(bytes)
    }
}

/// Stable key for a staged archive range.
#[cfg(target_os = "vita")]
fn byte_stage_key(archive: &str, index: usize, offset: usize, len: usize) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in archive.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
    }
    for value in [index as u64, offset as u64, len as u64] {
        for byte in value.to_le_bytes() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
        }
    }
    hash
}

pub struct Assets {
    pub romfs: PathBuf,
    #[cfg(target_os = "vita")]
    stream_resolver: crate::vita_stream_index::StreamResolver,
    /// Japanese or overseas data (`detect_build`)
    pub build: Build,
    /// The game language (sub_2DE70: config word +0x12 when it is below `languages`, else the system language):
    /// 0 Japanese, 1 English, 2 Simplified Chinese, 3 Traditional Chinese
    language: usize,
    /// sub_9A700: the language count of the SYSMES file (1 Japanese, 4 overseas)
    languages: usize,
    language_profile: Option<crate::languages::Profile>,
    script_text: Vec<u8>,
    #[cfg(target_os = "vita")]
    pub byte_stage: Arc<ByteStage>,
    #[cfg(target_os = "vita")]
    atlas_cache: Arc<atlas_cache::AtlasCache>,
    #[cfg(target_os = "vita")]
    cpu_cache_pressure: std::cell::Cell<bool>,
    pub allpac: Arc<Archive>,
    /// The second image archive (archive id 10): empty in the Japanese data; the overseas one holds the save
    /// list pictures IMG6000..6026 and 140 other images allpac lacks, with their thumbnails
    pub allpacml: Option<Arc<Archive>>,
    pub allpachd: Option<Arc<Archive>>,
    /// Witch on the Holy Night's allpachdml (archive 11): the HD pictures of more than one language
    pub allpachdml: Option<Arc<Archive>>,
    pub allpaccg: Option<Arc<Archive>>,
    pub allui: Arc<Archive>,
    pub tables: ExeTables,
    image_index: HashMap<String, usize>,
    pub scenes: SceneArchive,
    pub text: ScriptText,
    pub sar: Arc<SoundArchive>,
    parsed: HashMap<usize, Arc<Scene>>,
    images: HashMap<String, Option<Arc<Image>>>,
    #[cfg(target_os = "vita")]
    image_cache: crate::vita_image_cache::ImageCachePolicy,
    #[cfg(target_os = "vita")]
    image_worker: Option<knife_platform_vita::image_worker::BoundedWorker<ImageJob, Option<Decoded>>>,
    #[cfg(target_os="vita")]
    image_context:u64,
    #[cfg(target_os="vita")]
    image_needed:Option<image_needed::NeededRequest>,
    #[cfg(target_os = "vita")]
    image_skipped: std::collections::HashSet<String>,
    #[cfg(target_os = "vita")]
    image_perf: ImagePerf,
    sounds: RefCell<knife_audio::pcm_cache::PcmCache>,
    #[cfg(target_os="vita")]
    sound_worker: Option<knife_platform_vita::image_worker::BoundedWorker<sound_needed::Job, sound_needed::DecodedSound>>,
    #[cfg(target_os="vita")]
    sound_context: u64,
    #[cfg(target_os="vita")]
    sound_demand: Option<sound_needed::Demand>,
    #[cfg(target_os="vita")]
    sound_prepared: HashMap<String, Option<Arc<Pcm>>>,
    /// Indexed by allui entry, so EN/ZC variants and language-neutral atlases do not collide.
    /// A cached first texture identifies the owning Renderer before reuse.
    #[cfg(target_os = "vita")]
    ui_atlases: RefCell<HashMap<usize, Vec<(String, TexRef)>>>,
    /// Parsed SYSMES tables by language slot. Without this every caller retried a
    /// failed parse once per frame, re-reading the whole table from the card.
    sysmes_cache: RefCell<HashMap<usize, Vec<String>>>,
    /// When a parse failed, so a broken table is retried at most twice a second.
    sysmes_retry: RefCell<HashMap<usize, std::time::Instant>>,
}

/// The build a romfs belongs to: the overseas data names its archives `w_allpac` / `w_allui` / ... (sub_AE050),
/// Witch on the Holy Night's `jallpac` / `jallpachd` (WoH sub_89A80).
pub fn detect_build(romfs: &Path) -> Build {
    if romfs.join("w_allpac.nam").exists() {
        Build::Overseas
    } else if romfs.join("jallpac.nam").exists() {
        Build::Witch
    } else {
        Build::Japanese
    }
}

/// The executable's tables for a build, compiled in (`kdu exe-tables` writes them; knife_core::exe).
pub fn exe_tables(build: Build) -> Result<Nso> {
    Nso::from_bundle(match build {
        Build::Japanese => include_bytes!("../data/exe_tables.bin"),
        Build::Overseas => include_bytes!("../data/exe_tables_overseas.bin"),
        Build::Witch => include_bytes!("../data/exe_tables_witch.bin"),
    })
}

/// sub_2DE00: the game language of the console's desired language (nn::oe::GetDesiredLanguage, limited to the
/// languages the title declares; the Japanese build declares only Japanese): Japanese 0, Simplified Chinese 2,
/// Traditional Chinese 3, any other language 1 (English). knife takes the host locale for the console's.
pub fn system_language(build: Build) -> i16 {
    if build == Build::Japanese {
        return 0;
    }
    let locale = sys_locale::get_locale().unwrap_or_default().to_ascii_lowercase().replace('_', "-");
    let mut parts = locale.split('-');
    match parts.next().unwrap_or("") {
        "ja" => 0,
        "zh" => {
            let rest: Vec<&str> = parts.collect();
            // zh-Hant / zh-TW / zh-HK / zh-MO are Traditional, other Chinese locales Simplified
            if rest.iter().any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo")) { 3 } else { 2 }
        }
        _ => 1,
    }
}

/// The Japanese release's application title.
pub const TITLE: &str = "月姫 -A piece of blue glass moon-";
/// The overseas release's titles by game language (its control.nacp: Japanese, American English, Simplified and
/// Traditional Chinese entries; the Chinese ones write 月姬, U+59EC).
const OVERSEAS_TITLES: [&str; 4] =
    [TITLE, "TSUKIHIME A piece of blue glass moon", "月姬 -A piece of blue glass moon-", "月姬 -A piece of blue glass moon-"];

/// Witch on the Holy Night's application titles by game language.
const WITCH_TITLES: [&str; 4] = ["魔法使いの夜", "WITCH ON THE HOLY NIGHT", "魔法使之夜", "魔法使之夜"];

/// The SYSMES message the overseas build inserts at 217 (the "deleted" popup, `jp_50FC0` state 25). knife keeps
/// the Japanese numbering and moves it here, past the Japanese build's 1498 messages.
pub const SYSMES_DELETED: usize = 1498;
const SYSMES_OVERSEAS_INSERT: usize = 217;

/// The loaded executable is the overseas build (for tables that differ between the builds).
static OVERSEAS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn overseas() -> bool {
    OVERSEAS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The loaded executable is Witch on the Holy Night's.
static WITCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Record the build before anything reads the save file, whose layout depends on it (Assets::open sets the
/// same flags).
pub fn select_build(build: Build) {
    OVERSEAS.store(build == Build::Overseas, std::sync::atomic::Ordering::Relaxed);
    WITCH.store(build == Build::Witch, std::sync::atomic::Ordering::Relaxed);
}

pub fn witch() -> bool {
    #[cfg(test)]
    if TEST_WITCH.get() {
        return true;
    }
    WITCH.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
thread_local! {
    /// Tests of Witch on the Holy Night's tables switch the build on for their own thread.
    pub static TEST_WITCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Assets {
    /// `language` is the config word (`cfg::LANGUAGE`); `language()` resolves it like sub_2DE70.
    pub fn open(romfs: &Path, nso: &Nso, language: i16) -> Result<Self> {
        let tables = ExeTables::load(nso)?;
        let build = tables.build;
        // sub_AE050: the overseas archives allpac / allpacml / allui / allpaccg carry a `w_` prefix; Witch on the
        // Holy Night (WoH sub_89A80) opens jallpac (8), allpacml (9), jallpachd (10), allpachdml (11), allui (12)
        // and parts (25), with no CG archive
        let w = |base: &str| match build {
            Build::Overseas => format!("w_{base}"),
            Build::Witch if base == "allpac" => "jallpac".to_string(),
            Build::Japanese | Build::Witch => base.to_string(),
        };
        let hd = if build == Build::Witch { "jallpachd" } else { "allpachd" };
        let image_index = tables
            .images
            .iter()
            .enumerate()
            .map(|(i, im)| (im.name.to_ascii_uppercase(), i))
            .collect();
        let scenes = SceneArchive::parse(&std::fs::read(romfs.join("allscr.mrg")).context("reading allscr.mrg")?)?;
        let script_text = std::fs::read(romfs.join("script_text.mrg")).context("reading script_text.mrg")?;
        let sar_name = if build == Build::Witch { "witch.bfsar" } else { "knife1.bfsar" };
        #[cfg(target_os = "vita")]
        let sar = SoundArchive::open_file(&romfs.join(sar_name)).with_context(|| format!("opening bounded {sar_name}"))?;
        #[cfg(not(target_os = "vita"))]
        let sar = SoundArchive::parse(std::fs::read(romfs.join(sar_name)).with_context(|| format!("reading {sar_name}"))?)?;
        let allui = Arc::new(Archive::open(romfs, &w("allui"))?);
        let languages = Self::sysmes_file(&allui, build).map_or(1, |d| sysmes_u32(&d, 0).unwrap_or(1).max(1) as usize);
        let language_profile = crate::languages::Profile::load(romfs)?;
        if language_profile.is_some() { anyhow::ensure!(languages == 2, "language profile/resource count mismatch"); }
        let mut a = Self {
            romfs: romfs.to_path_buf(),
            #[cfg(target_os = "vita")]
            stream_resolver: crate::vita_stream_index::StreamResolver::new(romfs,
                crate::vita_stream_index::EMBEDDED, knife_platform_vita::audit::ASSET_MANIFEST),
            build,
            language: usize::MAX,
            languages,
            language_profile,
            text: ScriptText::parse(&script_text, 0)?,
            script_text,
            #[cfg(target_os = "vita")]
        byte_stage: Arc::new(ByteStage::default()),
        #[cfg(target_os = "vita")]
        atlas_cache: Arc::new(atlas_cache::AtlasCache::default()),
        #[cfg(target_os = "vita")]
        cpu_cache_pressure: std::cell::Cell::new(false),
        allpac: Arc::new(Archive::open(romfs, &w("allpac"))?),
            allpacml: Archive::open(romfs, &w("allpacml")).ok().map(Arc::new),
            allpachd: Archive::open(romfs, hd).ok().map(Arc::new),
            allpachdml: if build == Build::Witch { Archive::open(romfs, "allpachdml").ok().map(Arc::new) } else { None },
            allpaccg: if build == Build::Witch { None } else { Archive::open(romfs, &w("allpaccg")).ok().map(Arc::new) },
            allui,
            tables,
            image_index,
            scenes,
            sar: Arc::new(sar),
            parsed: HashMap::new(),
            images: HashMap::new(),
            #[cfg(target_os = "vita")]
            image_cache: Default::default(),
            #[cfg(target_os = "vita")]
            image_worker: None,
            #[cfg(target_os="vita")]
            image_context:1,
            #[cfg(target_os="vita")]
            image_needed:None,
            #[cfg(target_os = "vita")]
            image_skipped: Default::default(),
            #[cfg(target_os = "vita")]
            image_perf: ImagePerf::default(),
            sounds: RefCell::new(knife_audio::pcm_cache::PcmCache::default()),
            #[cfg(target_os="vita")]
            sound_worker: None,
            #[cfg(target_os="vita")]
            sound_context: 1,
            #[cfg(target_os="vita")]
            sound_demand: None,
            #[cfg(target_os="vita")]
            sound_prepared: HashMap::new(),
            #[cfg(target_os = "vita")]
            ui_atlases: RefCell::new(HashMap::new()),
        sysmes_cache: RefCell::new(HashMap::new()),
        sysmes_retry: RefCell::new(HashMap::new()),
        };
        crate::text::set_line_break(a.tables.line_break.clone());
        OVERSEAS.store(a.build == Build::Overseas, std::sync::atomic::Ordering::Relaxed);
        WITCH.store(a.build == Build::Witch, std::sync::atomic::Ordering::Relaxed);
        // the first language may be the one the text was parsed in, which `set_language` skips: the fonts'
        // metrics still apply (Witch on the Holy Night's Japanese font pairs to a 49 px line feed, not 50)
        if !a.set_language(language)? {
            a.apply_font_metrics();
        }
        #[cfg(target_os = "vita")]
        {
            let archives = a.image_archives();
            let source_cache = a.atlas_cache.clone();
            a.image_worker = match knife_platform_vita::image_worker::BoundedWorker::new(8, 2,
                move |job: ImageJob| {
                    let bake_probe=if job.motion { Some(Scope::new(Metric::MotionVariantDecode)) }
                        else { job.rebake.then(||Scope::new(Metric::BakeRebuild)) };
                    let bake_name=(job.rebake || job.motion).then(||job.name.clone());
                    let result=(|| {
                    if let Some((data,archive,index))=job.initial {
                        knife_platform_vita::image_worker::resource_state::image_location(archive.cache_identity(),index);
                        let pixels=Self::decode_shared_data_pixels(data,archive,index,&job.name,job.grade,job.strip,
                            if job.demanded {None}else{Some(PREFETCH_RGBA_CAP)});
                        return Some(Decoded {demanded:job.demanded,key:job.key,name:job.name,strip:job.strip,pixels});
                    }
                    if knife_perf::optimizations::enabled(knife_perf::optimizations::SHARED_ATLAS) {
                        let pixels = Self::decode_source(&source_cache,&archives,&job.name,job.grade,job.strip,
                            Some((PREFETCH_ENCODED_CAP,PREFETCH_RGBA_CAP)),true);
                        return Some(Decoded {demanded:job.demanded,key:job.key,name:job.name,strip:job.strip,pixels});
                    }
                    let (data, archive, index) = Self::image_data_limit(&archives, &job.name,
                        Some(PREFETCH_ENCODED_CAP))?;
                    // Non-compact / oversized atlases remain demand-loaded.
                    let image = compact::parse(&data).ok()?;
                    // A pre-baked GXT entry never produces an RGBA buffer, so the
                    // decoded-pixel allowance must not exclude it: 960x540 would be
                    // 2,073,600 B of nonexistent RGBA and every native image would
                    // silently fall back to the demand path.
                    let bakes_rgba = image.encoding != compact::Encoding::GxtNativeUnorm
                        || job.grade.cpu() || job.grade.raw || job.strip.is_some();
                    if bakes_rgba
                        && u64::from(image.size[0]) * u64::from(image.size[1]) * 4 > PREFETCH_RGBA_CAP as u64
                    {
                        return None;
                    }
                    let pixels = Self::decode_data_pixels(data, archive, index,
                        &job.name, job.grade, job.strip, Some(PREFETCH_RGBA_CAP));
                    Some(Decoded { demanded:job.demanded, key: job.key, name: job.name, strip: job.strip, pixels })
                    })();
                    if let Some(probe)=bake_probe {
                        let ok=result.as_ref().is_some_and(|decoded|decoded.pixels.is_ok());
                        probe.finish_hot(bake_name.as_deref().unwrap_or(""),0,u64::from(ok));
                    }
                    result
                }) {
                Ok(worker) => Some(worker),
                Err(e) => { tracing::warn!("bounded image worker unavailable: {e}"); None }
            };
        }
        #[cfg(target_os="vita")]
        a.start_sound_worker();
        Ok(a)
    }

    /// Hand the language's font metrics (`font_metrics`) to the text layout.
    fn apply_font_metrics(&self) {
        let (msg, bubble) = self.font_metrics();
        // Tsukihime's bubble font is in the message font list (`<folder>3`), whose line feed it shares
        let bubble_feed = if self.build == Build::Witch { bubble.line_feed } else { msg.line_feed };
        crate::text::set_font_metrics(msg.line_feed, msg.ascent, msg.baseline, bubble.ascent, bubble_feed);
    }

    /// sub_2DE70: the game language for a config word (negative or past the data's languages: the system's).
    pub fn resolve_language(&self, word: i16) -> usize {
        // This Vita distribution fixes the original Chinese ID (2); compact
        // profile mapping still selects physical language slot1, never ID1.
        if cfg!(target_os = "vita") { return 2; }
        if let Some(profile) = &self.language_profile {
            return profile.resolve(word, system_language(self.build));
        }
        if word >= 0 && (word as usize) < self.languages {
            word as usize
        } else {
            system_language(self.build) as usize
        }
    }

    /// Physical resource slot, independent of the IDs persisted in existing saves.
    pub fn language_slot(&self, id: usize) -> usize {
        self.language_profile.as_ref().map_or(id, |profile| profile.slot(id))
    }

    pub fn language_strip(&self) -> (usize, usize) {
        (self.language_slot(self.language), self.languages)
    }

    pub fn next_language(&self, word: i16, forward: bool) -> i16 {
        if cfg!(target_os = "vita") { return 2; }
        if let Some(profile) = &self.language_profile {
            return profile.cycle(self.resolve_language(word), forward);
        }
        let n = self.languages as i16;
        (if forward { word + 1 } else { word - 1 }).rem_euclid(n)
    }

    pub fn language(&self) -> usize {
        self.language
    }

    /// The application title in the game language, as the console shows it (`OVERSEAS_TITLES`; the Japanese
    /// release has its one title).
    pub fn title(&self) -> &'static str {
        match self.build {
            Build::Overseas => OVERSEAS_TITLES.get(self.language).copied().unwrap_or(TITLE),
            Build::Witch => WITCH_TITLES.get(self.language).copied().unwrap_or(WITCH_TITLES[0]),
            Build::Japanese => TITLE,
        }
    }

    /// Switch to the language of a config word: the script text pair (sub_53CF0 reads script_text.mrg entries
    /// 2 * language / 2 * language + 1) and the language-strip images (sub_3F130 / sub_3F1A0 rebuild them).
    /// Returns whether the language changed.
    pub fn set_language(&mut self, word: i16) -> Result<bool> {
        let language = self.resolve_language(word);
        if language == self.language && !self.text.is_empty() {
            return Ok(false);
        }
        self.text = ScriptText::parse(&self.script_text, self.language_slot(language))?;
        self.language = language;
        #[cfg(target_os="vita")]
        self.reset_image_needed_context();
        #[cfg(target_os = "vita")]
        {
            self.ui_atlases.get_mut().clear();
            self.image_skipped.clear();
            if let Some(worker) = &self.image_worker {
                worker.retain_window(self.image_context, &[]);
            }
        }
        crate::text::set_language(language);
        self.apply_font_metrics();
        let strips: Vec<String> = self
            .images
            .keys()
            .filter(|k| self.image_info(k.split('#').next().unwrap_or(k)).is_some_and(|i| i.language_strips))
            .cloned()
            .collect();
        for k in strips {
            self.images.remove(&k);
        }
        #[cfg(target_os = "vita")]
        self.image_cache.retain_keys(|key| self.images.contains_key(key));
        Ok(true)
    }

    pub fn scene(&mut self, index: usize) -> Option<Arc<Scene>> {
        if let Some(s) = self.parsed.get(&index) {
            knife_perf::add(Metric::ScriptSceneCacheHit, 0, index as u64);
            return Some(s.clone());
        }
        let read = Scope::new(Metric::ScriptSceneRead);
        let raw = self.scenes.scene(index)?;
        read.finish_hot(self.scene_name(index), raw.len() as u64, index as u64);
        let parse = Scope::new(Metric::ScriptSceneParse);
        let s = Arc::new(Scene::parse(raw));
        parse.finish_hot(self.scene_name(index), 0, index as u64);
        self.parsed.insert(index, s.clone());
        Some(s)
    }

    pub fn scene_name(&self, index: usize) -> &str {
        self.scenes.names.get(index).map(|s| s.as_str()).unwrap_or("?")
    }

    /// Scene index for a script `JUMP` id.
    pub fn scene_by_id(&self, id: i32) -> Option<usize> {
        self.tables.scene_index(u16::try_from(id).ok()?)
    }

    pub fn line(&self, index: usize) -> &str {
        self.text.get(index).unwrap_or("")
    }

    /// Exact image table index of a name (sub_7DFB0).
    pub fn image_index_of(&self, name: &str) -> Option<usize> {
        self.image_index.get(&name.to_ascii_uppercase()).copied()
    }

    /// The image a save-list preview draws for a layer showing `name` (sub_895A0): its thumbnail copy, and
    /// for an `_aa_bb` variant (ImageInfo +67 bit 0) the base image's, the name less its last 6 characters.
    /// The exe moves the anchor by the centre difference, so the base lands centred where the variant was.
    pub fn thumb_name(&self, name: &str) -> String {
        let upper = name.to_ascii_uppercase();
        let upper = upper.strip_prefix(THUMB).unwrap_or(&upper);
        let base = match self.image_index_of(upper).and_then(|i| self.tables.images.get(i)) {
            Some(im) if im.variant && upper.len() > 6 => &upper[..upper.len() - 6],
            _ => upper,
        };
        format!("{THUMB}{base}")
    }

    pub fn image_info(&self, name: &str) -> Option<&ImageInfo> {
        let upper = name.to_ascii_uppercase();
        let upper = upper.strip_prefix(THUMB).unwrap_or(&upper).to_string();
        if let Some(&i) = self.image_index.get(&upper) {
            return self.tables.images.get(i);
        }
        // expression variants (`NAME_AA_BB`) share the body's metrics
        let mut base = upper.as_str();
        for _ in 0..2 {
            match base.rsplit_once('_') {
                Some((b, _)) => base = b,
                None => break,
            }
            if let Some(&i) = self.image_index.get(base) {
                return self.tables.images.get(i);
            }
        }
        None
    }

    /// The image archives in lookup order: jp_32360 resolves a name with jp_A90C0(name, -1), which searches
    /// allpac (archive 9), then allpacml (10), then the others; allpac and allpacml come first here for
    /// `image_bytes`' thumbnails.
    fn image_archives(&self) -> Vec<Arc<Archive>> {
        [Some(&self.allpac), self.allpacml.as_ref(), self.allpaccg.as_ref(), Some(&self.allui), self.allpachd.as_ref()]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }

    /// Witch on the Holy Night's HD lookup (jp_A90C0(name, 1)): jallpachd's list (jd_18C0514 = 2), then
    /// allpachdml's (3), then allui's (4).
    fn hd_archives(&self) -> Vec<Arc<Archive>> {
        [self.allpachd.as_ref(), self.allpachdml.as_ref(), Some(&self.allui)].into_iter().flatten().cloned().collect()
    }

    /// Whether jp_A90C0(name, 1) finds `name` (CG mode's pictures).
    pub fn has_image_hd(&self, name: &str) -> bool {
        self.hd_archives().iter().any(|a| a.find_stem(name).is_some())
    }

    /// A picture through the HD lookup (CG mode's textures 285 / 286, jp_B6A20 after sub_83E00(2 / 3)), sized
    /// by its texels: the HD files are stored at the 1920x1080 size the tables give.
    pub fn image_hd(&mut self, r: &Renderer, name: &str) -> Option<Arc<Image>> {
        let (name, grade, key) = Self::image_key(name, Bake::NONE);
        let key = format!("HD {key}");
        if let Some(i) = self.images.get(&key).and_then(Option::as_ref) {
            #[cfg(target_os = "vita")]
            self.image_cache.touch(&key);
            return Some(i.clone());
        }
        #[cfg(target_os = "vita")]
        let pixels = if knife_perf::optimizations::enabled(knife_perf::optimizations::SHARED_ATLAS) {
            Self::decode_source(&self.atlas_cache,&self.hd_archives(),&name,grade,self.strip(&name),None,false)
        } else { Self::decode_pixels(&self.hd_archives(),&name,grade,self.strip(&name)) };
        #[cfg(not(target_os = "vita"))]
        let pixels = Self::decode_pixels(&self.hd_archives(), &name, grade, self.strip(&name));
        self.finish_image_sized(r, key, &name, pixels, true)
    }

    /// A normal entry's decoded bytes and exact archive locator. The locator
    /// lets Vita reopen one KIMG after a deferred GPU cache eviction.
    fn image_data(archives: &[Arc<Archive>], name: &str) -> Option<(Vec<u8>, Arc<Archive>, usize)> {
        Self::image_data_limit(archives, name, None)
    }

    /// Resolve only the original priority/thumbnail mapping, without file IO.
    /// A cached lower-priority archive must never bypass a preferred source.
    fn primary_image_locator(archives: &[Arc<Archive>], name: &str) -> Option<(Arc<Archive>, usize)> {
        if let Some(stem) = name.strip_prefix(THUMB) {
            let ml = archives.iter().find(|a| a.name.ends_with("allpacml"));
            let (a, base, thumbs) = [(archives.first(), "BG_TOP", "THUMSTAND_TOP"), (ml, "BGML_TOP", "THUMSTANDML_TOP")]
                .into_iter()
                .find_map(|(a, base, thumbs)| a.filter(|a| a.find_stem(stem).is_some()).map(|a| (a, base, thumbs)))?;
            let i = a.find_stem(stem)?.checked_sub(a.find_stem(base)? + 1)? + a.find_stem(thumbs)? + 1;
            Some((a.clone(), i))
        } else {
            archives.iter().find_map(|a| a.find_stem(name).map(|i| (a.clone(), i)))
        }
    }

    fn image_data_limit(archives: &[Arc<Archive>], name: &str, cap: Option<usize>) -> Option<(Vec<u8>, Arc<Archive>, usize)> {
        let read = |a: &Archive, i| match cap {
            Some(cap) => a.read_bounded(i, cap),
            None => a.read(i),
        };
        if let Some(stem) = name.strip_prefix(THUMB) {
            // sub_895A0: the layer's allpac entry - (bg_top.mzp + 1) + (thumstand_top.mzp + 1), a copy of the
            // art at 1/6 size (a 4x4 transparent DUMMY.NXZ for the `_aa_bb` variants and IMG7000 / IMG7001);
            // an allpacml image (archive 10 / 12) its entry - (bgml_top + 1) + (thumstandml_top + 1) there
            let (a, i) = Self::primary_image_locator(archives, name)?;
            return read(&a, i).inspect_err(|e| tracing::warn!("reading thumbnail {stem}: {e}"))
                .ok().map(|bytes| (bytes, a, i));
        }
        for a in archives {
            if let Some(i) = a.find_stem(name) {
                match read(a, i) {
                    Ok(d) => return Some((d, Arc::clone(a), i)),
                    // Speculative budget failure cannot fall through to a
                    // different archive (including a higher resolution copy).
                    Err(_) if cap.is_some() => return None,
                    Err(e) => tracing::warn!("reading {name} from {}: {e}", a.name),
                }
            }
        }
        None
    }

    /// Load (and cache) an image by script name. Returns None when it does not exist.
    pub fn image(&mut self, r: &Renderer, name: &str) -> Option<Arc<Image>> {
        self.image_graded(r, name, Bake::NONE)
    }

    /// Load (and cache) a rule mask (RULE_nn), which the transitions sample as stored (`Bake::raw`).
    pub fn rule(&mut self, r: &Renderer, name: &str) -> Option<Arc<Image>> {
        self.image_graded(r, name, Bake::RAW)
    }

    /// Load (and cache) an image built with an STEC colour grade and the SNX2 / SNX3 tone (`grade::Bake`). The
    /// exe keys its texture cache (sub_AAA40) the same way, by image and the pre_alter parameters.
    pub fn image_graded(&mut self, r: &Renderer, name: &str, bake: Bake) -> Option<Arc<Image>> {
        let (name, grade, key) = Self::image_key(name, bake);
        if let Some(i) = self.images.get(&key).and_then(Option::as_ref) {
            #[cfg(target_os = "vita")]
            self.image_cache.touch(&key);
            return Some(i.clone());
        }
        #[cfg(target_os = "vita")]
        let prefetched = {
            let start = std::time::Instant::now();
            let result=match self.image_worker.as_ref().map(|worker|worker.take_for(&key)) {
                Some(Ok(result))=>result.flatten(),
                Some(Err(error))=>{tracing::error!(name=%name,"bounded legacy image wait: {error}");return None;},
                None=>None,
            };
            self.image_perf.demand_wait_us += start.elapsed().as_micros() as u64;
            result.filter(|decoded| decoded.strip == self.strip(&name))
        };
        #[cfg(target_os = "vita")]
        if let Some(decoded) = prefetched {
            self.image_perf.worker_reused += 1;
            return self.finish_image(r, key, &name, decoded.pixels);
        }
        #[cfg(target_os = "vita")]
        let start = std::time::Instant::now();
        #[cfg(target_os = "vita")]
        let pixels = if knife_perf::optimizations::enabled(knife_perf::optimizations::SHARED_ATLAS) {
            Self::decode_source(&self.atlas_cache,&self.image_archives(),&name,grade,self.strip(&name),None,false)
        } else { Self::decode_pixels(&self.image_archives(),&name,grade,self.strip(&name)) };
        #[cfg(not(target_os = "vita"))]
        let pixels = Self::decode_pixels(&self.image_archives(), &name, grade, self.strip(&name));
        #[cfg(target_os = "vita")]
        {
            self.image_perf.demand_decodes += 1;
            self.image_perf.demand_us += start.elapsed().as_micros() as u64;
        }
        self.finish_image(r, key, &name, pixels)
    }

    /// Cache key of an image load: (upper-case name, effective bake, key).
    pub fn image_key(name: &str, bake: Bake) -> (String, Bake, String) {
        let bake = Bake { grade: if crate::grade::is_grade(bake.grade) { bake.grade } else { 0 }, ..bake };
        let name = name.to_ascii_uppercase();
        let key = format!("{name}{}", bake.key());
        (name, bake, key)
    }

    /// Whether `image_graded(name, bake)` would return without loading anything.
    pub fn image_cached(&self, name: &str, bake: Bake) -> bool {
        self.images.get(&Self::image_key(name, bake).2).is_some_and(Option::is_some)
    }

    /// Read and decode an image for `image_graded` on a worker thread; the result arrives on `done`, for
    /// `finish_decoded` to upload and cache. Returns the cache key.
    #[cfg(not(target_os = "vita"))]
    pub fn decode_async(&self, name: &str, bake: Bake, done: std::sync::mpsc::Sender<Decoded>) {
        let (name, grade, k) = Self::image_key(name, bake);
        let archives = self.image_archives();
        let strip = self.strip(&name);
        let spawned = std::thread::Builder::new().name("image decode".into()).spawn(move || {
            let pixels = Self::decode_pixels(&archives, &name, grade, strip);
            let _ = done.send(Decoded { key: k, name, strip, pixels });
        });
        if let Err(e) = spawned {
            tracing::warn!("image decode thread: {e}");
        }
    }

    /// Upload and cache an image `decode_async` read.
    pub fn finish_decoded(&mut self, r: &Renderer, d: Decoded) {
        // a language change while it decoded makes a language-strip image stale
        if !self.images.get(&d.key).is_some_and(Option::is_some) && d.strip == self.strip(&d.name) {
            self.finish_image(r, d.key, &d.name, d.pixels);
        }
    }

    #[cfg(target_os = "vita")]
    pub fn vita_trim_cpu_caches(&self, need:usize) -> usize {
        if need==0 {return 0;}
        // Do not immediately refill optional CPU owners just dropped under
        // pressure. Maintenance clears this latch only above the high water.
        self.cpu_cache_pressure.set(true);
        let mut released=self.sounds.borrow_mut().trim_idle(need);
        released+=self.byte_stage.trim_lru(need.saturating_sub(released));
        released+=self.atlas_cache.trim_lru(need.saturating_sub(released),false);
        released+=graded_cache::trim_lru(need.saturating_sub(released));
        released+=crate::grade::trim_cache(need.saturating_sub(released));
        // UI is a soft priority, not a pin: an extreme shortage can still
        // reclaim it after ordinary LRU owners in every CPU pool.
        released+=self.atlas_cache.trim_lru(need.saturating_sub(released),true);
        self.atlas_cache.record_hot_retained();
        released
    }

    /// Rewarm only the current-language story UI, on the existing source worker.
    /// No header/card read, wait, clone of payload, or eviction happens here.
    /// A 2 MiB scratch margin and the 48 MiB recovery target stay untouched.
    #[cfg(target_os = "vita")]
    pub fn vita_warm_ui_sources(&self, heap_headroom:usize) {
        use knife_perf::memory_budget::{CPU_TARGET_HEADROOM,MIB};
        let floor=CPU_TARGET_HEADROOM+2*MIB;
        if heap_headroom<floor {return;}
        self.cpu_cache_pressure.set(false);
        if !knife_perf::optimizations::enabled(knife_perf::optimizations::SHARED_ATLAS) {return;}
        let mut available=heap_headroom-floor;
        for name in [self.ui_atlas("DISP_PARTS"),"NOISE_PARTS".into()] {
            let Some(index)=self.allui.find_stem(&name) else {continue;};
            let Some(atlas)=atlas_index::find(knife_platform_vita::audit::ASSET_MANIFEST,index,
                &self.allui.entries()[index]) else {continue;};
            if !atlas.direct && atlas.decoded_bytes<=available {
                let queued=self.atlas_cache.prewarm_if_room(self.allui.clone(),index,atlas.decoded_bytes);
                available=available.saturating_sub(queued);
            }
        }
    }

    // Resolve the atlases that must be ready before an Extra page can fade in.
    // CG_PARTS is deliberately absent from the gallery cohort: its indexed
    // KIMG members are range-loaded on demand, so requiring the whole 25 MiB
    // NXZ output here recreates the high-water allocation that used to reject
    // the gallery after a long scene.
    #[cfg(any(target_os = "vita", test))]
    fn vita_extra_source_names(page: u32) -> &'static [&'static str] {
        match page {
            38 => &["EXTRA_PARTS"],
            39 => &["SOUND_PARTS"],
            40 => &["ARCHIVE_PARTS", "ARCHTHUMB_PARTS"],
            _ => &[],
        }
    }

    #[cfg(target_os = "vita")]
    fn vita_ui_source_index(&self, stem: &str) -> Option<&'static atlas_index::Atlas> {
        let index = self.allui.find_stem(&self.ui_atlas(stem)).or_else(|| self.allui.find_stem(stem))?;
        atlas_index::find(knife_platform_vita::audit::ASSET_MANIFEST, index, &self.allui.entries()[index])
    }

    #[cfg(target_os = "vita")]
    pub fn vita_release_extra_ui_sources(&self, page: u32) -> usize {
        let keep: &[&str] = match page {
            38 => &["EXTRA_PARTS", "CG_PARTS"], 39 => &["EXTRA_PARTS", "SOUND_PARTS"],
            40 => &["ARCHIVE_PARTS", "ARCHTHUMB_PARTS"], _ => &[],
        };
        let keep: Vec<_> = keep.iter().filter_map(|stem| self.vita_ui_source_index(stem).map(|atlas|atlas.index)).collect();
        let mut released = 0;
        let mut cached = self.ui_atlases.borrow_mut();
        for (index, entry) in self.allui.entries().iter().enumerate() {
            let stem = entry.name.split('.').next().unwrap_or("").to_ascii_uppercase();
            let extra = ["EXTRA_PARTS", "CG_PARTS", "SOUND_PARTS", "ARCHIVE_PARTS", "ARCHTHUMB_PARTS"]
                .iter().any(|base| stem == *base || stem.strip_prefix(base).is_some_and(|tail|tail.starts_with('_')));
            if extra && !keep.contains(&index) {
                cached.remove(&index);
                released += self.atlas_cache.discard_idle(&self.allui, index);
            }
        }
        released
    }

    /// Low-frequency page events carry the complete error in the same console
    /// stream as performance data, even when the startup header was missed.
    #[cfg(target_os = "vita")]
    pub fn vita_extra_ui_event(&self, page: u32, status: &str, detail: &str) {
        if !knife_perf::enabled() { return; }
        let gauges = knife_perf::gauges();
        let source = gauges[Gauge::SourceCacheBytes as usize].0;
        let deferred = gauges[Gauge::ResidentBytes as usize].0;
        let heap = knife_platform_vita::memory_available().ok().map(|m| m.heap_headroom);
        let mut quoted = String::new();
        knife_perf::json_string(&mut quoted, &detail.chars().take(768).collect::<String>());
        let tag = option_env!("KNIFE_BUILD_TAG").unwrap_or("unknown");
        let mut quoted_tag = String::new();
        knife_perf::json_string(&mut quoted_tag, tag);
        let heap = heap.map_or_else(|| "null".into(), |value|value.to_string());
        let _ = knife_platform_vita::audit::console(&format!(
            "{{\"kind\":\"ui_source_event\",\"schema\":1,\"build\":{quoted_tag},\"page\":{page},\"status\":\"{status}\",\"source_cache_bytes\":{source},\"resident_deferred_bytes\":{deferred},\"heap_headroom\":{heap},\"detail\":{quoted}}}\n"));
    }

    /// Cursor prediction is opportunistic: one existing worker, no eviction,
    /// and room for BOTH stored input scratch and decoded output above recovery
    /// headroom. No GPU allocation or caller-side file/header read is performed.
    #[cfg(target_os = "vita")]
    pub fn vita_warm_extra_ui_sources(&self, page: u32) {
        use knife_perf::{optimizations as opt, memory_budget::{CPU_TARGET_HEADROOM, MIB}};
        if !opt::enabled(opt::SHARED_ATLAS) || !opt::enabled(opt::UI_SOURCE_PREFETCH)
            || self.cpu_cache_pressure.get() { return; }
        let Ok(memory) = knife_platform_vita::memory_available() else { return; };
        let available = (memory.heap_headroom as usize).saturating_sub(CPU_TARGET_HEADROOM + 2 * MIB);
        for stem in Self::vita_extra_source_names(page) {
            let Some(atlas) = self.vita_ui_source_index(stem) else { continue; };
            let scratch = usize::from(atlas.stored_sectors) * 2048;
            if !atlas.direct && atlas.decoded_bytes.saturating_add(scratch) <= available
                && self.atlas_cache.prewarm_if_room(self.allui.clone(), atlas.index, atlas.decoded_bytes) != 0 {
                break; // prewarm_if_room admits only one idle job.
            }
        }
    }

    /// Demand joins/promotes the same source owner, without waiting on the UI
    /// thread. All required atlas outputs must be ready before metadata is handed
    /// to deferred rendering. Errors remain errors, never a blank "ready" page.
    #[cfg(target_os = "vita")]
    pub fn vita_poll_extra_ui_sources(&self, page: u32, retry: bool) -> Result<bool> {
        use knife_perf::optimizations as opt;
        if !opt::enabled(opt::SHARED_ATLAS) || !opt::enabled(opt::UI_SOURCE_PREFETCH) { return Ok(true); }
        let mut ready = true;
        for stem in Self::vita_extra_source_names(page) {
            // An unindexed asset pack retains its ordinary loader/error path.
            let Some(atlas) = self.vita_ui_source_index(stem) else { continue; };
            if atlas.direct { continue; }
            if retry { self.atlas_cache.retry_failed(&self.allui, atlas.index); }
            match self.atlas_cache.poll(self.allui.clone(), atlas.index, true) {
                atlas_cache::Poll::Ready(_) => {},
                atlas_cache::Poll::Pending => ready = false,
                atlas_cache::Poll::Failed(error) => anyhow::bail!("UI source {} stored={} output={}: {error}",
                    atlas.entry, usize::from(atlas.stored_sectors)*2048, atlas.decoded_bytes),
            }
        }
        Ok(ready)
    }

    /// One speculative upload per update, before motion/rendering. Real demand
    /// joins an active decode through image_graded instead of starting a duplicate.
    #[cfg(target_os = "vita")]
    pub fn prefetch_window(&mut self, r: &Renderer, wanted: &[(String, Bake)], preview: &[(String, Bake)]) {
        let keys: Vec<_> = wanted.iter().take(32)
            .map(|(name, bake)| Self::image_key(name, *bake).2).collect();
        self.image_skipped.retain(|key| keys.contains(key));
        if let Some(worker) = &self.image_worker {
            if !worker.retain_window(self.image_context, &keys) {return;}
        }
        if !self.cpu_cache_pressure.get()
            && knife_perf::optimizations::enabled(knife_perf::optimizations::SHARED_ATLAS) {
            let archives = self.image_archives();
            for (name,bake) in wanted.iter().take(32) {
                if self.image_cached(name,*bake)
                    && let Some((archive,index)) = Self::primary_image_locator(&archives,&Self::image_key(name,*bake).0) {
                    self.atlas_cache.request(archive,index);
                }
            }
            // DISP/NOISE warming is admission-checked at maintenance instead
            // of refilling an evicted UI page on every lookahead frame.
        }
        let ready = self.image_worker.as_ref().and_then(|worker| worker.try_take());
        let mut uploaded = false;
        if let Some((key, decoded)) = ready {
            if let Some(decoded) = decoded {
                self.finish_decoded(r, decoded);
                uploaded = true;
            } else {
                self.image_perf.skipped += 1;
                self.image_skipped.insert(key);
            }
        }
        // A thumbnail rejected by the worker cap must not leave its save slot
        // waiting forever. Resolve at most one through the demand path.
        if !uploaded && let Some((name, bake)) = preview.iter().find(|(name, bake)| {
            !self.image_cached(name, *bake) && (self.image_worker.as_ref().is_none_or(|w| !w.is_running())
                || self.image_skipped.contains(&Self::image_key(name, *bake).2))
        }) {
            self.image_graded(r, name, *bake);
        }
        if let Some(worker) = &self.image_worker {
            for (name, bake) in wanted.iter().take(32) {
                if self.image_cached(name, *bake) { continue; }
                let (name, grade, key) = Self::image_key(name, *bake);
                let strip = self.strip(&name);
                if !worker.request(key.clone(), ImageJob { initial:None,demanded:false,rebake:false,motion:false,key,name,grade,strip }) { break; }
            }
        }
    }

    #[cfg(target_os = "vita")]
    pub fn image_performance(&self) -> (ImagePerf, knife_platform_vita::image_worker::WorkerStats) {
        (self.image_perf, self.image_worker.as_ref().map(|worker| worker.stats()).unwrap_or_default())
    }

    fn finish_image(&mut self, r: &Renderer, key: String, name: &str, pixels: Result<Pixels>) -> Option<Arc<Image>> {
        self.finish_image_sized(r, key, name, pixels, false)
    }

    fn finish_image_sized(&mut self, r: &Renderer, key: String, name: &str, pixels: Result<Pixels>, hd: bool) -> Option<Arc<Image>> {
        self.try_finish_image_sized(r, key, name, pixels, hd)
            .inspect_err(|error| tracing::warn!("image {name}: {error:#}"))
            .ok()
    }

    /// Keep the native cause intact for the VM's failed-resource event. Legacy
    /// optional/prefetch callers retain their Option wrapper above.
    fn try_finish_image_sized(&mut self, r: &Renderer, key: String, name: &str, pixels: Result<Pixels>, hd: bool) -> Result<Arc<Image>> {
        let pixels = pixels.with_context(|| format!("decoding image {name}"))?;
        // Count which route the image actually took: a pre-baked GXT never decodes
        // or bakes, everything else does both.
        #[cfg(target_os = "vita")]
        if matches!(pixels, Pixels::GxtNative { .. }) {
            self.image_perf.gxt_images += 1;
        } else {
            self.image_perf.decoded_images += 1;
        }
        #[cfg(target_os = "vita")]
        if !matches!(pixels, Pixels::GxtNative { .. }) {
            let (width, height, bpp, altered) = pixels.vita_upload_layout()
                .with_context(|| format!("invalid image upload {name}"))?;
            let before = self.images.len();
            let admission = self.image_cache.admit_if(&mut self.images,
                || r.try_reserve_image_upload(width, height, bpp, altered), knife_render::is_texture_memory_pressure);
            let reclaimed = before - self.images.len();
            if reclaimed != 0 || admission.is_err() {
                tracing::info!(name, width, height, bpp, altered, reclaimed,
                    cached = self.images.len(), admitted = admission.is_ok(), "CG_RECLAIM");
                self.vita_diagnostic_images(&format!("image {name} reclaimed={reclaimed} admitted={}", admission.is_ok()));
            }
            // The image cache has finished its pressure sweep. A stale native
            // free-memory reading must not reject an allocation that the
            // driver can still satisfy; the upload checks actual capacity.
            if let Err(error) = admission {
                if !knife_render::is_unverified_native_headroom(&error) {
                    return Err(error).with_context(|| format!(
                        "image GPU admission {name} ({width}x{height}, bpp={bpp})"));
                }
            }
        }
        #[cfg(target_os = "vita")]
        let upload_start = std::time::Instant::now();
        #[cfg(target_os = "vita")]
        let upload_audit = if knife_perf::enabled() {
            pixels.vita_upload_layout().ok().map(|layout| (Scope::new(Metric::ImageFirstUpload), layout))
        } else { None };
        let mut image = self.upload_pixels(r, name, pixels)
            .with_context(|| format!("image GPU registration/upload {name}"))?;
        if hd {
            image.width = image.tex.width as f32;
            image.height = image.tex.height as f32;
        }
        let img = Arc::new(image);
        #[cfg(target_os = "vita")]
        {
            let us = upload_start.elapsed().as_micros() as u64;
            self.image_perf.uploads += 1;
            self.image_perf.upload_us += us;
            self.image_perf.max_upload_us = self.image_perf.max_upload_us.max(us);
            self.image_cache.touch(&key);
        }
        #[cfg(target_os = "vita")]
        if let Some((audit, (w, h, bpp, altered))) = upload_audit {
            audit.finish_hot(&format!("{name} {w}x{h} bpp={bpp} alter={altered}"),
                u64::from(w) * u64::from(h) * u64::from(bpp), 0);
        }
        self.images.insert(key, Some(img.clone()));
        Ok(img)
    }

    /// Drop cached images nobody else references.
    pub fn trim_images(&mut self, keep: usize) {
        if self.images.len() <= keep {
            return;
        }
        self.images.retain(|_, v| v.as_ref().is_some_and(|i| Arc::strong_count(i) > 1));
        #[cfg(target_os = "vita")]
        self.image_cache.retain_keys(|key| self.images.contains_key(key));
    }

    /// Give a missing UI atlas the same native headroom admission as a CG
    /// upload. Only actual pressure scans cache-owned images; the renderer
    /// retains every native texture referenced by the complete pending frame.
    #[cfg(target_os = "vita")]
    pub fn admit_frame_upload(&mut self, reserve: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        let before = self.images.len();
        let admission = self.image_cache.admit_if(&mut self.images, reserve, knife_render::is_texture_memory_pressure);
        let reclaimed = before - self.images.len();
        if reclaimed != 0 || admission.is_err() {
            tracing::info!(reclaimed, cached = self.images.len(), admitted = admission.is_ok(),
                phase = "frame upload", "CG_RECLAIM");
            self.vita_diagnostic_images(&format!("frame reclaimed={reclaimed} admitted={}", admission.is_ok()));
        }
        admission.map(|_| ())
    }

    #[cfg(target_os = "vita")]
    fn vita_diagnostic_images(&self, phase: &str) {
        if option_env!("KNIFE_VITA_DEV_DIAGNOSTICS") != Some("1") { return; }
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true)
            .open("ux0:data/knife-woh-dev/game-runtime.log")
        {
            let _ = writeln!(file, "CG_CACHE_PRESSURE {phase} remaining={}", self.images.len());
            // Pressure-only observation; do not clone the Arc or change which
            // images admission regards as idle. Keep the diagnostic bounded.
            for (key, image) in self.images.iter().take(128) {
                if let Some(image) = image {
                    let _ = writeln!(file, "CG_CACHE_OWNER key={key:?} image_refs={} texture_refs={} {}",
                        Arc::strong_count(image), Arc::strong_count(&image.tex),
                        image.tex.vita_diagnostic_ownership());
                }
            }
            let _ = file.sync_all();
        }
    }

    /// The (strip, strip count) an image draws: a language-strip image (overseas ImageInfo +67 bit 1) stacks
    /// one copy per language, and a layer draws the game language's (sub_374F0 / sub_375D0 source rectangle
    /// `[0, language * h / n, w, h / n]`, n = sub_9A700).
    fn strip(&self, name: &str) -> Option<(usize, usize)> {
        let base = name.strip_prefix(THUMB).unwrap_or(name);
        let i = self.image_index_of(base)?;
        self.tables.images.get(i).filter(|im| im.language_strips).map(|_| self.language_strip())
    }

    /// The CPU side of an image load (no renderer needed): archive read, BNTX parse or image decode, grade, and
    /// the language strip.
    fn decode_pixels(archives: &[Arc<Archive>], name: &str, grade: Bake, strip: Option<(usize, usize)>) -> Result<Pixels> {
        #[cfg(target_os = "vita")]
        if let Some((archive, index)) = Self::primary_image_locator(archives, name)
            && let Some(hit) = graded_cache::lookup_source(&archive, index, name, grade, strip) {
            let source = ImageReload { archive: hit.archive.clone(), index: hit.index,
                encoded_len: hit.encoded_len, grade, strip, initial:None };
            return Ok(Pixels::Reloadable(Box::new(hit.into_pixels()), source));
        }
        let (data, archive, index) = Self::image_data(archives, name).context("not found")?;
        Self::decode_data_pixels(data, archive, index, name, grade, strip, None)
    }

    fn decode_data_pixels(data: Vec<u8>, archive: Arc<Archive>, index: usize, name: &str,
        grade: Bake, strip: Option<(usize, usize)>, rgba_cap: Option<usize>) -> Result<Pixels> {
        #[cfg(target_os = "vita")]
        return Self::decode_shared_data_pixels(ByteView::owned(data), archive, index, name, grade, strip, rgba_cap);
        #[cfg(not(target_os = "vita"))]
        { let _ = (archive, index, name); Self::decode_image_pixels_limit(&data, grade, strip, rgba_cap) }
    }

    #[cfg(target_os = "vita")]
    fn decode_source(cache: &atlas_cache::AtlasCache, archives: &[Arc<Archive>], name: &str,
        grade: Bake, strip: Option<(usize, usize)>, cap: Option<(usize,usize)>, worker_progress:bool) -> Result<Pixels> {
        let (archive, index) = Self::primary_image_locator(archives, name).context("not found")?;
        if let Some(hit) = graded_cache::lookup_source(&archive,index,name,grade,strip) {
            let source = ImageReload { archive:hit.archive.clone(),index:hit.index,encoded_len:hit.encoded_len,grade,strip,initial:None };
            return Ok(Pixels::Reloadable(Box::new(hit.into_pixels()),source));
        }
        if let Some((encoded, _)) = cap {
            let plan = archive.read_plan(index)?;
            anyhow::ensure!(plan.decoded_bytes <= encoded, "prefetch encoded budget");
        }
        if worker_progress {
            knife_platform_vita::image_worker::resource_state::image_location(archive.cache_identity(),index);
            knife_platform_vita::image_worker::resource_state::image_phase(knife_platform_vita::image_worker::resource_state::Phase::DemandSource);
        }
        let data = cache.whole(archive.clone(),index)?;
        if worker_progress {knife_platform_vita::image_worker::resource_state::image_phase(knife_platform_vita::image_worker::resource_state::Phase::Decode);}
        Self::decode_shared_data_pixels(data,archive,index,name,grade,strip,cap.map(|v|v.1))
    }

    #[cfg(target_os = "vita")]
    fn decode_shared_data_pixels(data: ByteView, archive: Arc<Archive>, index: usize, name: &str,
        grade: Bake, strip: Option<(usize, usize)>, rgba_cap: Option<usize>) -> Result<Pixels> {
        let compact = compact::is_compact(&data).then(||compact::parse(&data)).transpose()?;
        anyhow::ensure!(!compact.as_ref().is_some_and(|i|i.encoding == compact::Encoding::GxtAlterSrgb),
            "legacy pre-Alter codec8 requires the native-colour asset migration (codec9)");
        if let Some(image) = compact.as_ref()
            && gxt_native_route(image.encoding == compact::Encoding::GxtNativeUnorm,grade.cpu(),strip.is_some(),grade.raw) {
            let len = compact::HEADER_SIZE.checked_add(image.payload.len()).context("GXT KIMG envelope length overflow")?;
            let (logical,physical) = (image.logical,image.size);
            let encoded = data.range(0,len).context("truncated GXT KIMG envelope")?;
            return Ok(Pixels::GxtNative {logical,physical,encoded,source:data,archive,index,first_use:false});
        }
        let pixels = Self::decode_cached_pixels(&data,&archive,index,name,grade,strip,rgba_cap)?;
        let encoded_len = data.len();
        Ok(Pixels::Reloadable(Box::new(pixels),ImageReload {archive,index,encoded_len,grade,strip,initial:Some(data)}))
    }

    #[cfg(target_os = "vita")]
    fn graded_source(data: &[u8]) -> Option<(bool, usize)> {
        if compact::is_compact(data) {
            let image = compact::parse(data).ok()?;
            if !matches!(image.encoding, compact::Encoding::Png | compact::Encoding::PngUnorm | compact::Encoding::Jpeg | compact::Encoding::GxtNativeUnorm) { return None; }
            let bytes = (image.size[0] as usize).checked_mul(image.size[1] as usize)?.checked_mul(4)?;
            Some((image.is_srgb(), bytes))
        } else if matches!(image::guess_format(data).ok(), Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)) {
            // Raw input's native decoder has a strict 960x1080 cap; reserve its
            // worst owner size before decode rather than guess PNG/JPEG dimensions.
            Some((true, 4 * 960 * 1080))
        } else { None }
    }

    #[cfg(target_os = "vita")]
    fn decode_cached_pixels(data: &[u8], archive: &Arc<Archive>, index: usize, name: &str,
        grade: Bake, strip: Option<(usize, usize)>, rgba_cap: Option<usize>) -> Result<Pixels> {
        if grade.cpu() && knife_perf::optimizations::enabled(knife_perf::optimizations::GRADE_CACHE)
            && let Some((srgb, need)) = Self::graded_source(data) {
            return graded_cache::build(archive.clone(), index, name, grade, strip, srgb, need, data.len(),
                || Self::decode_image_pixels_limit(data, grade, strip, rgba_cap));
        }
        Self::decode_image_pixels_limit(data, grade, strip, rgba_cap)
    }

    fn decode_image_pixels_limit(data: &[u8], grade: Bake, strip: Option<(usize, usize)>, rgba_cap: Option<usize>) -> Result<Pixels> {
        let compact = compact::is_compact(data).then(|| compact::parse(data)).transpose()?;
        let mut logical = compact.as_ref().map(|image| image.logical);
        let wrap = |pixels, logical: Option<[u32; 2]>| {
            match logical { Some(size) => Pixels::Scaled(Box::new(pixels), size), None => pixels }
        };
        if !grade.cpu() && strip.is_none()
            && let Some(texture) = compact.as_ref().and_then(|image| image.texture())
        {
            return Ok(wrap(Pixels::Bntx(texture, !grade.raw), logical));
        }
        if !grade.cpu() && strip.is_none() && bntx::is_bntx(&data) {
            return Ok(Pixels::Bntx(bntx::parse(&data)?.into_iter().next().context("empty BNTX")?, !grade.raw));
        }
        #[cfg(target_os = "vita")]
        if let Some(image) = compact.as_ref().filter(|i|i.encoding == compact::Encoding::GxtNativeUnorm) {
            use knife_core::format::gxt_decode;
            let size = gxt_decode::dimensions(image.payload)?;
            anyhow::ensure!([size.0,size.1] == image.size,"KIMG GXT dimensions mismatch");
            let (top,rows) = if let Some((k,n)) = strip {
                anyhow::ensure!(n > 0 && k < n && size.1 >= n as u32,"invalid language strip");
                if let Some(size) = logical.as_mut() { size[1] /= n as u32; }
                (size.1 as usize*k/n,size.1 as usize/n)
            } else { (0,size.1 as usize) };
            let decode = Scope::new(Metric::GxtCpuDecode);
            let (w,h,mut px) = gxt_decode::rgba8_rows(image.payload,top,rows,rgba_cap.unwrap_or(4*960*1080))?;
            decode.finish(px.len() as u64,0);
            if grade.cpu() {
                let processing = Scope::new(Metric::CpuGrade); grade.apply(&mut px,false);
                processing.finish(px.len() as u64,0);
                return Ok(wrap(Pixels::Baked(w,h,px),logical));
            }
            return Ok(wrap(Pixels::Rgba(w,h,px,None),logical));
        }
        #[cfg(target_os = "vita")]
        {
            let (payload, srgb, expected) = if let Some(image) = compact.as_ref() {
                (image.payload, image.is_srgb(), Some(image.size))
            } else { (data, true, None) };
            let codec = image::guess_format(payload).ok();
            if let Some(image) = compact.as_ref() {
                let expected_codec = match image.encoding {
                    compact::Encoding::Jpeg => Some(image::ImageFormat::Jpeg),
                    compact::Encoding::Png | compact::Encoding::PngUnorm => Some(image::ImageFormat::Png),
                    _ => None,
                };
                if let Some(expected_codec) = expected_codec {
                    anyhow::ensure!(codec == Some(expected_codec), "KIMG payload format mismatch");
                }
            }
            let gray = grade.raw && !grade.cpu() && strip.is_none()
                && knife_perf::optimizations::enabled(knife_perf::optimizations::GRAY_R8)
                && codec == Some(image::ImageFormat::Png)
                && payload.get(25).is_some_and(|color| matches!(*color, 0 | 4));
            if gray || (knife_perf::optimizations::enabled(knife_perf::optimizations::DECODE_OWNER)
                && matches!(codec, Some(image::ImageFormat::Png | image::ImageFormat::Jpeg))) {
                use knife_platform_vita::image::DecodedImage;
                let cap = u32::try_from(rgba_cap.unwrap_or(4 * 960 * 1080))?;
                let mut owner = if gray { DecodedImage::decode_gray_bounded(payload, cap)? }
                    else { DecodedImage::decode_bounded(payload, cap)? };
                let w = owner.width();
                let mut h = owner.height();
                if let Some(expected) = expected { anyhow::ensure!([w, h] == expected, "KIMG encoded dimensions mismatch"); }
                let mut range = 0..owner.byte_len() as usize;
                if let Some((k, n)) = strip {
                    anyhow::ensure!(n > 0 && k < n && h >= n as u32, "invalid language strip");
                    let rows = h / n as u32;
                    let top = (h as usize * k / n) * w as usize * 4;
                    let bytes = rows as usize * w as usize * 4;
                    range = top..top + bytes;
                    h = rows;
                    if let Some(size) = logical.as_mut() { size[1] /= n as u32; }
                    // A borrowed view replaces copy_within/truncate entirely.
                    knife_perf::add(Metric::LanguageCrop, 0, bytes as u64);
                }
                let baked = grade.cpu();
                if baked {
                    let processing = Scope::new(Metric::CpuGrade);
                    grade.apply(&mut owner.pixels_mut()[range.clone()], srgb);
                    processing.finish(range.len() as u64, 0);
                }
                return Ok(wrap(Pixels::Native { width: w, height: h, owner, range, baked }, logical));
            }
        }
        let (w, mut h, mut px, srgb) = decode_image_limit(&data, rgba_cap)?;
        if let Some((k, n)) = strip {
            anyhow::ensure!(n > 0 && k < n && h >= n as u32, "invalid language strip");
            let rows = h / n as u32;
            let top = (h as usize * k / n) * w as usize * 4;
            let bytes = (rows * w) as usize * 4;
            let copy = Scope::new(Metric::LanguageCrop);
            px.copy_within(top..top + bytes, 0);
            copy.finish(if top == 0 { 0 } else { bytes as u64 }, bytes as u64);
            px.truncate(bytes);
            h = rows;
            if let Some(size) = logical.as_mut() {
                size[1] /= n as u32;
            }
        }
        if grade.cpu() {
            let processing = Scope::new(Metric::CpuGrade);
            grade.apply(&mut px, srgb);
            processing.finish(px.len() as u64, 0);
            return Ok(wrap(Pixels::Baked(w, h, px), logical));
        }
        Ok(wrap(Pixels::Rgba(w, h, px, (!cfg!(target_os = "vita") && !grade.raw).then_some(srgb)), logical))
    }

    fn upload_pixels(&self, r: &Renderer, name: &str, pixels: Pixels) -> Result<Image> {
        #[cfg(target_os = "vita")]
        let (pixels, reload) = match pixels {
            Pixels::Reloadable(pixels, source) => (*pixels, Some(source)),
            pixels => (pixels, None),
        };
        let (pixels, logical) = match pixels {
            Pixels::Scaled(pixels, logical) => (*pixels, Some(logical)),
            #[cfg(target_os = "vita")]
            Pixels::GxtNative { logical, encoded, source, archive, index, physical, first_use } =>
                (Pixels::GxtNative { encoded, source, archive, index, logical, physical, first_use }, Some(logical)),
            pixels => (pixels, None),
        };
        #[cfg(target_os = "vita")]
        let sparse = match &pixels {
            Pixels::GxtNative { encoded, archive, index, logical, physical, .. } => {
                let entry = archive.entries().get(*index).map_or("", |entry| entry.name.as_str());
                let candidate = sparse_source::candidate(knife_platform_vita::audit::ASSET_MANIFEST,
                    &archive.name, *index, name, entry, *logical, *physical, encoded.len());
                let verified = candidate && knife_platform_vita::image::sha256_hex(encoded)
                    .is_some_and(|hash| &hash == sparse_source::ENVELOPE_SHA256);
                if name == "IMG0436" {
                    tracing::info!(name, archive = %archive.name, index, candidate, verified,
                        "SPARSE_SOURCE plan=img0436 tile=16 halo=2 rects=68");
                }
                verified
            }
            _ => false,
        };
        #[cfg(target_os = "vita")]
        let rebuilt_us = match &pixels { Pixels::SharedBaked(_, _, _, us) => *us, _ => 0 };
        #[cfg(target_os = "vita")]
        let altered = false; // Native encoded UNORM8 never needs an Alter target.
        #[cfg(target_os = "vita")]
        let raw_r8 = matches!(&pixels, Pixels::Bntx(t, false) if t.format == TexFormat::R8Unorm)
            || matches!(&pixels, Pixels::Native { owner, .. } if owner.bytes_per_pixel() == 1);
        let tex = match pixels {
            #[cfg(target_os = "vita")]
            Pixels::GxtNative { encoded, source, archive, index, logical, physical, first_use } => {
                let len = encoded.len();
                if knife_perf::optimizations::enabled(knife_perf::optimizations::SHARED_ATLAS)
                    && self.atlas_cache.seed(&archive,index,source) {
                    let cache = self.atlas_cache.clone();
                    // The source pool charges this owner while a real STCH
                    // holds its one-use lease. Its first draw cannot race an
                    // eviction and synchronously read the card again.
                    let ready=RefCell::new(first_use.then(||encoded.clone()));
                    let load = Rc::new(move ||{
                        if let Some(bytes)=ready.borrow_mut().take(){return Ok(bytes);}
                        cache.range(archive.clone(),index,0,len)
                    });
                    r.defer_gxt_shared(logical,physical,&encoded,name,load,extract_native_gxt)?
                } else {
                    // Owned-byte baseline still uses the bounded I/O service;
                    // disabling zero-copy must not restore an unbounded read.
                    let cache=self.atlas_cache.clone();
                    let load=Rc::new(move ||{
                        let bytes=cache.range(archive.clone(),index,0,len)?;
                        let copy=Scope::new(Metric::StageArcVec);let out=bytes.to_vec();
                        copy.finish(out.len()as u64,bytes.allocation_len()as u64);Ok(out)
                    });
                    if first_use || knife_perf::optimizations::enabled(knife_perf::optimizations::GXT_READY) {
                        r.defer_gxt_ready(logical,physical,encoded,name,load,extract_native_gxt)?
                    } else { r.defer_gxt_altered_reload(logical,physical,&encoded,name,load,extract_native_gxt)? }
                }
            },
            #[cfg(target_os = "vita")]
            Pixels::Native { width, height, owner, range, baked, .. } => {
                let px = &owner.pixels()[range];
                if owner.bytes_per_pixel() == 1 {
                    r.try_upload(width, height, Upload::R8(px), name)?
                } else if baked {
                    r.try_upload_baked(width, height, px, name)?
                } else { r.try_upload(width, height, Upload::Rgba8(px), name)? }
            },
            #[cfg(target_os = "vita")]
            Pixels::SharedBaked(w, h, px, _) => r.try_upload_baked(w, h, &px, name)?,
            Pixels::Bntx(t, true) => upload_bntx(r, &t)?,
            Pixels::Bntx(t, false) => upload_bntx_raw(r, &t)?,
            #[cfg(target_os = "vita")]
            Pixels::Rgba(w, h, px, Some(_)) => r.try_upload(w, h, Upload::Rgba8(&px), name)?,
            #[cfg(not(target_os = "vita"))]
            Pixels::Rgba(w, h, px, Some(srgb)) => r.upload_altered(w, h, Upload::Rgba8(&px), srgb, name),
            #[cfg(target_os = "vita")]
            Pixels::Rgba(w, h, px, None) => r.try_upload(w, h, Upload::Rgba8(&px), name)?,
            #[cfg(not(target_os = "vita"))]
            Pixels::Rgba(w, h, px, None) => r.upload(w, h, Upload::Rgba8(&px), name),
            #[cfg(target_os = "vita")]
            Pixels::Baked(w, h, px) => r.try_upload_baked(w, h, &px, name)?,
            #[cfg(not(target_os = "vita"))]
            Pixels::Baked(w, h, px) => r.upload_baked(w, h, &px, name),
            Pixels::Scaled(..) => unreachable!("compact metadata was removed before uploading"),
            #[cfg(target_os = "vita")]
            Pixels::Reloadable(..) => unreachable!("reload metadata was removed before uploading"),
        };
        let tex = if let Some(size) = logical { knife_render::GpuTexture::with_logical_size(tex, size) } else { tex };
        #[cfg(target_os = "vita")]
        let tex = if let Some(source) = reload {
            // Raw R8 rule masks keep their one-byte upload. Ordinary CGs and
            // CPU-graded variants are RGBA8 and can share residency admission.
            if raw_r8 {
                tex
            } else {
                let ImageReload { archive, index, encoded_len, grade, strip, initial } = source;
                if knife_perf::optimizations::enabled(knife_perf::optimizations::SHARED_ATLAS)
                    && let Some(initial) = initial { self.atlas_cache.seed(&archive,index,initial); }
                // Same background staging as the KATL path: a CG that re-reads megabytes from
                // the card blocked the frame for ~670 ms on hardware.
                let stage = Arc::clone(&self.byte_stage);
                let key = byte_stage_key(&archive.name, index, 0, encoded_len);
                if !knife_perf::optimizations::enabled(knife_perf::optimizations::SHARED_ATLAS) {
                    stage.request(key, Arc::clone(&archive), index, 0, encoded_len);
                }
                let decode_archive = archive.clone();
                let cached_archive = archive.clone();
                let cached_name = name.to_string();
                let decode_name = name.to_string();
                let source_cache = self.atlas_cache.clone();
                let load = Rc::new(move || if knife_perf::optimizations::enabled(knife_perf::optimizations::SHARED_ATLAS) {
                    let view = source_cache.range(archive.clone(),index,0,encoded_len)?;
                    let copy = Scope::new(Metric::StageArcVec); let out = view.to_vec();
                    copy.finish(out.len() as u64,0); Ok(out)
                } else { match stage.take(key) {
                    Some(bytes) => {
                        let copy = Scope::new(Metric::StageArcVec);
                        let out = bytes.to_vec();
                        copy.finish(out.len() as u64, 0);
                        Ok(out)
                    },
                    None => {
                        let bytes=stage.load_bounded(key,archive.clone(),index,0,encoded_len)?;
                        let copy=Scope::new(Metric::StageArcVec);let out=bytes.to_vec();
                        copy.finish(out.len()as u64,0);Ok(out)
                    },
                }});
                let decode = Rc::new(move |bytes: &[u8]| {
                    let pixels = Self::decode_cached_pixels(bytes, &decode_archive, index,
                        &decode_name, grade, strip, None)?;
                    let (pixels, reloaded_logical) = match pixels {
                        Pixels::Scaled(pixels, logical) => (*pixels, Some(logical)),
                        pixels => (pixels, None),
                    };
                    anyhow::ensure!(reloaded_logical == logical, "CG reload logical dimensions changed");
                    match pixels {
                        Pixels::Rgba(w, h, px, srgb) => Ok((w, h, knife_render::PixelOwner::from(px), srgb.unwrap_or(false))),
                        Pixels::Baked(w, h, px) => Ok((w, h, knife_render::PixelOwner::from(px), false)),
                        Pixels::SharedBaked(w, h, px, _) => Ok((w, h, knife_render::PixelOwner::new(px), false)),
                        Pixels::Native { width, height, owner, range, .. } => {
                            anyhow::ensure!(owner.bytes_per_pixel() == 4, "raw R8 must not enter RGBA reload");
                            Ok((width, height, knife_render::PixelOwner::new(NativeSlice { owner, range }),
                                false))
                        },
                        Pixels::Bntx(t, _) => Ok((t.width, t.height,
                            knife_render::PixelOwner::from(bntx_decode::to_rgba8(&t)?), false)),
                        _ => anyhow::bail!("invalid CG reload pixel route"),
                    }
                });
                let cached: knife_render::CachedImageReload = Rc::new(move || {
                    graded_cache::lookup_source(&cached_archive, index, &cached_name, grade, strip).map(|result|
                        (result.width, result.height, knife_render::PixelOwner::new(result.pixels), false))
                });
                let tex = r.adopt_image_reload_cached(tex, encoded_len, name, altered, load, decode, Some(cached))?;
                if rebuilt_us != 0 { r.set_image_rebuild_us(&tex, rebuilt_us)?; }
                tex
            }
        } else { tex };
        #[cfg(target_os = "vita")]
        let tex = if sparse {
            knife_render::GpuTexture::with_sparse_coverage(tex, knife_render::SparseCoveragePlan::Img0436)
        } else { tex };
        let no_shake = self.image_info(name).is_some_and(|i| i.no_shake);
        let (width, height, center_x, center_y, level) = match self.image_info(name) {
            // Witch on the Holy Night's layer draw (sub_1A090 -> jp_B0460 / sub_16210 / jp_36520) sizes the quad by
            // the altered texture, (u32)(size / 0.66667) px (jp_A3180) in the 1920x1080 space, a language-strip
            // image's height over the strip count; the ImageInfo gives only the centre (+52 / +56) the position is
            // offset by. Its width / height can disagree (SOU_L_12_10_00_01_01: 228 x 384 units, texture 402 x 860).
            // The save thumbnails (jp_89AD0) size their 1/6 copies by texture / 0.16667 in the 1280x720 thumbnail
            // surface, 1.5 times that in the 1920 space the previews are laid out in.
            Some(i) if witch() => {
                let n = self.strip(name).map_or(1, |(_, n)| n as u32);
                let (k, m) = if name.starts_with(THUMB) { (0.16667, 1.5) } else { (0.66667, 1.0) };
                let alt = |v: u32| (v as f32 / k) as u32 as f32 * m;
                let w = alt(tex.width) / 3.0;
                let h = alt(tex.height * n) / n as f32 * 362.0 / 1080.0;
                (w, h, i.center_x, i.center_y, i.level_scale())
            }
            Some(i) => (i.width, i.height, i.center_x, i.center_y, i.level_scale()),
            None => {
                // 1280x720 textures map to 640 units
                let w = tex.width as f32 / 2.0;
                let h = tex.height as f32 / 2.0 * (362.0 / 360.0);
                (w, h, w / 2.0, h / 2.0, 1.0)
            }
        };
        Ok(Image {
            tex,
            width,
            height,
            center_x,
            center_y,
            level,
            no_shake,
        })
    }

    /// Flowchart node preview: allpaccg entry `thumcg_top.mzp` + 1 + n (GALLERY_FC_THnnnn), by stem.
    pub fn flow_preview_name(&self, n: i32) -> Option<String> {
        let a = self.allpaccg.as_ref()?;
        let i = a.find_stem("THUMCG_TOP")? + 1 + usize::try_from(n).ok()?;
        let name = &a.entries().get(i)?.name;
        Some(name.split('.').next().unwrap_or(name).to_string())
    }

    #[cfg(target_os = "vita")]
    fn indexed_ui_textures(&self, r: &Renderer, index: usize) -> Option<Result<Vec<(String, TexRef)>>> {
        let entry = self.allui.entries().get(index)?;
        let atlas = atlas_index::find(knife_platform_vita::audit::ASSET_MANIFEST, index, entry)?;
        Some((|| {
            anyhow::ensure!(atlas.decoded_bytes <= atlas_cache::CAP, "indexed atlas exceeds CPU budget");
            let range_stage = atlas.direct;
            let mut out = Vec::with_capacity(atlas.members.len());
            for member in atlas.members {
                anyhow::ensure!(member.offset.checked_add(member.len).is_some_and(|end| end <= atlas.decoded_bytes),
                    "indexed atlas member exceeds decoded entry");
                if member.name == "help_key" {
                    out.push((member.name.to_string(), replace_help_key_ps(r)?));
                    continue;
                }
                if member.name == "conf_manual0_zc" {
                    out.push((member.name.to_string(), replace_conf_manual0_ps(r)?));
                    continue;
                }
                let archive = self.allui.clone();
                let cache = self.atlas_cache.clone();
                let (offset, len) = (member.offset, member.len);
                let load: Rc<dyn Fn() -> Result<ByteView>> = if range_stage {
                    // Offline raw KATL: the existing range worker reads only
                    // this KIMG. Never inflate or retain the complete atlas.
                    let stage = Arc::clone(&self.byte_stage);
                    let key = byte_stage_key(&archive.name, index, offset, len);
                    Rc::new(move || {
                        let bytes = match stage.take(key) {
                            Some(bytes) => bytes,
                            None => stage.load_direct_bounded(key, archive.clone(), index, offset, len)
                                .with_context(|| format!("UI range read failed entry={index} offset={offset} len={len}"))?,
                        };
                        Ok(bytes)
                    })
                } else {
                    Rc::new(move || cache.range(archive.clone(), index, offset, len))
                };
                let tex = if let Some(metadata) = member.gxt {
                    r.defer_gxt_indexed(member.logical, member.physical, len, metadata,
                        member.name, load, extract_native_gxt)?
                } else {
                    let (logical, physical, encoding) = (member.logical, member.physical, member.encoding);
                    let decode = Rc::new(move |bytes: &[u8]| {
                        let actual = compact::parse(bytes)?;
                        anyhow::ensure!(actual.logical == logical && actual.size == physical
                            && actual.encoding as u32 == encoding, "indexed image metadata changed");
                        decode_image_owned(bytes)
                    });
                    r.defer_altered_shared(logical, physical, len, member.name, load, decode)?
                };
                out.push((member.name.to_string(), tex));
            }
            Ok(out)
        })())
    }

    /// Warm only the title/story cohort after metadata registration. Inactive
    /// MENU/SAVE/CONF descriptors do not allocate atlas output. No owner is pinned
    /// here: later large pages use the same bounded LRU and may replace this cohort.
    #[cfg(target_os = "vita")]
    pub fn warm_initial_ui_payloads(&self) -> Result<()> {
        if !knife_perf::optimizations::enabled(knife_perf::optimizations::SHARED_ATLAS) { return Ok(()); }
        let names = [self.ui_atlas("TITLE_PARTS"), self.ui_atlas("DISP_PARTS"), "BORDER_PARTS".into()];
        let mut indices = Vec::new();
        let mut need = 0usize;
        for name in names {
            let Some(index) = self.allui.find_stem(&name) else { return Ok(()); };
            let Some(atlas) = atlas_index::find(knife_platform_vita::audit::ASSET_MANIFEST, index,
                &self.allui.entries()[index]) else { return Ok(()); };
            if atlas.direct { continue; }
            need = need.checked_add(atlas.decoded_bytes).context("startup atlas need overflow")?;
            indices.push(index);
        }
        anyhow::ensure!(need <= atlas_cache::CAP, "startup UI cohort exceeds shared CPU budget");
        for index in indices { drop(self.atlas_cache.whole(self.allui.clone(), index)?); }
        Ok(())
    }

    /// Load a UI texture atlas (BNTX in allui) by entry stem; returns all textures by name.
    pub fn ui_textures(&self, r: &Renderer, stem: &str) -> Result<Vec<(String, TexRef)>> {
        let i = self.allui.find_stem(stem).with_context(|| format!("{stem} not in allui"))?;
        #[cfg(target_os = "vita")]
        if let Some(cached) = self.ui_atlases.borrow().get(&i)
            && cached.first().is_some_and(|(_, tex)| r.owns_texture(tex))
        {
            return Ok(cached.clone());
        }
        let mut out = Vec::new();
        #[cfg(target_os = "vita")]
        let shared_atlas = knife_perf::optimizations::enabled(knife_perf::optimizations::SHARED_ATLAS);
        #[cfg(target_os = "vita")]
        let indexed = if shared_atlas || atlas_index::find(knife_platform_vita::audit::ASSET_MANIFEST,
            i, &self.allui.entries()[i]).is_some_and(|atlas| atlas.direct) {
            if let Some(result) = self.indexed_ui_textures(r, i) { out = result?; true } else { false }
        } else { false };
        #[cfg(not(target_os = "vita"))]
        let indexed = false;
        if !indexed {
        #[cfg(target_os = "vita")]
        let data = if shared_atlas {
            self.atlas_cache.whole(Arc::clone(&self.allui), i)?
        } else { ByteView::owned(self.allui.read(i)?) };
        #[cfg(not(target_os = "vita"))]
        let data = self.allui.read(i)?;

        if compact::is_atlas(&data) {
            for (name, image) in compact::parse_atlas(&data)? {
                #[cfg(target_os = "vita")]
                if name == "help_key" {
                    out.push((name.to_string(), replace_help_key_ps(r)?));
                    continue;
                }
                #[cfg(target_os = "vita")]
                if name == "conf_manual0_zc" {
                    out.push((name.to_string(), replace_conf_manual0_ps(r)?));
                    continue;
                }
                #[cfg(target_os = "vita")]
                {
                    let metadata = compact::parse(image)?;
                    // Each handle keeps an archive locator, not the encoded image. A GPU miss
                    // rereads the NX entry into bounded scratch and extracts just this KIMG.
                    let offset = (image.as_ptr() as usize)
                        .checked_sub(data.as_ptr() as usize)
                        .context("KATL image precedes atlas entry")?;
                    anyhow::ensure!(offset.checked_add(image.len()).is_some_and(|end| end <= data.len()),
                        "KATL image outside atlas entry");
                    let len = image.len();
                    let archive = Arc::clone(&self.allui);
                    if shared_atlas {
                        let cache = Arc::clone(&self.atlas_cache);
                        let load = Rc::new(move || cache.range(archive.clone(), i, offset, len));
                        let tex = if metadata.encoding == compact::Encoding::GxtNativeUnorm {
                            r.defer_gxt_shared(metadata.logical, metadata.size, image,
                                name, load, extract_native_gxt)?
                        } else {
                            r.defer_altered_shared(metadata.logical, metadata.size, len,
                                name, load, Rc::new(decode_image_owned))?
                        };
                        out.push((name.to_string(), tex));
                        continue;
                    }
                    let stage = Arc::clone(&self.byte_stage);
                    let key = byte_stage_key(&self.allui.name, i, offset, len);
                    // Start the read now: the atlas is usually sampled frames later, so the
                    // worker finishes long before the texture is prepared.
                    stage.request(key, Arc::clone(&archive), i, offset, len);
                    let load = Rc::new(move || match stage.take(key) {
                        Some(bytes) => {
                        let copy = Scope::new(Metric::StageArcVec);
                        let out = bytes.to_vec();
                        copy.finish(out.len() as u64, 0);
                        Ok(out)
                    },
                        None => {
                            let bytes=stage.load_bounded(key,archive.clone(),i,offset,len)?;
                            let copy=Scope::new(Metric::StageArcVec);let out=bytes.to_vec();
                            copy.finish(out.len()as u64,0);Ok(out)
                        },
                    });
                    // The entry's bytes are already in hand here, so keeping them (within the
                    // renderer's encoded-source budget) removes the card read that made a
                    // re-preparation cost 300-700 ms on hardware. Larger images still fall
                    // back to the reload descriptor, which is what the budget is for.
                    let copy = Scope::new(if metadata.encoding == compact::Encoding::GxtNativeUnorm {
                        Metric::UiRetainGxt
                    } else { Metric::UiRetainOther });
                    let retained = Arc::<[u8]>::from(image);
                    copy.finish(image.len() as u64, 0);
                    let tex = if metadata.encoding == compact::Encoding::GxtNativeUnorm {
                        r.defer_gxt_altered_reload(metadata.logical, metadata.size, image,
                            name, load, extract_native_gxt)?
                    } else {
                        match r.defer_altered(metadata.logical, metadata.size,
                            Arc::clone(&retained), name, decode_image)
                        {
                            Ok(tex) => tex,
                            Err(_) => r.defer_altered_reload(metadata.logical, metadata.size,
                                len, name, load, decode_image)?,
                        }
                    };
                    out.push((name.to_string(), tex));
                }
                #[cfg(not(target_os = "vita"))]
                out.push((name.to_string(), upload_compact(r, name, image)?));
            }
        } else {
            for t in bntx::parse(&data)? {
                let tex = upload_bntx(r, &t)?;
                out.push((t.name.clone(), tex));
            }
        }
        }
        // the game picks atlas images by index (the object table, sub_9A940); the other languages' atlases
        // (`NAME_<FOLDER>`) hold the same images in the same order under their own names (`title_titlelog_en`,
        // `caution_en`, `flow_info_en`, ...), so the UI also finds them under the Japanese atlas's names
        let ja_names: Vec<String> = self.language_profile.as_ref()
            .and_then(|profile| stem.rsplit_once('_').and_then(|(base, _)| profile.aliases.get(&base.to_ascii_uppercase())).cloned())
            .unwrap_or_else(|| stem
            .rsplit_once('_')
            .filter(|(_, l)| !l.eq_ignore_ascii_case("ja"))
            .and_then(|(base, _)| self.allui.find_stem(&format!("{base}_JA")))
            .and_then(|j| self.allui.read(j).ok())
            .and_then(|d| atlas_names(&d).ok())
            .unwrap_or_default());
        if !ja_names.is_empty() && ja_names.len() != out.len() {
            tracing::warn!("{stem}: {} images, the Japanese atlas {}", out.len(), ja_names.len());
        }
        let aliases: Vec<(String, TexRef)> = ja_names
            .into_iter()
            .zip(&out)
            .filter(|(ja, (n, _))| ja != n)
            .map(|(ja, (_, t))| (ja, t.clone()))
            .collect();
        out.extend(aliases);
        // the overseas Japanese atlas also renames some images the Japanese release has without a suffix
        // (`system_bg` -> `system_bg_ja`); the UI uses the Japanese release's names
        let aliases: Vec<(String, TexRef)> = out
            .iter()
            .filter_map(|(n, t)| {
                let base = n.strip_suffix("_ja")?;
                (!out.iter().any(|(m, _)| m == base)).then(|| (base.to_string(), t.clone()))
            })
            .collect();
        out.extend(aliases);
        #[cfg(target_os = "vita")]
        self.ui_atlases.borrow_mut().insert(i, out.clone());
        Ok(out)
    }

    /// BORDER_PARTS (sub_3B9A0 loads it into texture slots 338..355): alphagradation020..512, their `_inv`
    /// and `_2` variants. For the clip inset mask, the alpha of each one's middle row from the edge to the
    /// centre texel.
    pub fn soft_lut_rows(&self) -> Result<Vec<Vec<u8>>> {
        let i = self.allui.find_stem("BORDER_PARTS").context("BORDER_PARTS not in allui")?;
        let data = self.allui.read(i)?;
        if compact::is_atlas(&data) {
            return compact::parse_atlas(&data)?.iter().map(|(_, bytes)| {
                let (w, h, px, _) = decode_image(bytes)?;
                anyhow::ensure!(w == h, "BORDER_PARTS lookup must be square");
                let (w, r) = (w as usize, (w / 2) as usize);
                Ok((0..=r).map(|x| px[(r * w + x) * 4 + 3]).collect())
            }).collect();
        }
        bntx::parse(&data)?
            .iter()
            .map(|t| {
                let px = bntx_decode::to_rgba8(t)?;
                let (w, r) = (t.width as usize, (t.width / 2) as usize);
                Ok((0..=r).map(|x| px[(r * w + x) * 4 + 3]).collect())
            })
            .collect()
    }

    /// parts.mrg (no name table; the boot sequence loads entries by index, sub_B4A20 archive slot 27):
    /// 0 logo_fix, 1 caution2 (both cautions on one screen), 2 caution, 3 caution1.
    pub fn parts_texture(&self, r: &Renderer, index: usize) -> Option<TexRef> {
        let load = || -> Result<TexRef> {
            let data = Archive::open(&self.romfs, "parts")?.read(index)?;
            if compact::is_compact(&data) {
                return upload_compact(r, "parts", &data);
            }
            let t = bntx::parse(&data)?.into_iter().next().context("empty BNTX")?;
            upload_bntx(r, &t)
        };
        load().map_err(|e| tracing::warn!("parts #{index}: {e:#}")).ok()
    }

    /// The SYSMES file: SYSMES_TEXT in the Japanese allui, SYSMES_TEXT_ML in the overseas w_allui (jp_9A420);
    /// Witch on the Holy Night's SYSMES_TEXT holds four languages (no Korean, though allui has _KO atlases).
    fn sysmes_file(allui: &Archive, build: Build) -> Option<Vec<u8>> {
        let stem = match build {
            Build::Japanese | Build::Witch => "SYSMES_TEXT",
            Build::Overseas => "SYSMES_TEXT_ML",
        };
        allui.read(allui.find_stem(stem)?).ok()
    }

    /// The game language's system strings, indexed by the Japanese build's ids (u32 language count, u32 count,
    /// u64 0, then per language a u64 offset of a table of u64 offsets of NUL-terminated UTF-8 strings; entry 0
    /// is the language's folder name). The overseas file's message 217 moves to `SYSMES_DELETED`.
    pub fn sysmes(&self) -> Vec<String> {
        self.sysmes_in(self.language)
    }

    /// `sysmes` in language `lang` (jp_9A780(id, lang)).
    pub fn sysmes_in(&self, lang: usize) -> Vec<String> {
        // A language's table is parsed once: the config screen used to ask again every
        // frame, and a failed parse re-read the file each time.
        let slot = self.language_slot(lang);
        if let Some(v) = self.sysmes_cache.borrow().get(&slot) { return v.clone(); }
        let waiting = self.sysmes_retry.borrow().get(&slot).is_some_and(|at| {
            at.elapsed() < std::time::Duration::from_millis(500)
        });
        if waiting { return Vec::new(); }
        let parse = || -> Option<Vec<String>> {
            let d = Self::sysmes_file(&self.allui, self.build)?;
            let n = sysmes_u32(&d, 4)? as usize;
            let table = sysmes_u64(&d, 0x10 + 8 * self.language_slot(lang))?;
            let mut v = (0..n)
                .map(|i| {
                    let o = sysmes_u64(&d, table + 8 * i)?;
                    let end = d.get(o..)?.iter().position(|&b| b == 0)?;
                    Some(String::from_utf8_lossy(&d[o..o + end]).into_owned())
                })
                .collect::<Option<Vec<String>>>()?;
            if self.build == Build::Overseas && v.len() > SYSMES_OVERSEAS_INSERT {
                let deleted = v.remove(SYSMES_OVERSEAS_INSERT);
                v.resize(SYSMES_DELETED, String::new());
                v.push(deleted);
            }
            Some(v)
        };
        match parse() {
            Some(v) => {
                self.sysmes_cache.borrow_mut().insert(slot, v.clone());
                v
            }
            None => {
                self.sysmes_retry.borrow_mut().insert(slot, std::time::Instant::now());
                Vec::new()
            }
        }
    }

    /// The button guide's help_txt word cells of the game language (CONTHELP_TXT_UV.BIN, jp_9A420 / jp_9A710:
    /// i16 count, then from byte 8 per language `count` cells of i16 x0 y0 x1 y1).
    pub fn guide_words(&self) -> Vec<[u16; 4]> {
        let parse = || -> Option<Vec<[u16; 4]>> {
            let d = self.allui.read(self.allui.find_stem("CONTHELP_TXT_UV")?).ok()?;
            let n = i16::from_le_bytes(d.get(0..2)?.try_into().ok()?).max(0) as usize;
            let base = 8 + 8 * n * self.language_slot(self.language);
            (0..n)
                .map(|i| {
                    let c = d.get(base + 8 * i..base + 8 * i + 8)?;
                    Some([0, 2, 4, 6].map(|k| u16::from_le_bytes([c[k], c[k + 1]])))
                })
                .collect()
        };
        parse().unwrap_or_default()
    }

    /// A system atlas of the game language (jp_9A940: the object table's `name.nxgz` becomes
    /// `name_<language folder>.nxgz`, found case-insensitively; SELECT_PARTS_JA alone is named in full).
    pub fn ui_atlas(&self, stem: &str) -> String {
        format!("{stem}_{}", self.language_folder().to_ascii_uppercase())
    }

    /// sub_AB0F0: the first file of the language's font folder `<folder><suffix>` (suffix "" the story text,
    /// "2" the UI text, "3" the bubble text).
    pub fn font_path(&self, suffix: &str) -> Option<PathBuf> {
        let dir = self.romfs.join(format!("{}{suffix}", self.language_folder()));
        let mut files: Vec<PathBuf> =
            std::fs::read_dir(&dir).ok()?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_file()).collect();
        files.sort();
        files.into_iter().next()
    }

    /// The pair font metrics at the 40 px base size (sub_CAE88, `sfnt::pair_metrics`) of the message font set
    /// (the language's three fonts sub_AB0F0 loads) and of the bubble font (`<folder>3`), each paired with the
    /// exfont: line feed / ascent / baseline position Japanese 50 / 41 / 41, English 53 / 43 / 40 (bubble ascent
    /// 41), Simplified Chinese 49 / 39 / 39, Traditional Chinese 47 / 39 / 37 (the exfont's); the Japanese ones
    /// without font files.
    fn font_metrics(&self) -> (sfnt::SetMetrics, sfnt::SetMetrics) {
        use knife_core::format::{bffnt, bfttf};
        let read = |p: PathBuf| std::fs::read(p).ok().map(|d| bfttf::decode(&d).unwrap_or(d));
        let set = |suffixes: &[&str]| {
            let m: Vec<sfnt::Metrics> =
                suffixes.iter().filter_map(|s| self.font_path(s)).filter_map(read).filter_map(|d| sfnt::metrics(&d)).collect();
            sfnt::set_metrics(&m, crate::text::BASE_SIZE)
        };
        let ex = std::fs::read(self.romfs.join("exfont_nx01.bffnt"))
            .ok()
            .and_then(|d| bffnt::Font::parse(&d).ok())
            .map(|f| sfnt::SetMetrics {
                height: i32::from(f.height),
                ascent: i32::from(f.ascent),
                line_feed: i32::from(f.line_feed),
                baseline: i32::from(f.baseline),
            });
        let ja = sfnt::SetMetrics { height: 50, ascent: 41, line_feed: 50, baseline: 41 };
        let msg = set(&["", "2", "3"]).map_or(ja, |m| sfnt::pair_metrics(m, ex));
        let bubble = match set(&["3"]) {
            Some(m) => sfnt::pair_metrics(m, ex),
            // Witch on the Holy Night's bubble variant is the shared system font (`Game::load_fonts`)
            None if self.build == Build::Witch => self
                .shared_fonts()
                .first()
                .and_then(|d| sfnt::metrics(d))
                .and_then(|m| sfnt::set_metrics(&[m], crate::text::BASE_SIZE))
                .map_or(msg, |m| sfnt::pair_metrics(m, ex)),
            None => msg,
        };
        (msg, bubble)
    }

    /// The console shared fonts sub_AB0F0 asks for, in their order of priority (sub_C9F3C: nn::pl::GetSharedFont
    /// with the language code of dword_3C97F0[language]: ja / en-US the standard font, zh-Hans / zh-Hant the
    /// Chinese ones first). sub_CAA50 puts them all in each font set, so the later ones fill in missing glyphs.
    pub fn shared_font_names(&self) -> &'static [&'static str] {
        match self.language {
            2 => &[
                "nintendo_udsg-r_org_zh-cn_003.bfttf",
                "nintendo_udsg-r_ext_zh-cn_003.bfttf",
                "nintendo_udsg-r_std_003.bfttf",
            ],
            3 => &["nintendo_udjxh-db_zh-tw_003.bfttf", "nintendo_udsg-r_std_003.bfttf"],
            _ => &["nintendo_udsg-r_std_003.bfttf"],
        }
    }

    /// Paths of the shared system fonts, for callers that must not hold every
    /// font's bytes at once: three Chinese fonts plus the atlas copy exceeded the
    /// heap and aborted inside the allocator.
    pub fn shared_font_paths(&self) -> Vec<std::path::PathBuf> {
        self.shared_font_names().iter().map(|n| self.romfs.join(n)).collect()
    }

    /// Decode one shared system font.
    pub fn shared_font(&self, path: &std::path::Path) -> Option<Vec<u8>> {
        match std::fs::read(path) {
            Ok(d) => Some(knife_core::format::bfttf::decode(&d).unwrap_or(d)),
            Err(e) => {
                tracing::warn!("system font {}: {e}", path.display());
                None
            }
        }
    }

    /// The shared fonts there are (`shared_font_names`, decoded), in order; a missing one is reported.
    pub fn shared_fonts(&self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for name in self.shared_font_names() {
            let path = self.romfs.join(name);
            match std::fs::read(&path) {
                Ok(d) => out.push(knife_core::format::bfttf::decode(&d).unwrap_or(d)),
                Err(e) => tracing::warn!("system font {}: {e}", path.display()),
            }
        }
        out
    }

    /// A per-language romfs file (sub_AB0F0 fonts, the save icon, movies): `<stem>` in Japanese, else
    /// `<stem>_en` / `_zc` / `_zt` (the language folder names, SYSMES entry 0).
    pub fn language_folder(&self) -> &'static str {
        ["ja", "en", "zc", "zt"].get(self.language).copied().unwrap_or("ja")
    }

    /// Decode a sound effect from the BFSAR (`MOON_SE03001` -> `WSD_moon_se03001`).
    pub fn sound_effect(&mut self, name: &str) -> Option<Arc<Pcm>> {
        let key = name.to_ascii_lowercase();
        #[cfg(target_os="vita")]
        if let Some(pcm) = self.sound_prepared.get(&key) { return pcm.clone(); }
        if let Some(pcm) = self.sounds.borrow_mut().get(&key) {
            return pcm;
        }
        let decode = Scope::new(Metric::SoundEffectDecode);
        let res = self.sound_pcm(|| self.sar.load_wave_sound(&format!("wsd_{key}"))
            .or_else(|error| if Self::audio_allocation_failed(&error) { Err(error) }
                else { self.sar.load_wave_sound(&key) })
            .or_else(|error| if Self::audio_allocation_failed(&error) { Err(error) }
                else { self.load_stream_file(&format!("stream/{name}.opus")) }));
        let pcm = match res {
            Ok(p) => Some(Arc::new(p)),
            Err(e) => {
                tracing::warn!("sound {name}: {e:#}");
                // Memory pressure is transient; do not permanently negative-cache it.
                if Self::audio_allocation_failed(&e) { return None; }
                None
            }
        };
        decode.finish_hot(name, pcm.as_ref().map_or(0, |pcm| pcm.frames.len() as u64 * 8), u64::from(pcm.is_some()));
        self.sounds.borrow_mut().insert(key, pcm.clone());
        pcm
    }

    /// A wave sound by BFSAR item id (sub_710's dword_3341A0 ids), cached with the named sounds.
    pub fn sound_effect_id(&mut self, item: u32) -> Option<Arc<Pcm>> {
        let key = format!("#{item:x}");
        if let Some(pcm) = self.sounds.borrow_mut().get(&key) {
            return pcm;
        }
        let decode = Scope::new(Metric::SoundEffectDecode);
        let pcm = match self.sound_pcm(|| self.sar.load_wave_sound_id(item)) {
            Ok(p) => Some(Arc::new(p)),
            Err(e) => {
                tracing::warn!("sound {item:#x}: {e:#}");
                if Self::audio_allocation_failed(&e) { return None; }
                None
            }
        };
        decode.finish_hot(&key, pcm.as_ref().map_or(0, |pcm| pcm.frames.len() as u64 * 8), u64::from(pcm.is_some()));
        self.sounds.borrow_mut().insert(key, pcm.clone());
        pcm
    }

    fn audio_allocation_failed(error: &anyhow::Error) -> bool {
        error.chain().any(|cause| cause.is::<std::collections::TryReserveError>()
            || cause.downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::OutOfMemory))
    }

    fn sound_pcm(&self, mut load: impl FnMut() -> Result<Pcm>) -> Result<Pcm> {
        #[cfg(target_os = "vita")]
        {
            // Reserve room before a whole FWAV read (<=8 MiB) plus the final
            // stereo PCM (<=16 MiB), rather than waiting for end-of-frame trim.
            let target = knife_perf::memory_budget::CPU_TARGET_HEADROOM
                + knife_perf::memory_budget::CPU_SOUND_BYTES;
            if let Ok(memory) = knife_platform_vita::memory_available() {
                self.vita_trim_cpu_caches(target.saturating_sub(memory.heap_headroom as usize));
            }
        }
        let result = load();
        #[cfg(target_os = "vita")]
        if result.as_ref().is_err_and(Self::audio_allocation_failed) {
            // Fragmentation can reject a large block despite aggregate free
            // bytes. Release idle CPU owners and retry exactly once.
            self.vita_trim_cpu_caches(24 * knife_perf::memory_budget::MIB);
            return load();
        }
        result
    }

    pub fn load_stream_file(&self, rel: &str) -> Result<Pcm> {
        let data = std::fs::read(self.stream_path(rel)).with_context(|| format!("reading {rel}"))?;
        codec::decode_stream(&data)
    }

    /// Stream table paths retain their original names; compact distributions use standard Ogg Opus.
    pub fn stream_path(&self, rel: &str) -> PathBuf {
        #[cfg(target_os = "vita")]
        return self.stream_path_existing(rel).unwrap_or_else(|| self.romfs.join(rel));
        #[cfg(not(target_os = "vita"))]
        {
            let original = self.romfs.join(rel);
            let ogg = original.with_extension("ogg");
            if ogg.is_file() { ogg } else { original }
        }
    }

    #[cfg(target_os = "vita")]
    pub fn stream_path_existing(&self, rel: &str) -> Option<PathBuf> {
        self.stream_resolver.resolve(rel, knife_perf::optimizations::enabled(knife_perf::optimizations::STREAM_INDEX))
    }

    pub fn voice(&self, name: &str) -> Result<Pcm> {
        self.load_stream_file(&format!("stream/{name}.opus"))
    }
}

/// Substitute the complete help_key image with the embedded PS atlas. The
/// game's archive remains read-only and the original atlas UVs stay valid.
#[cfg(target_os = "vita")]
fn replace_help_key_ps(r: &Renderer) -> Result<TexRef> {
    let replacement = include_bytes!("../data/help_key_ps.kimg");
    let image = compact::parse(replacement)?;
    anyhow::ensure!(image.encoding == compact::Encoding::GxtNativeUnorm
        && image.logical == [384, 256] && image.size == [384, 256],
        "invalid embedded PS help_key replacement");
    let load = Rc::new(move || Ok(replacement.to_vec()));
    r.defer_gxt_altered_reload(image.logical, image.size, replacement,
        "help_key PS replacement", load, extract_native_gxt)
}

/// Substitute only the Chinese controller-help page image at atlas load time.
#[cfg(target_os = "vita")]
fn replace_conf_manual0_ps(r: &Renderer) -> Result<TexRef> {
    let replacement = include_bytes!("../data/conf_manual0_ps.kimg");
    let image = compact::parse(replacement)?;
    anyhow::ensure!(image.encoding == compact::Encoding::GxtNativeUnorm
        && image.logical == [1920, 1080] && image.size == [960, 540],
        "invalid embedded PS conf_manual0_zc replacement");
    let load = Rc::new(move || Ok(replacement.to_vec()));
    r.defer_gxt_altered_reload(image.logical, image.size, replacement,
        "conf_manual0_zc PS replacement", load, extract_native_gxt)
}

/// Upload a BNTX texture as a sprite texture (through pre_alter, `Renderer::upload_altered`; the exe builds every
/// image slot with sub_A3180).
pub fn upload_bntx(r: &Renderer, t: &bntx::Texture) -> Result<TexRef> {
    let label = &t.name;
    #[cfg(not(target_os = "vita"))]
    let srgb = t.format.is_srgb();
    #[cfg(target_os = "vita")]
    return match t.format {
        TexFormat::R8Unorm => r.try_upload(t.width, t.height, Upload::R8(&t.data), label),
        _ => {
            let rgba = bntx_decode::to_rgba8(t)?;
            r.try_upload(t.width, t.height, Upload::Rgba8(&rgba), label)
        }
    };
    #[cfg(not(target_os = "vita"))]
    Ok(match t.format {
        TexFormat::Bc7Unorm | TexFormat::Bc7Srgb if r.bc_supported => r.upload_altered(t.width, t.height, Upload::Bc7(&t.data), srgb, label),
        TexFormat::Bc1Unorm | TexFormat::Bc1Srgb if r.bc_supported => r.upload_altered(t.width, t.height, Upload::Bc1(&t.data), srgb, label),
        TexFormat::Bc3Unorm | TexFormat::Bc3Srgb if r.bc_supported => r.upload_altered(t.width, t.height, Upload::Bc3(&t.data), srgb, label),
        _ => {
            let rgba = bntx_decode::to_rgba8(t)?;
            r.upload_altered(t.width, t.height, Upload::Rgba8(&rgba), srgb, label)
        }
    })
}

/// Upload a BNTX texture to be sampled as stored (rule masks).
fn upload_bntx_raw(r: &Renderer, t: &bntx::Texture) -> Result<TexRef> {
    let label = &t.name;
    #[cfg(target_os = "vita")]
    return match t.format {
        TexFormat::R8Unorm => r.try_upload(t.width, t.height, Upload::R8(&t.data), label),
        _ => {
            let rgba = bntx_decode::to_rgba8(t)?;
            r.try_upload(t.width, t.height, Upload::Rgba8(&rgba), label)
        }
    };
    #[cfg(not(target_os = "vita"))]
    Ok(match t.format {
        TexFormat::Bc7Unorm | TexFormat::Bc7Srgb if r.bc_supported => r.upload(t.width, t.height, Upload::Bc7(&t.data), label),
        TexFormat::Bc1Unorm | TexFormat::Bc1Srgb if r.bc_supported => r.upload(t.width, t.height, Upload::Bc1(&t.data), label),
        TexFormat::Bc3Unorm | TexFormat::Bc3Srgb if r.bc_supported => r.upload(t.width, t.height, Upload::Bc3(&t.data), label),
        TexFormat::R8Unorm => r.upload(t.width, t.height, Upload::R8(&t.data), label),
        _ => {
            let rgba = bntx_decode::to_rgba8(t)?;
            r.upload(t.width, t.height, Upload::Rgba8(&rgba), label)
        }
    })
}

#[cfg(target_os = "vita")]
fn extract_native_gxt(data: &[u8]) -> Result<knife_render::DeferredGxt<'_>> {
    let image = compact::parse(data)?;
    anyhow::ensure!(image.encoding == compact::Encoding::GxtNativeUnorm,
        "deferred GXT encoding changed or requires a different colour transform");
    Ok(knife_render::DeferredGxt {
        logical: image.logical, physical: image.size, file: image.payload,
    })
}

/// Decode an archive image (BNTX, JPEG or PNG) to RGBA8: (width, height, pixels, sampled through an sRGB view).
/// sub_A3180 uploads JPEG / PNG decodes as "Jpeg Tex" / "Png Tex" (sub_E0CD8 format 0 -> 2822, sRGB).
fn decode_image(data: &[u8]) -> Result<(u32, u32, Vec<u8>, bool)> {
    decode_image_limit(data, None)
}

fn decode_image_limit(data: &[u8], rgba_cap: Option<usize>) -> Result<(u32, u32, Vec<u8>, bool)> {
    if compact::is_compact(data) {
        let image = compact::parse(data)?;
        if let Some(cap) = rgba_cap {
            anyhow::ensure!(u64::from(image.size[0]) * u64::from(image.size[1]) * 4 <= cap as u64,
                "KIMG exceeds decoded image budget");
        }
        if let Some(t) = image.texture() {
            return Ok((t.width, t.height, bntx_decode::to_rgba8(&t)?, t.format.is_srgb()));
        }
        let format = match image.encoding {
            compact::Encoding::Jpeg => image::ImageFormat::Jpeg,
            compact::Encoding::Png | compact::Encoding::PngUnorm => image::ImageFormat::Png,
            compact::Encoding::GxtNativeUnorm | compact::Encoding::GxtAlterSrgb => anyhow::bail!("GXT requires the matching native Vita block upload path"),
            _ => anyhow::bail!("unsupported CPU image encoding"),
        };
        anyhow::ensure!(image::guess_format(image.payload)? == format, "KIMG payload format mismatch");
        #[cfg(target_os = "vita")]
        {
            let (w, h, pixels) = decode_native_image(image.payload, rgba_cap)?;
            anyhow::ensure!([w, h] == image.size, "KIMG encoded dimensions mismatch");
            return Ok((w, h, pixels, false));
        }
        #[cfg(not(target_os = "vita"))]
        {
        let img = image::load_from_memory_with_format(image.payload, format).context("decoding KIMG payload")?.to_rgba8();
        anyhow::ensure!([img.width(), img.height()] == image.size, "KIMG encoded dimensions mismatch");
        return Ok((img.width(), img.height(), img.into_raw(), image.is_srgb()));
        }
    }
    if bntx::is_bntx(data) {
        let t = bntx::parse(data)?.into_iter().next().context("empty BNTX")?;
        return Ok((t.width, t.height, bntx_decode::to_rgba8(&t)?, t.format.is_srgb()));
    }
    #[cfg(target_os = "vita")]
    if matches!(image::guess_format(data)?, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
        let (w, h, pixels) = decode_native_image(data, rgba_cap)?;
        return Ok((w, h, pixels, false));
    }
    let img = image::load_from_memory(data).context("decoding image")?.to_rgba8();
    Ok((img.width(), img.height(), img.into_raw(), true))
}

#[cfg(target_os = "vita")]
fn decode_image_owned(data: &[u8]) -> Result<(u32, u32, knife_render::PixelOwner, bool)> {
    let pixels = Assets::decode_image_pixels_limit(data, Bake::NONE, None, None)?;
    let pixels = match pixels { Pixels::Scaled(px, _) => *px, px => px };
    match pixels {
        Pixels::Native { width, height, owner, range, .. } =>
            Ok((width, height, knife_render::PixelOwner::new(NativeSlice { owner, range }), false)),
        Pixels::Rgba(w, h, px, srgb) => Ok((w, h, px.into(), srgb.unwrap_or(false))),
        Pixels::Baked(w, h, px) => Ok((w, h, px.into(), false)),
        Pixels::SharedBaked(w, h, px, _) => Ok((w, h, knife_render::PixelOwner::new(px), false)),
        Pixels::Bntx(t, _) => Ok((t.width, t.height, bntx_decode::to_rgba8(&t)?.into(), false)),
        _ => anyhow::bail!("invalid owned atlas pixel route"),
    }
}

#[cfg(target_os = "vita")]
fn decode_native_image(data: &[u8], cap: Option<usize>) -> Result<(u32, u32, Vec<u8>)> {
    use knife_platform_vita::image::DecodedImage;
    let image = match cap {
        Some(cap) => DecodedImage::decode_bounded(data, u32::try_from(cap)?)?,
        None => DecodedImage::decode(data)?,
    };
    // The native owner stays on its decoding thread. Only a bounded owned Rust
    // allocation crosses the queue; drop the C owner before publishing it.
    let copy = Scope::new(Metric::DecodedCVec);
    let pixels = image.pixels().to_vec();
    copy.finish(pixels.len() as u64, 0);
    Ok((image.width(), image.height(), pixels))
}

fn atlas_names(data: &[u8]) -> Result<Vec<String>> {
    if compact::is_atlas(data) {
        Ok(compact::parse_atlas(data)?.into_iter().map(|(name, _)| name.to_string()).collect())
    } else {
        Ok(bntx::parse(data)?.into_iter().map(|t| t.name).collect())
    }
}

fn upload_compact(r: &Renderer, name: &str, data: &[u8]) -> Result<TexRef> {
    let logical = compact::parse(data)?.logical;
    let (w, h, pixels, srgb) = decode_image(data)?;
    let tex = r.upload_altered(w, h, Upload::Rgba8(&pixels), srgb, name);
    Ok(knife_render::GpuTexture::with_logical_size(tex, logical))
}

// BEGIN standalone CG GXT route (std-only so host rustc tests this exact logic).
#[cfg(any(target_os = "vita", test))]
fn gxt_native_route(codec9: bool, cpu_grade: bool, strip: bool, raw: bool) -> bool {
    codec9 && !cpu_grade && !strip && !raw
}
// END standalone CG GXT route.

// BEGIN standalone CG GXT routing tests (run from a host rustc harness).
#[cfg(test)]
mod cg_gxt_route_tests {
    use super::gxt_native_route;

    #[test]
    fn native_unorm_codec9_uses_deferred_blocks() {
        assert_eq!(gxt_native_route(true, false, false, false), true);
        assert_eq!(gxt_native_route(false, false, false, false), false);
    }

    #[test]
    fn cpu_grade_raw_and_language_strip_select_bounded_decoder() {
        assert_eq!(gxt_native_route(true, true, false, false), false);
        assert_eq!(gxt_native_route(true, false, true, false), false);
        assert_eq!(gxt_native_route(true, false, false, true), false);
    }
}
// END standalone CG GXT routing tests.

#[cfg(test)]
mod ui_source_prepare_tests {
    use super::Assets;

    #[test]
    fn gallery_preparation_does_not_require_the_whole_cg_atlas() {
        assert_eq!(Assets::vita_extra_source_names(38), &["EXTRA_PARTS"]);
    }

    #[test]
    fn other_extra_pages_keep_their_required_atlases() {
        assert_eq!(Assets::vita_extra_source_names(39), &["SOUND_PARTS"]);
        assert_eq!(Assets::vita_extra_source_names(40), &["ARCHIVE_PARTS", "ARCHTHUMB_PARTS"]);
    }
}

#[cfg(test)]
mod compact_image_tests {
    #[test]
    fn png_decode_preserves_alpha_colour_space_and_checks_header_dimensions() {
        let rgba = image::RgbaImage::from_pixel(8, 4, image::Rgba([32, 64, 128, 200]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(rgba).write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let payload = bytes.into_inner();
        for encoding in [2u32, 7] {
            let mut data = b"KIMG".to_vec();
            for n in [1, 1920, 1080, 8, 4, encoding, payload.len() as u32] { data.extend_from_slice(&n.to_le_bytes()); }
            data.extend_from_slice(&payload);
            let (w, h, pixels, srgb) = super::decode_image(&data).unwrap();
            assert_eq!((w, h, pixels.len(), srgb), (8, 4, 8 * 4 * 4, encoding == 2));
            assert!(pixels.chunks_exact(4).all(|p| p == [32, 64, 128, 200]));
            data[16..20].copy_from_slice(&7u32.to_le_bytes());
            assert!(super::decode_image(&data).is_err());
        }
    }

    /// The compact distribution stores each UI image independently; EN and ZC
    /// must resolve to distinct archive entries with matching alias order.
    #[test]
    fn real_compact_ui_atlas_language_and_encoded_budget() {
        let Some(root) = std::env::var_os("KNIFE_VITA_ASSETS") else { return; };
        let root = std::path::PathBuf::from(root);
        let archive = knife_core::format::archive::Archive::open_file_with_limits(
            &root, "allui", knife_core::format::archive::ArchiveLimits::VITA,
        ).unwrap();
        let en = archive.find_stem("TITLE_PARTS_EN").unwrap();
        let zc = archive.find_stem("TITLE_PARTS_ZC").unwrap();
        assert_ne!(en, zc, "language atlases must have distinct cache keys");
        let profile = crate::languages::Profile::load(&root).unwrap().unwrap();
        for i in [en, zc] {
            let data = archive.read(i).unwrap();
            let images = knife_core::format::compact::parse_atlas(&data).unwrap();
            assert_eq!(images.len(), profile.aliases["TITLE_PARTS"].len());
            let mut encoded_total = 0usize;
            let mut decoded_total = 0usize;
            for (name, image) in images {
                let metadata = knife_core::format::compact::parse(image).unwrap();
                assert!(metadata.logical[0] >= metadata.size[0] && metadata.logical[1] >= metadata.size[1]);
                assert!(image.len() <= 32 * 1024 * 1024, "{name} exceeds encoded budget");
                encoded_total += image.len();
                decoded_total += metadata.size[0] as usize * metadata.size[1] as usize * 4;
            }
            assert!(decoded_total > encoded_total * 4, "title PNGs should remain encoded until referenced");
        }
    }

    #[test]
    fn jpeg_decode_is_opaque_and_rejects_mislabeled_png() {
        let rgb = image::RgbImage::from_pixel(8, 4, image::Rgb([32, 64, 128]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(rgb).write_to(&mut bytes, image::ImageFormat::Jpeg).unwrap();
        let data = knife_core::format::compact::encode([16, 8], [8, 4],
            knife_core::format::compact::Encoding::Jpeg, &bytes.into_inner()).unwrap();
        let (w, h, px, srgb) = super::decode_image(&data).unwrap();
        assert_eq!((w, h, srgb), (8, 4, true));
        assert!(px.chunks_exact(4).all(|p| p[3] == 255));
        let mut wrong = data;
        wrong[24..28].copy_from_slice(&2u32.to_le_bytes());
        assert!(super::decode_image(&wrong).is_err());
    }
}

fn sysmes_u32(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

fn sysmes_u64(d: &[u8], o: usize) -> Option<usize> {
    Some(u64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?) as usize)
}


