//! Metadata queries use the actual script object returned by ResourceManager,
//! preserving script edits and getters instead of rebuilding immutable PSB data.
use super::{manager, player};
use tjs_bind::{Array, IntoTjs, RestArgs, Utf16};
use tjs_core::{NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, Value, value};

pub(super) fn contains(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    if args.len() < 2 {
        return Err(NativeError::Missing(args.len()));
    }
    let labelled = args.len() >= 3 && !matches!(args[0], Value::Real(_));
    let label = if labelled {
        Some(String::from_utf16_lossy(&value::to_string_units(
            cx.heap(),
            args[0],
        )?))
    } else {
        None
    };
    let index = usize::from(labelled);
    let x = value::to_real(cx.heap(), args[index])? as f32;
    let y = value::to_real(cx.heap(), args[index + 1])? as f32;
    let (snapshot, affine) = cx.with_state::<player::bindings::State, _>(|s, _| {
        Ok((s.drawing.snapshot.clone(), s.transform.affine))
    })?;
    let Some(snapshot) = snapshot else {
        return Ok(NativeStep::Return(Value::Int(0)));
    };
    krkr_engine::extensions::run_work(
        cx,
        move |stop| {
            let (playback, transform) = snapshot.as_ref();
            let frame = super::scene::geometry(playback, transform, &|| {
                stop.load(std::sync::atomic::Ordering::Relaxed)
            })?;
            let [x, y] = if label.is_some() {
                let p = affine.point([x, y, 0., 1.]);
                [p[0], p[1]]
            } else {
                [x, y]
            };
            let shape = frame
                .shapes
                .iter()
                .any(|s| label.as_ref().is_none_or(|l| s.label == *l) && s.contains(x, y));
            Ok(shape
                || (label.is_none()
                    && frame.icon_bounds.iter().any(|&[left, top, width, height]| {
                        x >= left && x < left + width && y >= top && y < top + height
                    })))
        },
        Box::new(Hit),
    )
}
#[derive(tjs_bind::Trace)]
struct Hit;
impl krkr_engine::extensions::WorkContinuation<bool> for Hit {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, hit: bool) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(Value::Int(i64::from(hit))))
    }
}

pub(super) fn variable_frames(cx: &mut NativeCx<'_>, name: Utf16) -> NativeResult<NativeStep> {
    let (manager, main, count) = cx.with_state::<player::bindings::State, _>(|s, _| {
        Ok((
            s.manager,
            s.playback.main.clone(),
            s.playback
                .main
                .as_ref()
                .and_then(|n| s.playback.files.get(n))
                .map_or(0, |s| s.variables.len()),
        ))
    })?;
    let Some(main) = main else {
        return Ok(NativeStep::Return(
            Array(Vec::<Value>::new()).into_tjs(cx.heap_mut())?,
        ));
    };
    let root = manager::bindings::with_state(cx, crate::exports::object(manager)?, |s| {
        s.cache.get(&main).map_or(Value::Void, |e| e.root)
    })?;
    Box::new(Frames {
        name: name.0,
        root,
        list: Value::Void,
        item: Value::Void,
        count,
        index: 0,
        phase: 0,
    })
    .resume(cx, Value::Void)
}
#[derive(tjs_bind::Trace)]
struct Frames {
    name: Vec<u16>,
    root: Value,
    list: Value,
    item: Value,
    count: usize,
    index: usize,
    phase: u8,
}
impl Frames {
    fn get(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        object: Value,
        name: &str,
    ) -> NativeResult<NativeStep> {
        let key = Value::Str(
            cx.heap_mut()
                .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
        );
        Ok(NativeStep::GetOr {
            object,
            key,
            raw: false,
            fallback: Value::Void,
            continuation: self,
        })
    }
    fn next(mut self: Box<Self>) -> NativeResult<NativeStep> {
        if self.index >= self.count {
            return Ok(NativeStep::Return(Value::Void));
        }
        self.phase = 3;
        let key = Value::Int(self.index as i64);
        self.index += 1;
        Ok(NativeStep::GetOr {
            object: self.list,
            key,
            raw: false,
            fallback: Value::Void,
            continuation: self,
        })
    }
}
impl NativeContinuation for Frames {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<NativeStep> {
        match self.phase {
            0 => {
                self.phase = 1;
                let root = self.root;
                self.get(cx, root, "metadata")
            }
            1 => {
                if !matches!(v, Value::Obj(r) if r.object.is_some()) {
                    return Ok(NativeStep::Return(Value::Void));
                }
                self.phase = 2;
                self.get(cx, v, "variableList")
            }
            2 => {
                self.list = v;
                self.next()
            }
            3 => {
                if !matches!(v, Value::Obj(r) if r.object.is_some()) {
                    return Ok(NativeStep::Return(Value::Void));
                }
                self.item = v;
                self.phase = 4;
                self.get(cx, v, "label")
            }
            4 => {
                if let Value::Str(s) = v
                    && cx.heap().string(s)? == self.name.as_slice()
                {
                    self.phase = 5;
                    let item = self.item;
                    self.get(cx, item, "frameList")
                } else {
                    self.next()
                }
            }
            _ => Ok(NativeStep::Return(v)),
        }
    }
}
