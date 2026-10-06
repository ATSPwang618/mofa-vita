//! Safe owned counterparts to the reference's temporary motion pointers.
//! A getter retains its evaluated geometry even when the player advances.
use super::{player, render::Frame};
use std::sync::Arc;
use tjs_bind::{Dictionary, IntoTjs, RestArgs, Utf16, flow};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, ObjId, Value, value};

fn same(a: &str, b: &str) -> bool {
    a.split('\0').next() == b.split('\0').next()
}
fn find_motion(frame: &Frame, index: usize, name: &str) -> Option<usize> {
    let motion = frame.motions.get(index)?;
    if let Some((_, index)) = motion.children.iter().find(|(label, _)| same(label, name)) {
        return Some(*index);
    }
    motion
        .children
        .iter()
        .find_map(|(_, i)| find_motion(frame, *i, name))
}
fn find_shape(frame: &Frame, index: usize, name: &str) -> Option<usize> {
    let motion = frame.motions.get(index)?;
    if let Some(&index) = motion
        .shapes
        .iter()
        .find(|&&i| same(&frame.shapes[i].label, name))
    {
        return Some(index);
    }
    motion
        .children
        .iter()
        .find_map(|(_, i)| find_shape(frame, *i, name))
}
fn object(cx: &mut NativeCx<'_>, state: bindings::State) -> NativeResult<Value> {
    let class = bindings::install(cx.heap_mut())?;
    Ok(Value::Obj(cx.heap_mut().alloc_native(class, state)?.into()))
}
fn lookup(
    cx: &mut NativeCx<'_>,
    owner: Value,
    frame: Arc<Frame>,
    index: usize,
    name: &str,
    motion_only: bool,
    root: bool,
) -> NativeResult<Value> {
    let mut result = Vec::new();
    if let Some(index) = find_motion(&frame, index, name) {
        let v = object(
            cx,
            bindings::State {
                owner,
                frame: Some(frame),
                index: Some(index),
                ..Default::default()
            },
        )?;
        if motion_only {
            return Ok(v);
        }
        result.push(("motion", v));
        if !root {
            result.push(("shape", v));
        }
    } else if motion_only {
        return Ok(Value::Void);
    } else if let Some(index) = find_shape(&frame, index, name) {
        let area = &frame.shapes[index];
        let v = object(
            cx,
            bindings::State {
                bounds: area.bounds.map(f64::from),
                kind: i64::from(area.kind),
                ..Default::default()
            },
        )?;
        result.push(("shape", v));
    }
    Dictionary(result).into_tjs(cx.heap_mut())
}
pub(super) fn get(
    cx: &mut NativeCx<'_>,
    name: Utf16,
    motion_only: bool,
) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let snapshot =
        cx.with_state::<player::bindings::State, _>(|s, _| Ok(s.drawing.snapshot.clone()))?;
    let Some(snapshot) = snapshot else {
        return Ok(NativeStep::Return(if motion_only {
            Value::Void
        } else {
            Dictionary(Vec::<(&str, Value)>::new()).into_tjs(cx.heap_mut())?
        }));
    };
    krkr_engine::extensions::run_work(
        cx,
        move |stop| {
            let (playback, transform) = snapshot.as_ref();
            super::scene::geometry(playback, transform, &|| {
                stop.load(std::sync::atomic::Ordering::Relaxed)
            })
        },
        Box::new(Ready {
            owner,
            name: name.0,
            motion_only,
        }),
    )
}
#[derive(tjs_bind::Trace)]
struct Ready {
    owner: ObjId,
    name: Vec<u16>,
    motion_only: bool,
}
impl krkr_engine::extensions::WorkContinuation<Frame> for Ready {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, frame: Frame) -> NativeResult<NativeStep> {
        lookup(
            cx,
            Value::Obj(self.owner.into()),
            Arc::new(frame),
            0,
            &String::from_utf16_lossy(&self.name),
            self.motion_only,
            true,
        )
        .map(NativeStep::Return)
    }
}
#[tjs_bind::class(name = "Motion.MotionObj")]
mod bindings {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        pub owner: Value,
        #[trace(skip = "Owned evaluated geometry")]
        pub frame: Option<Arc<Frame>>,
        pub index: Option<usize>,
        pub bounds: [f64; 4],
        pub kind: i64,
    }
    impl State {
        #[tjs::constructor]
        fn new() -> NativeResult<Self> {
            Err(NativeError::Message(
                "MotionObj is returned by getLayerGetter",
            ))
        }
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.frame = None;
            self.owner = Value::Void;
            self.index = None;
        }
        #[tjs::method(name = "getLayerGetter")]
        fn getter(cx: &mut NativeCx<'_>, name: Utf16) -> NativeResult<Value> {
            Self::lookup(cx, name, false)
        }
        #[tjs::method(name = "getLayerMotion")]
        fn motion(cx: &mut NativeCx<'_>, name: Utf16) -> NativeResult<Value> {
            Self::lookup(cx, name, true)
        }
        fn lookup(cx: &mut NativeCx<'_>, name: Utf16, motion_only: bool) -> NativeResult<Value> {
            let (owner, frame, index) =
                cx.with_state::<Self, _>(|s, _| Ok((s.owner, s.frame.clone(), s.index)))?;
            if let (Some(frame), Some(index)) = (frame, index) {
                super::lookup(
                    cx,
                    owner,
                    frame,
                    index,
                    &String::from_utf16_lossy(&name.0),
                    motion_only,
                    false,
                )
            } else if motion_only {
                Ok(Value::Void)
            } else {
                Dictionary(Vec::<(&str, Value)>::new()).into_tjs(cx.heap_mut())
            }
        }
        #[tjs::method(name = "setVariable", resumable = true)]
        fn variable(cx: &mut NativeCx<'_>, name: Utf16, value: f64) -> NativeResult<NativeStep> {
            let owner = cx.with_state::<Self, _>(|s, _| Ok(s.owner))?;
            if matches!(owner, Value::Void) {
                return Ok(NativeStep::Return(Value::Void));
            }
            Ok(NativeStep::CallMember {
                object: owner,
                key: "setVariable".to_owned().into_tjs(cx.heap_mut())?,
                arguments: vec![name.into_tjs(cx.heap_mut())?, Value::Real(value)],
                continuation: flow::callback((), |_, _, v| Ok(NativeStep::Return(v))),
            })
        }
        #[tjs::method(resumable = true)]
        fn contains(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            if args.len() < 2 {
                return Err(NativeError::Missing(args.len()));
            }
            let (owner, frame, index, bounds, kind) = cx.with_state::<Self, _>(|s, _| {
                Ok((s.owner, s.frame.clone(), s.index, s.bounds, s.kind))
            })?;
            if args.len() >= 3 && !matches!(args[0], Value::Real(_)) {
                if matches!(owner, Value::Void) {
                    return Ok(NativeStep::Return(Value::Void));
                }
                return Ok(NativeStep::CallMember {
                    object: owner,
                    key: "contains".to_owned().into_tjs(cx.heap_mut())?,
                    arguments: args.to_vec(),
                    continuation: flow::callback((), |_, _, v| Ok(NativeStep::Return(v))),
                });
            }
            let x = value::to_real(cx.heap(), args[0])?;
            let y = value::to_real(cx.heap(), args[1])?;
            let hit = if let (Some(frame), Some(index)) = (frame, index) {
                frame.motions.get(index).is_some_and(|m| {
                    frame.icon_bounds[m.icons.clone()]
                        .iter()
                        .any(|&[l, t, w, h]| {
                            x >= l as f64
                                && x < (l + w) as f64
                                && y >= t as f64
                                && y < (t + h) as f64
                        })
                })
            } else {
                let [l, t, w, h] = bounds;
                match kind {
                    0 => (x - l).powi(2) + (y - t).powi(2) <= 1.,
                    1 => (x - l - w / 2.).powi(2) + (y - t - h / 2.).powi(2) <= (w / 2.).powi(2),
                    _ => x >= l && x <= l + w && y >= t && y <= t + h,
                }
            };
            Ok(NativeStep::Return(Value::Int(i64::from(hit))))
        }
        #[tjs::getter(name = "l")]
        fn left(&self) -> f64 {
            self.bounds[0]
        }
        #[tjs::setter(name = "l")]
        fn set_left(&mut self, v: f64) {
            self.bounds[0] = v;
        }
        #[tjs::getter(name = "t")]
        fn top(&self) -> f64 {
            self.bounds[1]
        }
        #[tjs::setter(name = "t")]
        fn set_top(&mut self, v: f64) {
            self.bounds[1] = v;
        }
        #[tjs::getter(name = "w")]
        fn width(&self) -> f64 {
            self.bounds[2]
        }
        #[tjs::setter(name = "w")]
        fn set_width(&mut self, v: f64) {
            self.bounds[2] = v;
        }
        #[tjs::getter(name = "h")]
        fn height(&self) -> f64 {
            self.bounds[3]
        }
        #[tjs::setter(name = "h")]
        fn set_height(&mut self, v: f64) {
            self.bounds[3] = v;
        }
        #[tjs::getter(name = "shapeType")]
        fn shape_type(&self) -> i64 {
            self.kind
        }
        #[tjs::setter(name = "shapeType")]
        fn set_shape_type(&mut self, #[tjs(coerce)] v: i64) {
            self.kind = v;
        }
    }
}
