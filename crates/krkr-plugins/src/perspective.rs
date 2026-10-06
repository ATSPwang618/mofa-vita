//! krkr2/cpp/plugins/layerExPerspective.cpp's active main-image implementation.
//! Reference module name: perspective.dll; layerExPerspective is the source name.
krkr_engine::native_plugin! {
    pub(crate) Perspective {
        names: ["perspective.dll", "perspective.tpm"],
        classes: [],
        extensions: [("Layer", "perspectiveCopy",
            tjs_core::NativeCallable::Resumable(krkr_engine::extensions::layer_perspective))],
    }
}
