//! Portable implementation of the historical DrawDeviceD3D object graph.
mod attached;
mod compose;
pub(crate) mod device;
mod image;
pub(crate) mod layer;
mod picture;
use krkr_engine::plugins::{Context, Exports, Plugin};
use tjs_core::{NativeResult, Value};

#[derive(Default, tjs_bind::Trace)]
pub(crate) struct DrawDevice {
    exports: Exports,
}
krkr_engine::native_plugin! { impl DrawDevice { names: ["drawdeviceD3D.dll", "drawdeviceD3D.tpm"] } }
static D3D: tjs_core::NativeClass = tjs_core::NativeClass {
    name: "D3D",
    ..device::bindings::CLASS
};
impl Plugin for DrawDevice {
    fn dependencies(&self) -> &'static [&'static str] {
        &["emoteplayer.dll"]
    }
    fn link(&mut self, cx: &mut Context<'_>) -> NativeResult<()> {
        let mut exports = Exports::default();
        for (name, class) in [
            ("DrawDeviceD3D", device::bindings::install(cx.heap)?),
            ("D3D", cx.heap.register_class(&D3D)?),
            ("D3DLayer", layer::bindings::install(cx.heap)?),
            ("D3DImage", image::bindings::install(cx.heap)?),
            ("D3DPicture", picture::bindings::install(cx.heap)?),
            ("D3DEmotePlayer", crate::emote::device_player::install(cx)?),
        ] {
            exports.value(cx, cx.global, name, Value::Obj(class.into()))?;
        }
        let layer = layer::bindings::install(cx.heap)?;
        for (name, value) in [
            ("DrawPlaneBoth", 0),
            ("DrawPlaneFront", 1),
            ("DrawPlaneBack", 2),
        ] {
            cx.export_member(layer, name, Value::Int(value))?;
        }
        attached::install(cx, &mut exports)?;
        self.exports = exports;
        Ok(())
    }
    fn unlink(&mut self, cx: &mut Context<'_>) -> NativeResult<bool> {
        self.exports.unlink(cx)
    }
}
