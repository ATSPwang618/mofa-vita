//! kwidgets layerExScreen/main.cpp: property dispatch and conversion order are
//! observable. The obsolete HWND lookup is replaced by a managed host target.
use krkr_engine::extensions::{self, WindowScreenTarget};
use tjs_core::{
    NativeCallable, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId,
    ObjRef, Value, value,
};

krkr_engine::native_plugin! {
    pub(crate) Screen {
        names: ["LayerExScreen.dll", "LayerExScreen.tpm"],
        classes: [],
        extensions: [
            ("Layer", "getScreenLeft", NativeCallable::Resumable(left)),
            ("Layer", "getScreenTop", NativeCallable::Resumable(top)),
        ],
    }
}
fn left(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    start(cx, args, false)
}
fn top(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    start(cx, args, true)
}
fn start(cx: &mut NativeCx<'_>, args: &[Value], vertical: bool) -> NativeResult<NativeStep> {
    // Raw callbacks return before even inspecting their receiver or parameters
    // when the original caller supplies no result slot.
    if !cx.result_needed() {
        return Ok(NativeStep::Return(Value::Void));
    }
    let offset = match args.first() {
        Some(Value::Int(v)) => *v as i32,
        _ => 0,
    };
    let owner = cx.this();
    Box::new(Position {
        owner,
        current: owner,
        stage: Stage::Coordinate,
        vertical,
        offset,
        position: 0,
        numerator: 0,
        target: None,
    })
    .get(cx, if vertical { "top" } else { "left" }, Value::Void)
}
#[derive(Clone, Copy, tjs_bind::Trace)]
enum Stage {
    Coordinate,
    Parent,
    Target,
    ZoomWindow,
    Numerator,
    Denominator,
}
#[derive(tjs_bind::Trace)]
struct Position {
    owner: ObjId,
    current: ObjId,
    stage: Stage,
    vertical: bool,
    offset: i32,
    position: i32,
    numerator: i32,
    target: Option<WindowScreenTarget>,
}
impl Position {
    fn get(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        name: &'static str,
        previous: Value,
    ) -> NativeResult<NativeStep> {
        let object = Value::Obj(ObjRef::bound(self.current));
        let key = Value::Str(
            cx.heap_mut()
                .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
        );
        // getValue first asks IsValid(name), which observes the raw member and
        // invokes missing without executing a property getter. PropGet is a
        // second dispatch; missing can therefore execute twice for one read.
        Ok(NativeStep::GetOr {
            object,
            key,
            raw: true,
            fallback: Value::Void,
            continuation: Box::new(Validity {
                next: self,
                object,
                key,
                previous,
            }),
        })
    }
}
#[derive(tjs_bind::Trace)]
struct Validity {
    next: Box<Position>,
    object: Value,
    key: Value,
    previous: Value,
}
impl NativeContinuation for Validity {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, raw: Value) -> NativeResult<NativeStep> {
        if let Value::Obj(reference) = raw
            && let Some(object) = reference.object
            && !cx.heap().is_valid(object)?
        {
            // IsValid returned false: the shared tTJSVariant is left unchanged.
            return self.next.resume(cx, self.previous);
        }
        Ok(NativeStep::GetOr {
            object: self.object,
            key: self.key,
            raw: false,
            fallback: self.previous,
            continuation: self.next,
        })
    }
}
fn object(value: Value) -> NativeResult<Option<ObjId>> {
    match value {
        Value::Obj(reference) => Ok(reference.object),
        _ => Err(NativeError::Type("an object")),
    }
}
impl NativeContinuation for Position {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        match self.stage {
            Stage::Coordinate => {
                self.position = self
                    .position
                    .wrapping_add(value::to_integer(cx.heap(), result)? as i32);
                self.stage = Stage::Parent;
                self.get(cx, "parent", result)
            }
            Stage::Parent => {
                if let Some(parent) = object(result)? {
                    self.current = parent;
                    self.stage = Stage::Coordinate;
                    let name = if self.vertical { "top" } else { "left" };
                    self.get(cx, name, result)
                } else {
                    self.current = self.owner;
                    self.stage = Stage::Target;
                    self.get(cx, "window", Value::Void)
                }
            }
            Stage::Target => {
                self.target = Some(extensions::window_screen_target(cx, result)?);
                self.position = self.position.wrapping_add(self.offset);
                self.stage = Stage::ZoomWindow;
                self.get(cx, "window", Value::Void)
            }
            Stage::ZoomWindow => {
                self.current =
                    object(result)?.ok_or(NativeError::Type("a non-null zoom object"))?;
                self.stage = Stage::Numerator;
                self.get(cx, "zoomNumer", result)
            }
            Stage::Numerator => {
                self.numerator = value::to_integer(cx.heap(), result)? as i32;
                self.stage = Stage::Denominator;
                self.get(cx, "zoomDenom", result)
            }
            Stage::Denominator => {
                let denominator = value::to_integer(cx.heap(), result)? as i32;
                let scaled = (f64::from(self.position)
                    * (f64::from(self.numerator) / f64::from(denominator)))
                .trunc();
                // C++ float-to-int is undefined outside this range. Keep the
                // normal truncation behavior and report invalid custom zooms.
                if !scaled.is_finite()
                    || scaled < f64::from(i32::MIN)
                    || scaled > f64::from(i32::MAX)
                {
                    return Err(NativeError::Message(
                        "screen coordinate is outside the signed 32-bit range",
                    ));
                }
                self.target
                    .take()
                    .expect("captured window target")
                    .coordinate(cx, scaled as i32, self.vertical)
            }
        }
    }
}
