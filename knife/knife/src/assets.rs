//! Game data access: archives, exe tables, scripts, sound archive and a GPU texture cache.

use crate::grade::Bake;
#[cfg(target_os = "vita")]
use std::{cell::RefCell, rc::Rc};
use std::{
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
    /// RGBA8 baked on the CPU (`Bake::apply`), sRGB-encoded like the altered texture it stands for
    Baked(u32, u32, Vec<u8>),
    /// Reduced physical pixels, with the original image dimensions used by scripts and layout.
    Scaled(Box<Pixels>, [u32; 2]),
    /// Reuse the worker's first decode, but let hidden CG layers release GPU
    /// residency. Retain an exact archive entry locator, never a second RGBA copy.
    #[cfg(target_os = "vita")]
    Reloadable(Box<Pixels>, ImageReload),
    /// Already Alter-baked Vita GXT; encoded bytes move through the worker
    /// channel, then are dropped after registering a bounded archive reload.
    #[cfg(target_os = "vita")]
    GxtAlter {
        encoded: Vec<u8>, archive: Arc<Archive>, index: usize,
        logical: [u32; 2], physical: [u32; 2],
    },
}

#[cfg(target_os = "vita")]
pub struct ImageReload {
    archive: Arc<Archive>,
    index: usize,
    encoded_len: usize,
    grade: Bake,
    strip: Option<(usize, usize)>,
}

#[cfg(target_os = "vita")]
impl Pixels {
    /// Native allocation plan, before consuming the CPU result. The BNTX
    /// fallback decodes colour textures to RGBA8; raw R8 masks stay one byte.
    fn vita_upload_layout(&self) -> Result<(u32, u32, u32, bool)> {
        let (width, height, bpp, altered, bytes) = match self {
            Self::Scaled(pixels, _) => return pixels.vita_upload_layout(),
            Self::Reloadable(pixels, _) => return pixels.vita_upload_layout(),
            Self::GxtAlter { .. } => anyhow::bail!("GXT block upload uses deferred allocation"),
            Self::Bntx(texture, altered) => (
                texture.width, texture.height,
                if !altered && texture.format == TexFormat::R8Unorm { 1 } else { 4 },
                *altered, None,
            ),
            Self::Rgba(w, h, px, srgb) => (*w, *h, 4, srgb.is_some(), Some(px.len())),
            Self::Baked(w, h, px) => (*w, *h, 4, false, Some(px.len())),
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
    key: String,
    name: String,
    strip: Option<(usize, usize)>,
    pixels: Result<Pixels>,
}

#[cfg(target_os = "vita")]
struct ImageJob {
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
    entries: std::sync::Mutex<std::collections::VecDeque<(u64, Arc<[u8]>)>>,
    in_flight: std::sync::Mutex<std::collections::HashSet<u64>>,
    bytes: std::sync::atomic::AtomicUsize,
}

#[cfg(target_os = "vita")]
impl ByteStage {
    const CAP: usize = 24 * 1024 * 1024;

    /// Queue a read unless it is already staged or in flight.
    pub fn request(self: &Arc<Self>, key: u64, archive: Arc<Archive>, index: usize,
        offset: usize, len: usize) {
        {
            let entries = self.entries.lock().unwrap();
            if entries.iter().any(|(k, _)| *k == key) {
                return;
            }
        }
        {
            let mut in_flight = self.in_flight.lock().unwrap();
            if !in_flight.insert(key) {
                return;
            }
        }
        let stage = Arc::clone(self);
        let spawned = std::thread::Builder::new().name("byte stage".into()).spawn(move || {
            let read = archive.read_uncompressed_range(index, offset, len).ok();
            if let Some(bytes) = read {
                let arc: Arc<[u8]> = Arc::from(bytes);
                let size = arc.len();
                let mut entries = stage.entries.lock().unwrap();
                entries.push_back((key, arc));
                let mut total = stage.bytes.fetch_add(size, std::sync::atomic::Ordering::Relaxed) + size;
                while total > Self::CAP {
                    let Some((_, old)) = entries.pop_front() else { break };
                    total -= old.len();
                    stage.bytes.fetch_sub(old.len(), std::sync::atomic::Ordering::Relaxed);
                }
            }
            stage.in_flight.lock().unwrap().remove(&key);
        });
        if spawned.is_err() {
            self.in_flight.lock().unwrap().remove(&key);
        }
    }

    /// Take staged bytes, if the worker finished them.
    pub fn take(&self, key: u64) -> Option<Arc<[u8]>> {
        let mut entries = self.entries.lock().ok()?;
        let index = entries.iter().position(|(k, _)| *k == key)?;
        let (_, bytes) = entries.remove(index)?;
        self.bytes.fetch_sub(bytes.len(), std::sync::atomic::Ordering::Relaxed);
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
    pub sar: SoundArchive,
    parsed: HashMap<usize, Arc<Scene>>,
    images: HashMap<String, Option<Arc<Image>>>,
    #[cfg(target_os = "vita")]
    image_cache: crate::vita_image_cache::ImageCachePolicy,
    #[cfg(target_os = "vita")]
    image_worker: Option<knife_platform_vita::image_worker::BoundedWorker<ImageJob, Option<Decoded>>>,
    #[cfg(target_os = "vita")]
    image_skipped: std::collections::HashSet<String>,
    #[cfg(target_os = "vita")]
    image_perf: ImagePerf,
    sounds: HashMap<String, Option<Arc<Pcm>>>,
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
            build,
            language: usize::MAX,
            languages,
            language_profile,
            text: ScriptText::parse(&script_text, 0)?,
            script_text,
            #[cfg(target_os = "vita")]
        byte_stage: Arc::new(ByteStage::default()),
        allpac: Arc::new(Archive::open(romfs, &w("allpac"))?),
            allpacml: Archive::open(romfs, &w("allpacml")).ok().map(Arc::new),
            allpachd: Archive::open(romfs, hd).ok().map(Arc::new),
            allpachdml: if build == Build::Witch { Archive::open(romfs, "allpachdml").ok().map(Arc::new) } else { None },
            allpaccg: if build == Build::Witch { None } else { Archive::open(romfs, &w("allpaccg")).ok().map(Arc::new) },
            allui,
            tables,
            image_index,
            scenes,
            sar,
            parsed: HashMap::new(),
            images: HashMap::new(),
            #[cfg(target_os = "vita")]
            image_cache: Default::default(),
            #[cfg(target_os = "vita")]
            image_worker: None,
            #[cfg(target_os = "vita")]
            image_skipped: Default::default(),
            #[cfg(target_os = "vita")]
            image_perf: ImagePerf::default(),
            sounds: HashMap::new(),
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
            a.image_worker = match knife_platform_vita::image_worker::BoundedWorker::new(8, 2,
                move |job: ImageJob| {
                    let (data, archive, index) = Self::image_data_limit(&archives, &job.name,
                        Some(PREFETCH_ENCODED_CAP))?;
                    // Non-compact / oversized atlases remain demand-loaded.
                    let image = compact::parse(&data).ok()?;
                    // A pre-baked GXT entry never produces an RGBA buffer, so the
                    // decoded-pixel allowance must not exclude it: 960x540 would be
                    // 2,073,600 B of nonexistent RGBA and every native image would
                    // silently fall back to the demand path.
                    let bakes_rgba = image.encoding != compact::Encoding::GxtAlterSrgb;
                    if bakes_rgba
                        && u64::from(image.size[0]) * u64::from(image.size[1]) * 4 > PREFETCH_RGBA_CAP as u64
                    {
                        return None;
                    }
                    let pixels = Self::decode_data_pixels(data, archive, index,
                        &job.name, job.grade, job.strip, Some(PREFETCH_RGBA_CAP));
                    Some(Decoded { key: job.key, name: job.name, strip: job.strip, pixels })
                }) {
                Ok(worker) => Some(worker),
                Err(e) => { tracing::warn!("bounded image worker unavailable: {e}"); None }
            };
        }
        Ok(a)
    }

    /// Hand the language's font metrics (`font_metrics`) to the text layout.
    fn apply_font_metrics(&self) {
        let (msg, bubble) = self.font_metrics();
        // Tsukihime's bubble font is in the message font list (`<folder>3`), whose line feed it shares
        let bubble_feed = if self.build == Build::Witch { bubble.line_feed } else { msg.line_feed };
        crate::text::set_font_metrics(msg.line_feed, msg.ascent, msg.baseline, bubble.ascent, bubble_feed);
    }

    /// jp_9A700: the languages the data declares.
    pub fn languages(&self) -> usize {
        self.languages
    }

    /// sub_2DE70: the game language for a config word (negative or past the data's languages: the system's).
    pub fn resolve_language(&self, word: i16) -> usize {
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
        #[cfg(target_os = "vita")]
        {
            self.ui_atlases.get_mut().clear();
            self.image_skipped.clear();
            if let Some(worker) = &self.image_worker {
                worker.retain_window(language as u64, &[]);
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
            return Some(s.clone());
        }
        let raw = self.scenes.scene(index)?;
        let s = Arc::new(Scene::parse(raw));
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
        if let Some(i) = self.images.get(&key) {
            #[cfg(target_os = "vita")]
            self.image_cache.touch(&key);
            return i.clone();
        }
        let pixels = Self::decode_pixels(&self.hd_archives(), &name, grade, self.strip(&name));
        self.finish_image_sized(r, key, &name, pixels, true)
    }

    /// A normal entry's decoded bytes and exact archive locator. The locator
    /// lets Vita reopen one KIMG after a deferred GPU cache eviction.
    fn image_data(archives: &[Arc<Archive>], name: &str) -> Option<(Vec<u8>, Arc<Archive>, usize)> {
        Self::image_data_limit(archives, name, None)
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
            let ml = archives.iter().find(|a| a.name.ends_with("allpacml"));
            let (a, base, thumbs) = [(archives.first(), "BG_TOP", "THUMSTAND_TOP"), (ml, "BGML_TOP", "THUMSTANDML_TOP")]
                .into_iter()
                .find_map(|(a, base, thumbs)| a.filter(|a| a.find_stem(stem).is_some()).map(|a| (a, base, thumbs)))?;
            let i = a.find_stem(stem)?.checked_sub(a.find_stem(base)? + 1)? + a.find_stem(thumbs)? + 1;
            return read(a, i).inspect_err(|e| tracing::warn!("reading thumbnail {stem}: {e}"))
                .ok().map(|bytes| (bytes, Arc::clone(a), i));
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
        if let Some(i) = self.images.get(&key) {
            #[cfg(target_os = "vita")]
            self.image_cache.touch(&key);
            return i.clone();
        }
        #[cfg(target_os = "vita")]
        let prefetched = {
            let start = std::time::Instant::now();
            let result = self.image_worker.as_ref().and_then(|worker| worker.take_for(&key)).flatten();
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
        self.images.contains_key(&Self::image_key(name, bake).2)
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
        if !self.images.contains_key(&d.key) && d.strip == self.strip(&d.name) {
            self.finish_image(r, d.key, &d.name, d.pixels);
        }
    }

    /// One speculative upload per update, before motion/rendering. Real demand
    /// joins an active decode through image_graded instead of starting a duplicate.
    #[cfg(target_os = "vita")]
    pub fn prefetch_window(&mut self, r: &Renderer, wanted: &[(String, Bake)], preview: &[(String, Bake)]) {
        let keys: Vec<_> = wanted.iter().take(32)
            .map(|(name, bake)| Self::image_key(name, *bake).2).collect();
        self.image_skipped.retain(|key| keys.contains(key));
        if let Some(worker) = &self.image_worker {
            worker.retain_window(self.language as u64, &keys);
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
                if !worker.request(key.clone(), ImageJob { key, name, grade, strip }) { break; }
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
        let pixels = match pixels {
            Ok(pixels) => pixels,
            Err(e) => {
                tracing::warn!("image {name}: {e:#}");
                self.images.insert(key, None);
                return None;
            }
        };
        // Count which route the image actually took: a pre-baked GXT never decodes
        // or bakes, everything else does both.
        #[cfg(target_os = "vita")]
        if matches!(pixels, Pixels::GxtAlter { .. }) {
            self.image_perf.gxt_images += 1;
        } else {
            self.image_perf.decoded_images += 1;
        }
        #[cfg(target_os = "vita")]
        if !matches!(pixels, Pixels::GxtAlter { .. }) {
            let (width, height, bpp, altered) = match pixels.vita_upload_layout() {
                Ok(layout) => layout,
                Err(e) => {
                    tracing::warn!("invalid image upload {name}: {e:#}");
                    return None;
                }
            };
            let before = self.images.len();
            let admission = self.image_cache.admit(&mut self.images,
                || r.try_reserve_image_upload(width, height, bpp, altered));
            let reclaimed = before - self.images.len();
            if reclaimed != 0 || admission.is_err() {
                tracing::info!(name, width, height, bpp, altered, reclaimed,
                    cached = self.images.len(), admitted = admission.is_ok(), "CG_RECLAIM");
                self.vita_diagnostic_images(&format!("image {name} reclaimed={reclaimed} admitted={}", admission.is_ok()));
            }
            if let Err(e) = admission {
                // Native pressure is retryable; never turn it into a permanent
                // not-found entry that would hide the image for this session.
                tracing::warn!("image GPU admission {name}: {e:#}");
                return None;
            }
        }
        #[cfg(target_os = "vita")]
        let upload_start = std::time::Instant::now();
        let img = match self.upload_pixels(r, name, pixels) {
            Ok(mut i) => {
                if hd {
                    i.width = i.tex.width as f32;
                    i.height = i.tex.height as f32;
                }
                Some(Arc::new(i))
            }
            Err(e) => {
                tracing::warn!("image GPU upload {name}: {e:#}");
                #[cfg(not(target_os = "vita"))]
                self.images.insert(key, None);
                return None;
            }
        };
        #[cfg(target_os = "vita")]
        {
            let us = upload_start.elapsed().as_micros() as u64;
            self.image_perf.uploads += 1;
            self.image_perf.upload_us += us;
            self.image_perf.max_upload_us = self.image_perf.max_upload_us.max(us);
            self.image_cache.touch(&key);
        }
        self.images.insert(key, img.clone());
        img
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
        let admission = self.image_cache.admit(&mut self.images, reserve);
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
        let (data, archive, index) = Self::image_data(archives, name).context("not found")?;
        Self::decode_data_pixels(data, archive, index, name, grade, strip, None)
    }

    fn decode_data_pixels(data: Vec<u8>, archive: Arc<Archive>, index: usize, name: &str,
        grade: Bake, strip: Option<(usize, usize)>, rgba_cap: Option<usize>) -> Result<Pixels> {
        #[cfg(target_os = "vita")]
        let mut data = data;
        #[cfg(target_os = "vita")]
        let compact = compact::is_compact(&data).then(|| compact::parse(&data)).transpose()?;
        #[cfg(target_os = "vita")]
        if let Some(image) = compact.as_ref()
            && image.encoding == compact::Encoding::GxtAlterSrgb
        {
            gxt_alter_route(true, grade.cpu(), strip.is_some(), grade.raw)
                .map_err(|why| anyhow::anyhow!("{name}: prebaked GXT cannot perform {why}; use original image resource"))?;
            // `compact::parse` accepts sector padding, but the deferred loader
            // must return precisely the KIMG envelope, without archive tail.
            let len = compact::HEADER_SIZE.checked_add(image.payload.len())
                .context("GXT KIMG envelope length overflow")?;
            anyhow::ensure!(len <= data.len(), "truncated GXT KIMG envelope");
            let logical = image.logical;
            let physical = image.size;
            data.truncate(len); // move one archive Vec through the worker; never clone GXT blocks
            return Ok(Pixels::GxtAlter {
                logical, physical, encoded: data, archive, index,
            });
        }
        #[cfg(not(target_os = "vita"))]
        let _ = (archive, index);
        let pixels = Self::decode_image_pixels_limit(&data, grade, strip, rgba_cap)?;
        #[cfg(target_os = "vita")]
        return Ok(Pixels::Reloadable(Box::new(pixels), ImageReload {
            archive, index, encoded_len: data.len(), grade, strip,
        }));
        #[cfg(not(target_os = "vita"))]
        Ok(pixels)
    }

    /// Shared by the first worker decode and a later residency miss. Grades,
    /// raw masks, language strips and compact layout must take the same route.
    fn decode_image_pixels(data: &[u8], grade: Bake, strip: Option<(usize, usize)>) -> Result<Pixels> {
        Self::decode_image_pixels_limit(data, grade, strip, None)
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
        let (w, mut h, mut px, srgb) = decode_image_limit(&data, rgba_cap)?;
        if let Some((k, n)) = strip {
            anyhow::ensure!(n > 0 && k < n && h >= n as u32, "invalid language strip");
            let rows = h / n as u32;
            let top = (h as usize * k / n) * w as usize * 4;
            let bytes = (rows * w) as usize * 4;
            px.copy_within(top..top + bytes, 0);
            px.truncate(bytes);
            h = rows;
            if let Some(size) = logical.as_mut() {
                size[1] /= n as u32;
            }
        }
        if grade.cpu() {
            grade.apply(&mut px, srgb);
            return Ok(wrap(Pixels::Baked(w, h, px), logical));
        }
        Ok(wrap(Pixels::Rgba(w, h, px, (!grade.raw).then_some(srgb)), logical))
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
            Pixels::GxtAlter { logical, encoded, archive, index, physical } =>
                (Pixels::GxtAlter { encoded, archive, index, logical, physical }, Some(logical)),
            pixels => (pixels, None),
        };
        #[cfg(target_os = "vita")]
        let altered = matches!(&pixels, Pixels::Bntx(_, true) | Pixels::Rgba(_, _, _, Some(_)));
        #[cfg(target_os = "vita")]
        let raw_r8 = matches!(&pixels, Pixels::Bntx(t, false) if t.format == TexFormat::R8Unorm);
        let tex = match pixels {
            #[cfg(target_os = "vita")]
            Pixels::GxtAlter { encoded, archive, index, logical, physical } => {
                let len = encoded.len();
                let load = Rc::new(move || archive.read_uncompressed_range(index, 0, len));
                r.defer_gxt_altered_reload(logical, physical, &encoded, name, load,
                    extract_pre_altered_gxt)?
            },
            Pixels::Bntx(t, true) => upload_bntx(r, &t)?,
            Pixels::Bntx(t, false) => upload_bntx_raw(r, &t)?,
            #[cfg(target_os = "vita")]
            Pixels::Rgba(w, h, px, Some(srgb)) => r.try_upload_altered(w, h, Upload::Rgba8(&px), srgb, name)?,
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
                let ImageReload { archive, index, encoded_len, grade, strip } = source;
                // Same background staging as the KATL path: a CG that re-reads megabytes from
                // the card blocked the frame for ~670 ms on hardware.
                let stage = Arc::clone(&self.byte_stage);
                let key = byte_stage_key(&archive.name, index, 0, encoded_len);
                stage.request(key, Arc::clone(&archive), index, 0, encoded_len);
                let load = Rc::new(move || match stage.take(key) {
                    Some(bytes) => Ok(bytes.to_vec()),
                    None => archive.read_uncompressed_range(index, 0, encoded_len),
                });
                let decode = Rc::new(move |bytes: &[u8]| {
                    let pixels = Self::decode_image_pixels(bytes, grade, strip)?;
                    let (pixels, reloaded_logical) = match pixels {
                        Pixels::Scaled(pixels, logical) => (*pixels, Some(logical)),
                        pixels => (pixels, None),
                    };
                    anyhow::ensure!(reloaded_logical == logical, "CG reload logical dimensions changed");
                    match pixels {
                        Pixels::Rgba(w, h, px, srgb) => Ok((w, h, px, srgb.unwrap_or(false))),
                        Pixels::Baked(w, h, px) => Ok((w, h, px, true)),
                        Pixels::Bntx(t, alter) => Ok((t.width, t.height, bntx_decode::to_rgba8(&t)?, alter && t.format.is_srgb())),
                        _ => anyhow::bail!("invalid CG reload pixel route"),
                    }
                });
                r.adopt_image_reload(tex, encoded_len, name, altered, load, decode)?
            }
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

    /// Load a UI texture atlas (BNTX in allui) by entry stem; returns all textures by name.
    pub fn ui_textures(&self, r: &Renderer, stem: &str) -> Result<Vec<(String, TexRef)>> {
        let i = self.allui.find_stem(stem).with_context(|| format!("{stem} not in allui"))?;
        #[cfg(target_os = "vita")]
        if let Some(cached) = self.ui_atlases.borrow().get(&i)
            && cached.first().is_some_and(|(_, tex)| r.owns_texture(tex))
        {
            return Ok(cached.clone());
        }
        let data = self.allui.read(i)?;
        let mut out = Vec::new();
        if compact::is_atlas(&data) {
            for (name, image) in compact::parse_atlas(&data)? {
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
                    let stage = Arc::clone(&self.byte_stage);
                    let key = byte_stage_key(&self.allui.name, i, offset, len);
                    // Start the read now: the atlas is usually sampled frames later, so the
                    // worker finishes long before the texture is prepared.
                    stage.request(key, Arc::clone(&archive), i, offset, len);
                    let load = Rc::new(move || match stage.take(key) {
                        Some(bytes) => Ok(bytes.to_vec()),
                        None => archive.read_uncompressed_range(i, offset, len),
                    });
                    // The entry's bytes are already in hand here, so keeping them (within the
                    // renderer's encoded-source budget) removes the card read that made a
                    // re-preparation cost 300-700 ms on hardware. Larger images still fall
                    // back to the reload descriptor, which is what the budget is for.
                    let retained = Arc::<[u8]>::from(image);
                    let tex = if metadata.encoding == compact::Encoding::GxtAlterSrgb {
                        r.defer_gxt_altered_reload(metadata.logical, metadata.size, image,
                            name, load, extract_pre_altered_gxt)?
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
        if let Some(s) = self.sounds.get(&key) {
            return s.clone();
        }
        let res = self
            .sar
            .load_wave_sound(&format!("wsd_{key}"))
            .or_else(|_| self.sar.load_wave_sound(&key))
            .or_else(|_| self.load_stream_file(&format!("stream/{name}.opus")));
        let pcm = match res {
            Ok(p) => Some(Arc::new(p)),
            Err(e) => {
                tracing::warn!("sound {name}: {e:#}");
                None
            }
        };
        if self.sounds.len() > 256 {
            self.sounds.clear();
        }
        self.sounds.insert(key, pcm.clone());
        pcm
    }

    /// A wave sound by BFSAR item id (sub_710's dword_3341A0 ids), cached with the named sounds.
    pub fn sound_effect_id(&mut self, item: u32) -> Option<Arc<Pcm>> {
        let key = format!("#{item:x}");
        if let Some(s) = self.sounds.get(&key) {
            return s.clone();
        }
        let pcm = match self.sar.load_wave_sound_id(item) {
            Ok(p) => Some(Arc::new(p)),
            Err(e) => {
                tracing::warn!("sound {item:#x}: {e:#}");
                None
            }
        };
        if self.sounds.len() > 256 {
            self.sounds.clear();
        }
        self.sounds.insert(key, pcm.clone());
        pcm
    }

    pub fn load_stream_file(&self, rel: &str) -> Result<Pcm> {
        let data = std::fs::read(self.stream_path(rel)).with_context(|| format!("reading {rel}"))?;
        codec::decode_stream(&data)
    }

    /// Stream table paths retain their original names; compact distributions use standard Ogg Opus.
    pub fn stream_path(&self, rel: &str) -> PathBuf {
        let original = self.romfs.join(rel);
        let ogg = original.with_extension("ogg");
        if ogg.is_file() { ogg } else { original }
    }

    pub fn voice(&self, name: &str) -> Result<Pcm> {
        self.load_stream_file(&format!("stream/{name}.opus"))
    }
}

/// Upload a BNTX texture as a sprite texture (through pre_alter, `Renderer::upload_altered`; the exe builds every
/// image slot with sub_A3180).
pub fn upload_bntx(r: &Renderer, t: &bntx::Texture) -> Result<TexRef> {
    let (label, srgb) = (&t.name, t.format.is_srgb());
    #[cfg(target_os = "vita")]
    return match t.format {
        TexFormat::Bc7Unorm | TexFormat::Bc7Srgb if r.bc_supported => r.try_upload_altered(t.width, t.height, Upload::Bc7(&t.data), srgb, label),
        TexFormat::Bc1Unorm | TexFormat::Bc1Srgb if r.bc_supported => r.try_upload_altered(t.width, t.height, Upload::Bc1(&t.data), srgb, label),
        TexFormat::Bc3Unorm | TexFormat::Bc3Srgb if r.bc_supported => r.try_upload_altered(t.width, t.height, Upload::Bc3(&t.data), srgb, label),
        _ => {
            let rgba = bntx_decode::to_rgba8(t)?;
            r.try_upload_altered(t.width, t.height, Upload::Rgba8(&rgba), srgb, label)
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
fn extract_pre_altered_gxt(data: &[u8]) -> Result<knife_render::DeferredGxt<'_>> {
    let image = compact::parse(data)?;
    anyhow::ensure!(image.encoding == compact::Encoding::GxtAlterSrgb,
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
            compact::Encoding::GxtAlterSrgb => anyhow::bail!("pre-altered GXT requires the native Vita block upload path"),
            _ => anyhow::bail!("unsupported CPU image encoding"),
        };
        anyhow::ensure!(image::guess_format(image.payload)? == format, "KIMG payload format mismatch");
        #[cfg(target_os = "vita")]
        {
            let (w, h, pixels) = decode_native_image(image.payload, rgba_cap)?;
            anyhow::ensure!([w, h] == image.size, "KIMG encoded dimensions mismatch");
            return Ok((w, h, pixels, image.is_srgb()));
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
        return Ok((w, h, pixels, true));
    }
    let img = image::load_from_memory(data).context("decoding image")?.to_rgba8();
    Ok((img.width(), img.height(), img.into_raw(), true))
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
    Ok((image.width(), image.height(), image.pixels().to_vec()))
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
fn gxt_alter_route(codec8: bool, cpu_grade: bool, strip: bool, raw: bool) -> std::result::Result<bool, &'static str> {
    if !codec8 { return Ok(false); }
    if cpu_grade { return Err("CPU grade"); }
    if strip { return Err("language strip"); }
    if raw { return Err("raw sample"); }
    Ok(true)
}
// END standalone CG GXT route.

// BEGIN standalone CG GXT routing tests (run from a host rustc harness).
#[cfg(test)]
mod cg_gxt_route_tests {
    use super::gxt_alter_route;

    #[test]
    fn default_alter_codec8_uses_deferred_blocks() {
        assert_eq!(gxt_alter_route(true, false, false, false), Ok(true));
        assert_eq!(gxt_alter_route(false, false, false, false), Ok(false));
    }

    #[test]
    fn cpu_grade_raw_and_language_strip_refuse_prebaked_colour() {
        assert_eq!(gxt_alter_route(true, true, false, false), Err("CPU grade"));
        assert_eq!(gxt_alter_route(true, false, true, false), Err("language strip"));
        assert_eq!(gxt_alter_route(true, false, false, true), Err("raw sample"));
    }
}
// END standalone CG GXT routing tests.

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


