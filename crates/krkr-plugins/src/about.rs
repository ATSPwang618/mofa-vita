//! getabout.dll exports the same System entry point as windowEx.dll.
krkr_engine::native_plugin! {
    pub(crate) About {
        names: ["getabout.dll", "getabout.tpm"],
        classes: [],
        extensions: [("System", "getAboutString", tjs_core::NativeCallable::Leaf(crate::window::about))],
    }
}
