//! Portable TextRenderBase from krkrsdl3/plugins/textrender.cpp.
mod model;
mod parsing;
use krkr_engine::extensions;
use model::{Character, Layout, Metrics};
use tjs_bind::{Array, IntoTjs, Utf16};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, ObjRef, Value,
    value,
};
krkr_engine::native_plugin! {
    pub(crate) TextRender { names: ["textrender.dll", "textrender.tpm"], classes: [binding], extensions: [] }
}
#[tjs_bind::class(name = "TextRenderBase")]
mod binding {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub(super) layout: Layout,
    }
    impl tjs_core::Trace for State {
        fn trace(&self, _: &mut dyn FnMut(Value)) {}
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.layout = Layout::default();
        }
        #[tjs::method(name = "render", resumable = true)]
        fn render(
            cx: &mut NativeCx<'_>,
            text: Utf16,
            #[tjs(coerce)] _auto_indent: i32,
            #[tjs(coerce)] _diff: i32,
            #[tjs(coerce)] _all: i32,
            #[tjs(coerce)] _same: bool,
        ) -> NativeResult<NativeStep> {
            parsing::start(cx, text.0)
        }
        #[tjs::method(name = "setRenderSize", resumable = true)]
        fn size(
            cx: &mut NativeCx<'_>,
            #[tjs(coerce)] width: i32,
            #[tjs(coerce)] height: i32,
        ) -> NativeResult<NativeStep> {
            let owner = cx.this();
            with_state(cx, owner, |s| {
                s.layout.width = width;
                s.layout.height = height;
            })?;
            parsing::finish(cx, owner, true)
        }
        #[tjs::method(name = "clear", resumable = true)]
        fn clear(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            parsing::finish(cx, cx.this(), true)
        }
        #[tjs::method(name = "done", resumable = true)]
        fn done(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            parsing::finish(cx, cx.this(), false)
        }
        #[tjs::method(name = "setDefault", resumable = true)]
        fn set_default(cx: &mut NativeCx<'_>, settings: Value) -> NativeResult<NativeStep> {
            DefaultsRead::start(cx, settings)
        }
        #[tjs::method(name = "setOption")]
        fn set_option(&self, settings: Value) -> NativeResult<()> {
            object_or_null(settings)?;
            Ok(())
        }
        #[tjs::method(name = "getCharacters")]
        fn characters(
            &self,
            cx: &mut NativeCx<'_>,
            #[tjs(coerce)] start: i32,
            #[tjs(coerce)] end: i32,
        ) -> NativeResult<Value> {
            let mut out = Vec::new();
            if end < start || (start == 0 && end == 0) {
                for c in &self.layout.characters {
                    out.push(character(cx, c)?);
                }
            }
            Array(out).into_tjs(cx.heap_mut())
        }
        #[tjs::method(name = "resetFont")]
        fn reset_font(&mut self) {
            self.layout.style = self.layout.defaults.clone();
        }
        #[tjs::method(name = "resetStyle")]
        fn reset_style(&mut self) {
            self.layout.style = self.layout.defaults.clone();
        }
        #[tjs::method(name = "getKeyWait")]
        fn get_wait(&self) -> Array<Vec<Value>> {
            Array(Vec::new())
        }
        #[tjs::getter(name = "keyWait")]
        fn key_wait(&self) -> Array<Vec<Value>> {
            Array(Vec::new())
        }
        #[tjs::setter(name = "keyWait")]
        fn set_wait(&self, _value: Value) -> NativeResult<()> {
            Err(NativeError::Message("no support setKeyWait"))
        }
        #[tjs::method(name = "calcShowCount")]
        fn count(&self) -> i64 {
            self.layout.characters.len() as i64
        }
        #[tjs::getter(name = "renderText")]
        fn render_text(&self) -> Utf16 {
            Utf16(self.layout.style.text.clone())
        }
        #[tjs::setter(name = "renderText")]
        fn set_render_text(&mut self, #[tjs(coerce)] text: Utf16) {
            self.layout.style.text = tjs_core::string::c_string(&text.0).to_vec();
        }
        #[tjs::getter(name = "bold")]
        fn get_style_bold(&self) -> bool {
            self.layout.style.bold
        }
        #[tjs::setter(name = "bold")]
        fn set_style_bold(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.style.bold = value as i32 != 0;
        }
        #[tjs::getter(name = "italic")]
        fn get_style_italic(&self) -> bool {
            self.layout.style.italic
        }
        #[tjs::setter(name = "italic")]
        fn set_style_italic(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.style.italic = value as i32 != 0;
        }
        #[tjs::getter(name = "face")]
        fn get_style_face(&self) -> Utf16 {
            Utf16(self.layout.style.face.clone())
        }
        #[tjs::setter(name = "face")]
        fn set_style_face(&mut self, value: Utf16) {
            self.layout.style.face = tjs_core::string::c_string(&value.0).to_vec();
        }
        #[tjs::getter(name = "fontSize")]
        fn get_style_fontsize(&self) -> i64 {
            self.layout.style.fontsize as i64
        }
        #[tjs::setter(name = "fontSize")]
        fn set_style_fontsize(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.style.fontsize = value as i32;
        }
        #[tjs::getter(name = "fontScale")]
        fn get_style_fontscale(&self) -> f64 {
            self.layout.style.fontscale
        }
        #[tjs::setter(name = "fontScale")]
        fn set_style_fontscale(&mut self, #[tjs(coerce)] value: f64) {
            self.layout.style.fontscale = value;
        }
        #[tjs::getter(name = "chColor")]
        fn get_style_color(&self) -> i64 {
            self.layout.style.color as i64
        }
        #[tjs::setter(name = "chColor")]
        fn set_style_color(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.style.color = value as i32;
        }
        #[tjs::getter(name = "rubySize")]
        fn get_style_ruby_size(&self) -> i64 {
            self.layout.style.ruby_size as i64
        }
        #[tjs::setter(name = "rubySize")]
        fn set_style_ruby_size(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.style.ruby_size = value as i32;
        }
        #[tjs::getter(name = "rubyOffset")]
        fn get_style_ruby_offset(&self) -> i64 {
            self.layout.style.ruby_offset as i64
        }
        #[tjs::setter(name = "rubyOffset")]
        fn set_style_ruby_offset(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.style.ruby_offset = value as i32;
        }
        #[tjs::getter(name = "shadow")]
        fn get_style_shadow(&self) -> bool {
            self.layout.style.shadow
        }
        #[tjs::setter(name = "shadow")]
        fn set_style_shadow(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.style.shadow = value as i32 != 0;
        }
        #[tjs::getter(name = "edge")]
        fn get_style_edge(&self) -> bool {
            self.layout.style.edge
        }
        #[tjs::setter(name = "edge")]
        fn set_style_edge(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.style.edge = value as i32 != 0;
        }
        #[tjs::getter(name = "lineSpacing")]
        fn get_style_line_spacing(&self) -> i64 {
            self.layout.style.line_spacing as i64
        }
        #[tjs::setter(name = "lineSpacing")]
        fn set_style_line_spacing(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.style.line_spacing = value as i32;
        }
        #[tjs::getter(name = "pitch")]
        fn get_style_pitch(&self) -> i64 {
            self.layout.style.pitch as i64
        }
        #[tjs::setter(name = "pitch")]
        fn set_style_pitch(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.style.pitch = value as i32;
        }
        #[tjs::getter(name = "lineSize")]
        fn get_style_line_size(&self) -> i64 {
            self.layout.style.line_size as i64
        }
        #[tjs::setter(name = "lineSize")]
        fn set_style_line_size(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.style.line_size = value as i32;
        }
        #[tjs::getter(name = "defaultBold")]
        fn get_defaults_bold(&self) -> bool {
            self.layout.defaults.bold
        }
        #[tjs::setter(name = "defaultBold")]
        fn set_defaults_bold(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.defaults.bold = value as i32 != 0;
        }
        #[tjs::getter(name = "defaultItalic")]
        fn get_defaults_italic(&self) -> bool {
            self.layout.defaults.italic
        }
        #[tjs::setter(name = "defaultItalic")]
        fn set_defaults_italic(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.defaults.italic = value as i32 != 0;
        }
        #[tjs::getter(name = "defaultFace")]
        fn get_defaults_face(&self) -> Utf16 {
            Utf16(self.layout.defaults.face.clone())
        }
        #[tjs::setter(name = "defaultFace")]
        fn set_defaults_face(&mut self, value: Utf16) {
            self.layout.defaults.face = tjs_core::string::c_string(&value.0).to_vec();
        }
        #[tjs::getter(name = "defaultFontSize")]
        fn get_defaults_fontsize(&self) -> i64 {
            self.layout.defaults.fontsize as i64
        }
        #[tjs::setter(name = "defaultFontSize")]
        fn set_defaults_fontsize(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.defaults.fontsize = value as i32;
        }
        #[tjs::getter(name = "defaultFontScale")]
        fn get_defaults_fontscale(&self) -> f64 {
            self.layout.defaults.fontscale
        }
        #[tjs::setter(name = "defaultFontScale")]
        fn set_defaults_fontscale(&mut self, #[tjs(coerce)] value: f64) {
            self.layout.defaults.fontscale = value as f32 as f64;
        }
        #[tjs::getter(name = "defaultChColor")]
        fn get_defaults_color(&self) -> i64 {
            self.layout.defaults.color as i64
        }
        #[tjs::setter(name = "defaultChColor")]
        fn set_defaults_color(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.defaults.color = value as i32;
        }
        #[tjs::getter(name = "defaultRubySize")]
        fn get_defaults_ruby_size(&self) -> i64 {
            self.layout.defaults.ruby_size as i64
        }
        #[tjs::setter(name = "defaultRubySize")]
        fn set_defaults_ruby_size(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.defaults.ruby_size = value as i32;
        }
        #[tjs::getter(name = "defaultRubyOffset")]
        fn get_defaults_ruby_offset(&self) -> i64 {
            self.layout.defaults.ruby_offset as i64
        }
        #[tjs::setter(name = "defaultRubyOffset")]
        fn set_defaults_ruby_offset(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.defaults.ruby_offset = value as i32;
        }
        #[tjs::getter(name = "defaultShadow")]
        fn get_defaults_shadow(&self) -> bool {
            self.layout.defaults.shadow
        }
        #[tjs::setter(name = "defaultShadow")]
        fn set_defaults_shadow(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.defaults.shadow = value as i32 != 0;
        }
        #[tjs::getter(name = "defaultEdge")]
        fn get_defaults_edge(&self) -> bool {
            self.layout.defaults.edge
        }
        #[tjs::setter(name = "defaultEdge")]
        fn set_defaults_edge(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.defaults.edge = value as i32 != 0;
        }
        #[tjs::getter(name = "defaultLineSpacing")]
        fn get_defaults_line_spacing(&self) -> i64 {
            self.layout.defaults.line_spacing as i64
        }
        #[tjs::setter(name = "defaultLineSpacing")]
        fn set_defaults_line_spacing(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.defaults.line_spacing = value as i32;
        }
        #[tjs::getter(name = "defaultPitch")]
        fn get_defaults_pitch(&self) -> i64 {
            self.layout.defaults.pitch as i64
        }
        #[tjs::setter(name = "defaultPitch")]
        fn set_defaults_pitch(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.defaults.pitch = value as i32;
        }
        #[tjs::getter(name = "defaultLineSize")]
        fn get_defaults_line_size(&self) -> i64 {
            self.layout.defaults.line_size as i64
        }
        #[tjs::setter(name = "defaultLineSize")]
        fn set_defaults_line_size(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.defaults.line_size = value as i32;
        }
        #[tjs::getter(name = "defaultValign")]
        fn get_defaults_valign(&self) -> i64 {
            self.layout.defaults.valign as i64
        }
        #[tjs::setter(name = "defaultValign")]
        fn set_defaults_valign(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.defaults.valign = value as i32;
        }
        #[tjs::getter(name = "renderOver")]
        fn get_render_over(&self) -> bool {
            self.layout.style.over
        }
        #[tjs::setter(name = "renderOver")]
        fn set_render_over(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.style.over = value as i32 != 0;
        }
        #[tjs::getter(name = "renderDelay")]
        fn get_render_delay(&self) -> i64 {
            self.layout.style.delay as i64
        }
        #[tjs::setter(name = "renderDelay")]
        fn set_render_delay(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.style.delay = value as i32;
        }
        #[tjs::getter(name = "vertical")]
        fn get_vertical(&self) -> bool {
            self.layout.vertical
        }
        #[tjs::setter(name = "vertical")]
        fn set_vertical(&mut self, #[tjs(coerce)] value: i64) {
            self.layout.vertical = value as i32 != 0;
        }
        #[tjs::getter(name = "renderLeft")]
        fn get_render_left(&self) -> i64 {
            self.layout.left as i64
        }
        #[tjs::setter(name = "renderLeft")]
        fn set_render_left(&self, #[tjs(coerce)] _value: i64) -> NativeResult<()> {
            Err(NativeError::Message("renderLeft is read-only"))
        }
        #[tjs::getter(name = "renderRight")]
        fn get_render_right(&self) -> i64 {
            self.layout.right as i64
        }
        #[tjs::setter(name = "renderRight")]
        fn set_render_right(&self, #[tjs(coerce)] _value: i64) -> NativeResult<()> {
            Err(NativeError::Message("renderRight is read-only"))
        }
        #[tjs::getter(name = "renderTop")]
        fn get_render_top(&self) -> i64 {
            self.layout.top as i64
        }
        #[tjs::setter(name = "renderTop")]
        fn set_render_top(&self, #[tjs(coerce)] _value: i64) -> NativeResult<()> {
            Err(NativeError::Message("renderTop is read-only"))
        }
        #[tjs::getter(name = "renderBottom")]
        fn get_render_bottom(&self) -> i64 {
            self.layout.bottom as i64
        }
        #[tjs::setter(name = "renderBottom")]
        fn set_render_bottom(&self, #[tjs(coerce)] _value: i64) -> NativeResult<()> {
            Err(NativeError::Message("renderBottom is read-only"))
        }
        #[tjs::getter(name = "renderCount")]
        fn get_render_count(&self) -> i64 {
            self.layout.style.text.len() as i64
        }
        #[tjs::setter(name = "renderCount")]
        fn set_render_count(&self, #[tjs(coerce)] _value: i64) -> NativeResult<()> {
            Err(NativeError::Message("renderCount is read-only"))
        }
    }
}
fn object_or_null(value: Value) -> NativeResult<Option<ObjId>> {
    match value {
        Value::Obj(o) => Ok(o.object),
        _ => Err(NativeError::Type("an object or null")),
    }
}
fn string(cx: &mut NativeCx<'_>, s: &[u16]) -> Value {
    Value::Str(cx.heap_mut().alloc_string(s.to_vec()))
}
fn character(cx: &mut NativeCx<'_>, c: &Character) -> NativeResult<Value> {
    let dict = cx.heap_mut().alloc_dictionary();
    let values = [
        ("bold", Value::Int(c.bold as i64)),
        ("italic", Value::Int(c.italic as i64)),
        ("graph", Value::Int(c.graph as i64)),
        ("vertical", Value::Int(0)),
        ("x", Value::Int(c.x as i64)),
        ("y", Value::Int(c.y as i64)),
        ("cw", Value::Int(c.width as i64)),
        ("size", Value::Int(c.size as i64)),
        ("color", Value::Int(c.color as i64)),
        (
            "edge",
            if c.edge == 0 {
                Value::Void
            } else {
                Value::Int(c.edge as i64)
            },
        ),
        (
            "shadow",
            if c.shadow == 0 {
                Value::Void
            } else {
                Value::Int(c.shadow as i64)
            },
        ),
        ("face", string(cx, &c.face)),
        ("text", string(cx, &c.text)),
    ];
    for (name, v) in values {
        let key = cx
            .heap_mut()
            .intern(&name.encode_utf16().collect::<Vec<_>>());
        cx.heap_mut().set_member(dict, key, v)?;
    }
    Ok(Value::Obj(ObjRef::bound(dict)))
}
const DEFAULT_KEYS: &[&str] = &[
    "bold",
    "italic",
    "fontsize",
    "fontscale",
    "face",
    "chColor",
    "rubySize",
    "rubyOffset",
    "shadow",
    "shadowColor",
    "edge",
    "edgeColor",
    "lineSpacing",
    "pitch",
    "lineSize",
    "align",
    "valign",
];
#[derive(tjs_bind::Trace)]
struct DefaultsRead {
    owner: ObjId,
    source: Value,
    index: usize,
}
impl DefaultsRead {
    fn start(cx: &mut NativeCx<'_>, source: Value) -> NativeResult<NativeStep> {
        let Some(id) = object_or_null(source)? else {
            return Ok(NativeStep::Return(Value::Void));
        };
        Self {
            owner: cx.this(),
            source: Value::Obj(ObjRef::bound(id)),
            index: 0,
        }
        .next(cx)
    }
    fn next(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.index == DEFAULT_KEYS.len() {
            return Ok(NativeStep::Return(Value::Void));
        }
        let key = string(
            cx,
            &DEFAULT_KEYS[self.index].encode_utf16().collect::<Vec<_>>(),
        );
        Ok(NativeStep::GetOr {
            object: self.source,
            key,
            raw: false,
            fallback: Value::Void,
            continuation: Box::new(self),
        })
    }
}
impl NativeContinuation for DefaultsRead {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<NativeStep> {
        if !matches!(v, Value::Void) {
            let key = DEFAULT_KEYS[self.index];
            let text = if key == "face" {
                Some(match v {
                    Value::Str(s) => tjs_core::string::c_string(cx.heap().string(s)?).to_vec(),
                    _ => return Err(NativeError::Type("a string font face")),
                })
            } else {
                None
            };
            let real = if key == "fontscale" {
                value::to_real(cx.heap(), v)?
            } else {
                0.
            };
            let n = if text.is_none() && key != "fontscale" {
                value::to_integer(cx.heap(), v)? as i32
            } else {
                0
            };
            binding::with_state(cx, self.owner, |s| {
                let d = &mut s.layout.defaults;
                match key {
                    "bold" => d.bold = n != 0,
                    "italic" => d.italic = n != 0,
                    "fontsize" => d.fontsize = n,
                    "fontscale" => d.fontscale = real,
                    "face" => d.face = text.unwrap(),
                    "chColor" => d.color = n,
                    "rubySize" => d.ruby_size = n,
                    "rubyOffset" => d.ruby_offset = n,
                    "shadow" => d.shadow = n != 0,
                    "shadowColor" => d.shadow_color = n,
                    "edge" => d.edge = n != 0,
                    "edgeColor" => d.edge_color = n,
                    "lineSpacing" => d.line_spacing = n,
                    "pitch" => d.pitch = n,
                    "lineSize" => d.line_size = n,
                    "align" => d.align = n,
                    "valign" => d.valign = n,
                    _ => {}
                }
            })?;
        }
        self.index += 1;
        self.next(cx)
    }
}
