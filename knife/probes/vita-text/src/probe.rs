use std::{fs::File, io::{Read, Write}, path::Path, sync::Arc};

use anyhow::{Context, Result, ensure};
use knife_core::{exe::Nso, format::{bffnt, bfttf, sfnt}, linebreak::LineBreak};
use ab_glyph::{Font, FontArc};
use knife_render::{Blend, CANVAS_H, CANVAS_W, Quad, Renderer, Shade};
use crate::text::{self, FontAtlas, Layout, PlacedChar};

pub const FONT_FILES: [&str; 6] = [
    "en/HelveticaNeueLTGEO-55Roman.otf",
    "zc/arshanghaisonggbpro_lt.otf",
    "nintendo_udsg-r_std_003.bfttf",
    "nintendo_udsg-r_org_zh-cn_003.bfttf",
    "nintendo_udsg-r_ext_zh-cn_003.bfttf",
    "exfont_nx01.bffnt",
];
const MAX_FONT_BYTES: u64 = 8 * 1024 * 1024;

pub struct FontInputs {
    pub english: Vec<u8>,
    pub chengkong: Vec<u8>,
    pub standard: Vec<u8>,
    pub chinese_original: Vec<u8>,
    pub chinese_extension: Vec<u8>,
    pub exfont: Vec<u8>,
}

impl FontInputs {
    pub fn load(root: &Path) -> Result<Self> {
        let read = |name: &str| -> Result<Vec<u8>> {
            let path = root.join(name);
            let mut file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
            let length = file.metadata()?.len();
            ensure!((1..=MAX_FONT_BYTES).contains(&length), "font length outside bounded input: {name}");
            let mut bytes = Vec::with_capacity(length as usize);
            (&mut file).take(MAX_FONT_BYTES + 1).read_to_end(&mut bytes)?;
            ensure!(bytes.len() as u64 == length && file.metadata()?.len() == length,
                "font changed or exceeded bounded input: {name}");
            Ok(bytes)
        };
        Ok(Self {
            english: read(FONT_FILES[0])?, chengkong: read(FONT_FILES[1])?,
            standard: read(FONT_FILES[2])?, chinese_original: read(FONT_FILES[3])?,
            chinese_extension: read(FONT_FILES[4])?, exfont: read(FONT_FILES[5])?,
        })
    }
    pub fn log_inputs(&self, log: &mut impl Write) -> Result<()> {
        for (name, bytes) in FONT_FILES.into_iter().zip([
            &self.english, &self.chengkong, &self.standard, &self.chinese_original,
            &self.chinese_extension, &self.exfont,
        ]) {
            writeln!(log, "INPUT {name} bytes={}", bytes.len())?;
        }
        Ok(())
    }
    pub fn shared(&self, language: Language) -> Result<FontAtlasInputs> {
        let raw: Vec<&[u8]> = match language {
            Language::English => vec![&self.standard],
            Language::Chengkong => vec![&self.chinese_original, &self.chinese_extension, &self.standard],
        };
        let fonts = raw.into_iter().map(|data| bfttf::decode(data).context("decode actual shared BFTTF font"))
            .collect::<Result<Vec<_>>>()?;
        Ok(FontAtlasInputs { fonts })
    }
}

pub struct FontAtlasInputs { pub fonts: Vec<Vec<u8>> }

pub const ENGLISH_SAMPLE: &str = "@c1<Magic|mah-gic> in the holy night. @iAoko and Alice@r walk home.@nSometimes, it's our heroine: öà and ‘quotes’. ——";
pub const CHENGKONG_SAMPLE: &str = "@c1<魔法使|魔法师>之夜，澄空汉化。@n@i青子与有珠@r走进夜色。——@n英文 English 与中文，标点不应单独起行！[heart]";

#[derive(Clone, Copy, Debug)]
pub enum Language { English, Chengkong }
impl Language {
    pub fn id(self) -> usize { match self { Self::English => 1, Self::Chengkong => 2 } }
    pub fn label(self) -> &'static str { match self { Self::English => "EN", Self::Chengkong => "ZC-Chengkong" } }
}

pub struct PreparedPage {
    pub atlas: FontAtlas,
    pub quads: Vec<Quad>,
    pub placed: Vec<PlacedChar>,
    pub metrics: sfnt::SetMetrics,
    pub system_atlas: FontAtlas,
    pub system_metrics: sfnt::SetMetrics,
    pub layout: Layout,
    pub final_pen: (f32, f32, usize),
    pub language: Language,
}

fn metrics(fonts: &[&[u8]], exfont: &bffnt::Font) -> Result<sfnt::SetMetrics> {
    let design = fonts.iter().map(|font| sfnt::metrics(font).context("original font design metrics"))
        .collect::<Result<Vec<_>>>()?;
    let scaled = sfnt::set_metrics(&design, text::BASE_SIZE).context("original font set metrics")?;
    Ok(sfnt::pair_metrics(scaled, Some(sfnt::SetMetrics {
        height: exfont.height.into(), ascent: exfont.ascent.into(),
        line_feed: exfont.line_feed.into(), baseline: exfont.baseline.into(),
    })))
}

pub fn original_line_break() -> Result<()> {
    let nso = Nso::from_bundle(include_bytes!("../../../knife/data/exe_tables_witch.bin"))?;
    text::set_line_break(Arc::new(LineBreak::load(&nso)?));
    Ok(())
}

fn normalized_face(data: &[u8]) -> Result<FontArc> {
    let mut normalized = data.to_vec();
    knife_core::format::cff::normalize(&mut normalized);
    Ok(FontArc::try_from_vec(normalized)?)
}

pub fn prepare_page(renderer: &Renderer, fonts: &FontInputs, language: Language) -> Result<PreparedPage> {
    original_line_break()?;
    text::set_language(language.id());
    let message = match language { Language::English => &fonts.english, Language::Chengkong => &fonts.chengkong };
    let exfont = bffnt::Font::parse(&fonts.exfont)?;
    let metrics = metrics(&[message], &exfont)?;
    let shared = fonts.shared(language)?;
    // Assets::font_metrics selects shared_fonts().first() for Witch's bubble
    // slot metrics. The atlas below still retains the complete fallback chain.
    let first_shared = shared.fonts.first().context("empty shared font priority")?;
    let system_metrics = self::metrics(&[first_shared], &exfont)?;
    text::set_font_metrics(metrics.line_feed, metrics.ascent, metrics.baseline,
        system_metrics.ascent, system_metrics.line_feed);

    let mut atlas = FontAtlas::new(renderer, message.clone())?;
    atlas.load_exfont(renderer, &fonts.exfont)?;
    let mut shared_fonts = shared.fonts.into_iter();
    let mut system_atlas = FontAtlas::new(renderer, shared_fonts.next().context("empty shared font priority")?)?;
    for font in shared_fonts { system_atlas.add_fallback(font)?; }
    system_atlas.load_exfont(renderer, &fonts.exfont)?;

    let size = 56.0 * text::glyph_scale();
    let layout = Layout {
        left: 96.0, right: 1824.0, size, spacing: 2.0, sx: 1.0,
        line_height: text::line_feed() * size / text::BASE_SIZE + 28.0,
        ascent: text::msg_ascent(58.0), ruby_y: -2.0,
    };
    let mut placed = Vec::new();
    let sample = match language { Language::English => ENGLISH_SAMPLE, Language::Chengkong => CHENGKONG_SAMPLE };
    let runs = text::parse_markup(sample);
    let final_pen = text::layout(&atlas, &layout, &runs, layout.left, 260.0, 0, &mut placed);
    ensure!(placed.len() >= 20 && final_pen.2 > 0, "original bilingual layout produced no text");
    // A missing glyph would silently choose .notdef in the original engine.
    // Fail this probe instead, so placeholders cannot satisfy the integration.
    let face = normalized_face(message)?;
    for pc in &placed {
        ensure!(pc.bar.is_some() || atlas.has_ex(pc.c) || face.glyph_id(pc.c).0 != 0,
            "{} actual message face lacks U+{:04X}", language.label(), pc.c as u32);
        ensure!([pc.x, pc.baseline, pc.size, pc.sx, pc.stretch, pc.italic].iter().all(|v| v.is_finite()),
            "nonfinite original placed character");
    }
    let mut quads = Vec::new();
    let header = match language { Language::English => "EN / original FontAtlas - R8 partial upload", Language::Chengkong => "澄空中文 / 原始文字图集与排版" };
    system_atlas.draw_str_slot_as(renderer, &mut quads, header, [38.0, 38.0], 1.0,
        [96.0, 100.0], [0.75, 0.9, 1.0, 1.0], true);
    for pc in &placed {
        let rgb = text::fill_rgb(pc, [1.0; 3]);
        let q = if pc.order % 2 == 0 {
            atlas.placed_quad(renderer, pc, 0.0, 0.0, [rgb[0], rgb[1], rgb[2], 1.0])
        } else {
            atlas.rimmed_quad_edge(renderer, pc, text::edge_rim(1, pc.size), text::RIM_ALPHA,
                rgb, [24, 32, 48], 1.0)
        };
        if let Some(q) = q { quads.push(q); }
    }
    ensure!(quads.len() > 20 && quads.iter().all(|q| q.blend == Blend::Alpha &&
        matches!(q.shade, Shade::Text | Shade::RimText)), "text did not produce actual Text/RimText alpha draws");
    Ok(PreparedPage { atlas, quads, placed, metrics, system_atlas, system_metrics, layout, final_pen, language })
}

pub fn run_text_probe(renderer: &mut Renderer, root: &Path, log: &mut impl Write) -> Result<()> {
    let fonts = FontInputs::load(root)?;
    fonts.log_inputs(log)?;
    for language in [Language::English, Language::Chengkong] {
        let stage = match language { Language::English => "EN", Language::Chengkong => "ZC" };
        writeln!(log, "STAGE {stage} BEGIN")?;
        log.flush()?;
        let page = prepare_page(renderer, &fonts, language)?;
        let ordinary = page.quads.iter().filter(|q| matches!(q.shade, Shade::Text)).count();
        let rimmed = page.quads.iter().filter(|q| matches!(q.shade, Shade::RimText)).count();
        writeln!(log, "{} placed={} quads={} Text={ordinary} RimText={rimmed} metrics={:?} shared={:?}",
            language.label(), page.placed.len(), page.quads.len(), page.metrics, page.system_metrics)?;
        writeln!(log, "ATLAS message=2048x2048 R8 shared=2048x2048 R8; logical canvas={CANVAS_W}x{CANVAS_H} physical=960x540")?;
        log.flush()?;
        let target = renderer.try_create_scene_target(CANVAS_W, CANVAS_H, "text probe canvas")?;
        let mut encoder = renderer.encoder();
        renderer.try_draw(&mut encoder, &target, Some([0.015, 0.025, 0.05, 1.0]), &page.quads)?;
        renderer.try_present(encoder, &target)?;
        writeln!(log, "{} native upload/submission completed; independent screenshot verification still required", language.label())?;
        writeln!(log, "STAGE {stage} FRAME READY")?;
        log.flush()?;
        // Keep the submitted resources alive while the parent runner captures
        // each language. Host tests record commands and deliberately do not wait.
        #[cfg(target_os = "vita")]
        std::thread::sleep(std::time::Duration::from_secs(4));
        writeln!(log, "STAGE {stage} HOLD COMPLETE")?;
        log.flush()?;
    }
    Ok(())
}
