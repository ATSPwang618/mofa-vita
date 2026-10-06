//! addFont.dll: portable private registration, including fonts inside VFS archives.
//! Behavior reference: krkrsdl3/plugins/addFont.cpp and core/media/font/TVPFont.cpp.
krkr_engine::native_plugin! {
    pub(crate) AddFont {
        names: ["addFont.dll", "addFont.tpm"],
        classes: [],
        extensions: [("System", "addFont", add::CALL)],
    }
}
#[tjs_bind::function(resumable = true)]
fn add(
    cx: &mut tjs_core::NativeCx<'_>,
    args: tjs_bind::RestArgs<'_>,
) -> tjs_core::NativeResult<tjs_core::NativeStep> {
    let name = tjs_core::value::to_string_units(cx.heap(), crate::exports::arg(args, 0)?)?;
    // extract and extra parameters are ignored by the SDL implementation.
    krkr_engine::extensions::register_font(cx, &name)
}
