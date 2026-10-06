//! The A2 default is embedded unmodified, independent of OS fonts and ref/.
//! All workers, names, sizes and styles retain the same parsed allocation.
use super::{Budget, Coverage, Face, FontArc, Permit};
use std::sync::{Arc, OnceLock};

pub const LICENSE: &str = include_str!("../../fonts/QiushuiShotai-LICENSE.txt");
/// Shared immutable font for host UI rasterizers.
pub fn font() -> FontArc {
    face().font.clone()
}
/// Embedded font bytes for UI backends that own their font allocation.
pub fn data() -> &'static [u8] {
    &DATA
}
// One allocation shared by the engine and host UI, including across codegen units.
static DATA: [u8; 29_202_800] = *include_bytes!("../../fonts/QiushuiShotai.ttf");
const ALLOWANCE: usize = 32 * 1024 * 1024;

struct Bundled {
    face: Arc<Face>,
    // Retain the license in the executable alongside the embedded font.
    _license: &'static str,
}
static BUNDLED: OnceLock<Bundled> = OnceLock::new();
static BUDGET: OnceLock<Budget> = OnceLock::new();

/// This is a conservative reservation for the font bytes, parsed tables and
/// license, not a measurement of process RSS. The ordinary 24 MiB pool is separate.
fn budget() -> &'static Budget {
    BUDGET.get_or_init(|| Budget::new(ALLOWANCE))
}

pub(super) fn is_family(name: &str) -> bool {
    name.eq_ignore_ascii_case(krkr_protocol::text::DEFAULT_FONT_FACE) || name == "秋水書体"
}

pub(super) fn is_loaded() -> bool {
    BUNDLED.get().is_some()
}

pub(super) fn face() -> Arc<Face> {
    BUNDLED
        .get_or_init(|| {
            let permit: Permit = budget().reserve(ALLOWANCE).expect("one bundled font");
            let metadata = ttf_parser::Face::parse(&DATA, 0).expect("validated bundled font");
            let units = f32::from(metadata.units_per_em());
            let underline = metadata
                .underline_metrics()
                .map_or([-units * 0.1, units * 0.05], |m| {
                    [f32::from(m.position), f32::from(m.thickness)]
                });
            Bundled {
                face: Arc::new(Face {
                    // FontRef borrows static bytes; FontArc shares parsed data.
                    font: FontArc::try_from_slice(&DATA).expect("validated bundled font"),
                    ascent: Face::ascent(&metadata),
                    bold: metadata.is_bold(),
                    italic: metadata.is_italic(),
                    underline,
                    coverage: Coverage::from_face(&metadata),
                    permit,
                }),
                _license: LICENSE,
            }
        })
        .face
        .clone()
}
