//! Portable LayerExDraw vector graphics, following the SDL plugin API.
mod appearance;
mod brush;
mod coordinates;
mod font;
mod geometry;
mod image;
mod image_draw;
mod layer;
mod matrix;
mod matrix_class;
mod path;
mod path_class;
mod properties;
mod raster;
mod record;
mod render;

mod constants;
mod text;
use crate::exports::Exports;
use krkr_engine::plugins::{Context, Plugin};
use tjs_core::{NativeResult, Value};
#[derive(Default, tjs_bind::Trace)]
pub(crate) struct LayerDraw {
    exports: Exports,
    fonts: font::Registry,
}
krkr_engine::native_plugin! { impl LayerDraw { names: ["layerExDraw.dll", "layerExDraw.tpm"] } }
impl Plugin for LayerDraw {
    fn link(&mut self, cx: &mut Context<'_>) -> NativeResult<()> {
        let class = namespace::install(cx.heap)?;
        let fonts = font::install(cx.heap, self.fonts.clone())?;
        for (name, nested) in [
            ("PointF", geometry::point::install(cx.heap)?),
            ("RectF", geometry::rect::install(cx.heap)?),
            ("Matrix", matrix_class::bindings::install(cx.heap)?),
            ("Image", image::bindings::install(cx.heap)?),
            ("Font", fonts),
            ("Appearance", appearance::bindings::install(cx.heap)?),
            ("Path", path_class::bindings::install(cx.heap)?),
        ] {
            cx.export_member(class, name, Value::Obj(nested.into()))?;
        }
        for &(name, value) in constants::VALUES {
            cx.export_member(class, name, Value::Int(value))?;
        }
        self.exports
            .function(cx, class, "addPrivateFont", font::add::CALL)?;
        self.exports
            .function(cx, class, "getFontList", font::list::CALL)?;
        self.exports
            .value(cx, cx.global, "GdiPlus", Value::Obj(class.into()))?;
        layer::install(cx, &mut self.exports)
    }
    fn unlink(&mut self, cx: &mut Context<'_>) -> NativeResult<bool> {
        self.exports.unlink(cx)?;
        self.fonts.clear();
        Ok(true)
    }
}
#[tjs_bind::class(name = "GdiPlus", static_class = true)]
mod namespace {
    #[derive(Default, tjs_bind::Trace)]
    pub struct State;
    impl State {
        #[tjs::constructor]
        fn new() -> tjs_core::NativeResult<Self> {
            Err(tjs_core::NativeError::Message(
                "GdiPlus cannot be instantiated",
            ))
        }
    }
}
