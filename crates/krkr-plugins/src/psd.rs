//! Complete public surface of krkr2 cpp/plugins/psdfile. The parser/decoder is
//! VM-free; script property writes and pixel uploads run on the original VM.
mod storage;
use crate::exports::{Exports, arg};
use bindings::with_state as state;
use krkr_engine::{
    assets::{StorageMedium, name},
    extensions,
    plugins::{Context, Plugin},
    protocol::budget::Budget,
    storages,
};
use krkr_image::psd::{self, Decoder, Document, Image, Loader, Meta};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};
use tjs_bind::{Array, Dictionary, IntoTjs, RestArgs, Utf16, flow};
use tjs_core::{
    Heap, MemberFlags, NativeCx, NativeError, NativeResult, NativeStep, ObjId, Value, value,
};

#[derive(Default, tjs_bind::Trace)]
pub(crate) struct Psd {
    exports: Exports,
    #[trace(skip = "RAII registration owns a VM-free storage medium and VFS")]
    registration: Option<storage::Registration>,
}
krkr_engine::native_plugin! { impl Psd { names: ["psd.dll", "psd.tpm"] } }
impl Plugin for Psd {
    fn link(&mut self, cx: &mut Context<'_>) -> NativeResult<()> {
        let vfs = storages::service_from_heap(cx.heap)?;
        let limits = vfs.borrow().limits();
        let budget = extensions::image_staging_budget(cx.heap)?
            .unwrap_or_else(|| Budget::new(limits.max_read_bytes));
        let medium = Arc::new(storage::Medium::new(
            budget,
            vfs.borrow().file_directory().to_vec(),
        ));
        let registration = storage::Registration::new(medium.clone(), vfs)?;
        let class = bindings::install_with_state(
            cx.heap,
            bindings::State {
                medium: Some(medium),
                loaded: None,
                generation: 0,
            },
        )?;
        for (name, id) in [
            ("bitmap", 0),
            ("grayscale", 1),
            ("indexed", 2),
            ("rgb", 3),
            ("cmyk", 4),
            ("multichannel", 7),
            ("duotone", 8),
            ("lab", 9),
        ] {
            cx.export_member(class, &format!("color_mode_{name}"), Value::Int(id))?;
        }
        for (id, (name, _)) in psd::BLENDS.iter().enumerate() {
            cx.export_member(class, &format!("blend_mode_{name}"), Value::Int(id as i64))?;
        }
        self.exports
            .value(cx, cx.global, "PSD", Value::Obj(class.into()))?;
        self.registration = Some(registration);
        Ok(())
    }
    fn unlink(&mut self, cx: &mut Context<'_>) -> NativeResult<bool> {
        self.exports.unlink(cx)?;
        self.registration = None;
        Ok(true)
    }
}
struct Stored {
    doc: Arc<Document>,
    ids: BTreeMap<i32, usize>,
    paths: BTreeMap<Vec<u16>, usize>,
}
impl Stored {
    fn new(doc: Arc<Document>) -> Self {
        let (ids, paths) = doc.storage_index();
        Self { doc, ids, paths }
    }
}
#[tjs_bind::class(name = "PSD")]
mod bindings {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        #[trace(
            skip = "Stored owns parsed PSD data, indexes and byte buffers without TJS handles"
        )]
        pub(super) loaded: Option<Arc<Stored>>,
        #[trace(skip = "Storage medium owns Rust resource caches without TJS handles")]
        pub(super) medium: Option<Arc<storage::Medium>>,
        pub(super) generation: u64,
    }
    impl Drop for State {
        fn drop(&mut self) {
            self.clear();
        }
    }
    impl State {
        #[tjs::constant(name = "layer_type_normal")]
        const NORMAL: i64 = 0;
        #[tjs::constant(name = "layer_type_hidden")]
        const HIDDEN: i64 = 1;
        #[tjs::constant(name = "layer_type_folder")]
        const FOLDER: i64 = 2;
        #[tjs::constant(name = "layer_type_adjust")]
        const ADJUST: i64 = 3;
        #[tjs::constant(name = "layer_type_fill")]
        const FILL: i64 = 4;
        fn clear(&mut self) {
            if let (Some(doc), Some(medium)) = (self.loaded.take(), &self.medium) {
                medium.forget(&doc);
            }
            self.generation = self.generation.wrapping_add(1);
        }
        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>, _args: RestArgs<'_>) -> NativeResult<Self> {
            let class = cx.heap().registered_class("PSD").ok_or(NativeError::This)?;
            let medium = state(cx, class, |s| s.medium.clone())?;
            Ok(Self {
                medium,
                loaded: None,
                generation: 0,
            })
        }
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.clear();
        }
        #[tjs::method(resumable = true)]
        fn load(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let file = value::to_string_units(cx.heap(), arg(args, 0)?)?;
            let owner = cx.this();
            let generation = state(cx, owner, |s| {
                s.clear();
                s.generation
            })?;
            storages::managed::plans(
                cx,
                vec![(file.clone(), false)],
                ((owner, generation), file),
                |((owner, generation), file), cx, mut plans| {
                    let Some(plan) = plans.pop().flatten() else {
                        return Ok(NativeStep::Return(Value::Int(0)));
                    };
                    let plan = Arc::new(plan);
                    let service = storages::service(cx)?;
                    let loader = match Loader::new(plan, service.borrow().limits()) {
                        Ok(loader) => loader,
                        Err(_) => return Ok(NativeStep::Return(Value::Int(0))),
                    };
                    let domain = name::split_name(&file).1.to_vec();
                    Ok(flow::work(
                        Loading {
                            owner,
                            generation,
                            domain,
                            loader: Some(loader),
                        },
                        Loading::poll,
                    ))
                },
            )
        }
        #[tjs::getter]
        fn width(&self) -> i64 {
            self.loaded
                .as_ref()
                .map_or(-1, |s| i64::from(s.doc.size.width))
        }
        #[tjs::getter]
        fn height(&self) -> i64 {
            self.loaded
                .as_ref()
                .map_or(-1, |s| i64::from(s.doc.size.height))
        }
        #[tjs::getter]
        fn channels(&self) -> i64 {
            self.loaded
                .as_ref()
                .map_or(-1, |s| i64::from(s.doc.channels))
        }
        #[tjs::getter]
        fn depth(&self) -> i64 {
            self.loaded.as_ref().map_or(-1, |s| i64::from(s.doc.depth))
        }
        #[tjs::getter(name = "color_mode")]
        fn color_mode(&self) -> i64 {
            self.loaded
                .as_ref()
                .map_or(-1, |s| i64::from(s.doc.color_mode))
        }
        #[tjs::getter(name = "layer_count")]
        fn layer_count(&self) -> i64 {
            self.loaded
                .as_ref()
                .map_or(-1, |s| s.doc.layers.len() as i64)
        }
        #[tjs::method(name = "getLayerName")]
        fn layer_name(cx: &mut NativeCx<'_>, #[tjs(coerce)] no: i32) -> NativeResult<Utf16> {
            let (doc, index) = layer(cx, no)?;
            Ok(Utf16(doc.layers[index].name.clone()))
        }
        #[tjs::method(name = "getLayerType")]
        fn layer_type(cx: &mut NativeCx<'_>, #[tjs(coerce)] no: i32) -> NativeResult<i64> {
            let (doc, index) = layer(cx, no)?;
            Ok(i64::from(doc.layers[index].kind))
        }
        #[tjs::method(name = "getLayerInfo")]
        fn layer_info(cx: &mut NativeCx<'_>, #[tjs(coerce)] no: i32) -> NativeResult<Value> {
            let (doc, index) = layer(cx, no)?;
            let info = doc.layers[index].info(&doc, blend_type(doc.layers[index].blend));
            meta(cx, &info)
        }
        #[tjs::method(name = "getGuides")]
        fn guides(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let doc = document(cx)?;
            meta(cx, &doc.guides)
        }
        #[tjs::method(name = "getSlices")]
        fn slices(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let doc = document(cx)?;
            meta(cx, &doc.slices)
        }
        #[tjs::method(name = "getLayerComp")]
        fn comps(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let doc = document(cx)?;
            meta(cx, &doc.comps)
        }
        #[tjs::method(name = "clearStorageCache", class_only = true)]
        fn clear_cache(cx: &mut NativeCx<'_>) -> NativeResult<()> {
            let class = cx.heap().registered_class("PSD").ok_or(NativeError::This)?;
            if let Some(medium) = state(cx, class, |s| s.medium.clone())? {
                medium.clear_cache();
            }
            Ok(())
        }
        #[tjs::method(name = "getLayerData", resumable = true)]
        fn data(cx: &mut NativeCx<'_>, target: Value, no: Value) -> NativeResult<NativeStep> {
            transfer(cx, target, Some(no), 0)
        }
        #[tjs::method(name = "getLayerDataRaw", resumable = true)]
        fn raw(cx: &mut NativeCx<'_>, target: Value, no: Value) -> NativeResult<NativeStep> {
            transfer(cx, target, Some(no), 1)
        }
        #[tjs::method(name = "getLayerDataMask", resumable = true)]
        fn mask(cx: &mut NativeCx<'_>, target: Value, no: Value) -> NativeResult<NativeStep> {
            transfer(cx, target, Some(no), 2)
        }
        #[tjs::method(name = "getBlend", resumable = true)]
        fn merged(cx: &mut NativeCx<'_>, target: Value) -> NativeResult<NativeStep> {
            transfer(cx, target, None, 3)
        }
    }
    pub(super) fn generation(s: &State) -> u64 {
        s.generation
    }
}
fn error(e: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(e.to_string())
}
fn document(cx: &mut NativeCx<'_>) -> NativeResult<Arc<Document>> {
    state(cx, cx.this(), |s| s.loaded.as_ref().map(|d| d.doc.clone()))?
        .ok_or(NativeError::Message("no data"))
}
fn layer(cx: &mut NativeCx<'_>, index: i32) -> NativeResult<(Arc<Document>, usize)> {
    let doc = document(cx)?;
    if index < 0 || index as usize >= doc.layers.len() {
        return Err(NativeError::Message("not such layer"));
    }
    Ok((doc, index as usize))
}
fn meta(cx: &mut NativeCx<'_>, m: &Meta) -> NativeResult<Value> {
    Metadata(m).into_tjs(cx.heap_mut())
}
struct Metadata<'a>(&'a Meta);
impl IntoTjs for Metadata<'_> {
    fn into_tjs(self, heap: &mut Heap) -> NativeResult<Value> {
        Ok(match self.0 {
            Meta::Void => Value::Void,
            Meta::Int(n) => Value::Int(*n),
            Meta::Real(n) => Value::Real(*n),
            Meta::Text(t) => Utf16(t.clone()).into_tjs(heap)?,
            Meta::List(items) => Array(items.iter().map(Metadata)).into_tjs(heap)?,
            Meta::Map(items) => {
                Dictionary(items.iter().map(|(k, v)| (k, Metadata(v)))).into_tjs(heap)?
            }
        })
    }
}
#[derive(tjs_bind::Trace)]
struct Loading {
    owner: ObjId,
    generation: u64,
    domain: Vec<u16>,
    #[trace(skip = "Loader owns a storage read plan and byte buffers without TJS handles")]
    loader: Option<Loader>,
}
impl Loading {
    fn poll(&mut self, cx: &mut NativeCx<'_>) -> NativeResult<Option<NativeStep>> {
        if state(cx, self.owner, |s| bindings::generation(s))? != self.generation {
            return Err(NativeError::Message("PSD source changed while loading"));
        }
        match self.loader.as_mut().expect("active loader").advance() {
            Ok(false) => Ok(None),
            Err(_) => Ok(Some(NativeStep::Return(Value::Int(0)))),
            Ok(true) => {
                let stored = Arc::new(Stored::new(Arc::new(
                    self.loader.take().expect("finished loader").finish(),
                )));
                state(cx, self.owner, |s| {
                    if let Some(medium) = &s.medium {
                        medium.remember(&self.domain, &stored);
                    }
                    s.loaded = Some(stored);
                })?;
                Ok(Some(NativeStep::Return(Value::Int(1))))
            }
        }
    }
}
fn blend_type(mode: i32) -> i32 {
    // ltPs constants, same mapping as PSD::convBlendMode.
    match mode {
        2 => 25,
        3 => 16,
        4 => 23,
        5 => 15,
        6 => 24,
        7 => 17,
        8 => 21,
        9 => 14,
        10 => 18,
        11 => 20,
        12 => 19,
        17 => 26,
        18 => 28,
        _ => 13,
    }
}
#[derive(tjs_bind::Trace)]
struct Transfer {
    target: Value,
    owner: ObjId,
    generation: u64,
    #[trace(skip = "Decoder owns PSD channels and pixel buffers without TJS handles")]
    decoder: Option<Decoder>,
    result: Value,
}
fn transfer(
    cx: &mut NativeCx<'_>,
    target: Value,
    no: Option<Value>,
    mode: u8,
) -> NativeResult<NativeStep> {
    let budget = extensions::layer_pixel_budget(cx, target)?;
    let owner = cx.this();
    let mut props = VecDeque::new();
    let (doc, image) = if let Some(no) = no {
        let index = value::to_integer(cx.heap(), no)? as i32;
        let (doc, index) = layer(cx, index)?;
        let layer = &doc.layers[index];
        if layer.kind != 0 && !(mode == 2 && layer.kind == 2) {
            return Err(NativeError::Message("layer is not normal type"));
        }
        let (mut bounds, default_mask) = if mode == 2 {
            layer.mask_bounds()
        } else {
            (layer.bounds, 0)
        };
        if mode == 2 && (bounds.width() == 0 || bounds.height() == 0) {
            bounds = psd::Bounds {
                right: 1,
                bottom: 1,
                ..Default::default()
            };
        }
        if bounds.width() <= 0 || bounds.height() <= 0 {
            return Ok(NativeStep::Return(Value::Void));
        }
        let (width, height) = (bounds.width(), bounds.height());
        for (key, n) in [
            ("left", i64::from(bounds.left)),
            ("top", i64::from(bounds.top)),
            (
                "opacity",
                if mode == 2 {
                    255
                } else {
                    i64::from(layer.opacity)
                },
            ),
            (
                "fill_opacity",
                if mode == 2 {
                    255
                } else {
                    i64::from(layer.fill_opacity)
                },
            ),
            ("width", width),
            ("height", height),
            (
                "type",
                i64::from(blend_type(if mode == 2 { 0 } else { layer.blend })),
            ),
            ("visible", i64::from(layer.visible())),
        ] {
            props.push_back((key, Value::Int(n)));
        }
        for (key, n) in [
            ("imageLeft", 0),
            ("imageTop", 0),
            ("imageWidth", width),
            ("imageHeight", height),
        ] {
            props.push_back((key, Value::Int(n)));
        }
        props.push_back((
            "name",
            Value::Str(cx.heap_mut().alloc_string(layer.name.clone())),
        ));
        if mode == 2 {
            props.push_back(("defaultMaskColor", Value::Int(i64::from(default_mask))));
        }
        (
            doc,
            match mode {
                0 => Image::Layer(index),
                1 => Image::Raw(index),
                _ => Image::Mask(index),
            },
        )
    } else {
        let doc = state(cx, owner, |s| s.loaded.as_ref().map(|d| d.doc.clone()))?;
        let Some(doc) = doc.filter(|d| d.has_merged()) else {
            return Ok(NativeStep::Return(Value::Int(0)));
        };
        let (width, height) = (i64::from(doc.size.width), i64::from(doc.size.height));
        for (key, n) in [
            ("width", width),
            ("height", height),
            ("imageLeft", 0),
            ("imageTop", 0),
            ("imageWidth", width),
            ("imageHeight", height),
        ] {
            props.push_back((key, Value::Int(n)));
        }
        (doc, Image::Merged)
    };
    let decoder = Decoder::new(doc, image, budget).map_err(error)?;
    let generation = state(cx, owner, |s| bindings::generation(s))?;
    Ok(flow::set_properties(
        Transfer {
            target,
            owner,
            generation,
            decoder: Some(decoder),
            result: if mode == 3 {
                Value::Int(1)
            } else {
                Value::Void
            },
        },
        target,
        props,
        MemberFlags {
            ensure: true,
            ..Default::default()
        },
        Transfer::check,
        |state, _| Ok(flow::work(state, Transfer::poll)),
    ))
}
impl Transfer {
    fn check(&self, cx: &mut NativeCx<'_>) -> NativeResult<()> {
        if state(cx, self.owner, |s| bindings::generation(s))? != self.generation {
            return Err(NativeError::Message(
                "PSD source changed during image transfer",
            ));
        }
        Ok(())
    }
    fn poll(&mut self, cx: &mut NativeCx<'_>) -> NativeResult<Option<NativeStep>> {
        self.check(cx)?;
        if let Some(decoder) = &mut self.decoder {
            if !decoder.advance().map_err(error)? {
                return Ok(None);
            }
            let pixels = self.decoder.take().unwrap().finish();
            let upload = extensions::layer_write_pixels(cx, self.target, pixels)?;
            return Ok(Some(flow::returning(upload, self.result)));
        }
        Ok(Some(NativeStep::Return(self.result)))
    }
}
