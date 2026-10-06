//! Altered Rust adaptation of layerExImage / CxImage 7.0.2. See image/LICENSE.txt.
mod algorithms;
use krkr_engine::{
    extensions,
    protocol::{
        filter::{Filter, Kind},
        graphics::{Adjustment, Rect},
    },
};
use std::{cell::Cell, rc::Rc};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, ObjRef, Value,
    value,
};

#[derive(Clone, tjs_bind::Trace)]
struct Random(
    #[trace(skip = "CRT-compatible integer random state contains no VM handles")] Rc<Cell<u32>>,
);
#[derive(Clone, Default, tjs_bind::Trace)]
struct Cached(Option<[Value; 7]>);
const PROPERTIES: [&str; 7] = [
    "imageWidth",
    "imageHeight",
    "clipLeft",
    "clipTop",
    "clipWidth",
    "clipHeight",
    "update",
];

krkr_engine::native_plugin! {
    pub(crate) Image {
        names: ["layerExImage.dll", "layerExImage.tpm"],
        link(cx, exports) {
            let layer = crate::exports::class(cx, "Layer")?;
            let random = Random(Rc::new(Cell::new(1)));
            for (name, call) in [
                ("light", light::CALL), ("colorize", colorize::CALL),
                ("modulate", modulate::CALL), ("noise", noise::CALL),
                ("generateWhiteNoise", white::CALL), ("gaussianBlur", blur::CALL),
            ] {
                exports.captured_function(cx, layer, name, call, random.clone(), false)?;
            }
            Ok(())
        }
    }
}
// RestArgs preserves ncbind's argument count check before reset, and numeric
// conversion after reset. Every effect writes even when its result is ignored.
macro_rules! entry {
    ($name:ident, $kind:ident, $count:expr) => {
        #[tjs_bind::function(resumable = true)]
        fn $name(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<NativeStep> {
            start(cx, args, Operation::$kind, $count)
        }
    };
}
entry!(light, Light, 2);
entry!(colorize, Colorize, 3);
entry!(modulate, Modulate, 3);
entry!(noise, Noise, 1);
entry!(white, White, 0);
entry!(blur, Blur, 1);
#[derive(Clone, Copy, tjs_bind::Trace)]
enum Operation {
    Light,
    Colorize,
    Modulate,
    Noise,
    White,
    Blur,
}
#[derive(tjs_bind::Trace)]
struct Work {
    owner: ObjId,
    class: ObjId,
    random: Random,
    operation: Operation,
    args: Vec<Value>,
    cache: [Value; 7],
    index: usize,
    capturing: bool,
    redraw: bool,
    values: [i32; 6],
    redraw_args: [Value; 4],
}
fn start(
    cx: &mut NativeCx<'_>,
    args: &[Value],
    operation: Operation,
    count: usize,
) -> NativeResult<NativeStep> {
    if args.len() < count {
        return Err(NativeError::Missing(count - 1));
    }
    let owner = cx.this();
    extensions::layer_size(cx, Value::Obj(owner.into()))?;
    cx.heap_mut().initialize_native_default::<Cached>(owner)?;
    let cached = cx
        .heap_mut()
        .with_native_state::<Cached, _>(owner, |s| s.0)?;
    let function = cx.function().ok_or(NativeError::This)?;
    let random = cx
        .heap_mut()
        .with_native_state::<Random, _>(function, |s| s.clone())?;
    let class = cx
        .heap()
        .registered_class("Layer")
        .ok_or(NativeError::This)?;
    Box::new(Work {
        owner,
        class,
        random,
        operation,
        args: args[..count].to_vec(),
        cache: cached.unwrap_or([Value::Void; 7]),
        index: 0,
        capturing: cached.is_none(),
        redraw: false,
        values: [0; 6],
        redraw_args: [Value::Void; 4],
    })
    .next(cx)
}
fn bound(value: Value, owner: ObjId) -> NativeResult<Value> {
    let Value::Obj(mut reference) = value else {
        return Err(NativeError::Type(
            "a cached Layer property or method object",
        ));
    };
    if reference.object.is_none() {
        return Err(NativeError::This);
    }
    reference.this = Some(owner);
    Ok(Value::Obj(reference))
}
impl Work {
    fn next(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.capturing {
            if self.index < self.cache.len() {
                let key = Value::Str(
                    cx.heap_mut().alloc_string(
                        PROPERTIES[[0, 1, 6, 2, 3, 4, 5][self.index]]
                            .encode_utf16()
                            .collect::<Vec<_>>(),
                    ),
                );
                return Ok(NativeStep::GetOr {
                    object: Value::Obj(ObjRef::bound(self.class)),
                    key,
                    raw: true,
                    fallback: Value::Void,
                    continuation: self,
                });
            }
            cx.heap_mut()
                .with_native_state::<Cached, _>(self.owner, |s| s.0 = Some(self.cache))?;
            self.capturing = false;
            self.index = 0;
        }
        if !self.redraw && self.index == 2 {
            // Replaces only mainImageBufferForWrite/pitch; never exposes pointers.
            extensions::layer_prepare_draw(cx, Value::Obj(self.owner.into()))?;
        }
        if self.index < 6 {
            return Ok(NativeStep::Get {
                object: bound(self.cache[self.index], self.owner)?,
                key: Value::Void,
                continuation: self,
            });
        }
        if self.redraw {
            return Ok(NativeStep::Call {
                function: bound(self.cache[6], self.owner)?,
                arguments: self.redraw_args.to_vec(),
                continuation: tjs_bind::flow::complete(Value::Void),
            });
        }
        let rect = Rect {
            left: self.values[2],
            top: self.values[3],
            width: self.values[4].max(0) as u32,
            height: self.values[5].max(0) as u32,
        };
        let budget = extensions::layer_pixel_budget(cx, Value::Obj(self.owner.into()))?;
        let filter = algorithms::make(
            cx,
            self.operation,
            &self.args,
            rect,
            &self.random.0,
            &budget,
        )?;
        let step = extensions::layer_adjust(
            cx,
            Value::Obj(self.owner.into()),
            rect,
            Adjustment::Filter(filter),
            false,
        )?;
        self.redraw = true;
        self.index = 2;
        Ok(tjs_bind::flow::then(
            step,
            tjs_bind::flow::callback(self, |work, cx, _| work.next(cx)),
        ))
    }
}
impl NativeContinuation for Work {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        if self.capturing {
            bound(result, self.owner)?;
            self.cache[[0, 1, 6, 2, 3, 4, 5][self.index]] = result;
        } else if self.redraw {
            self.redraw_args[self.index - 2] = result;
        } else {
            self.values[self.index] = value::to_integer(cx.heap(), result)? as i32;
        }
        self.index += 1;
        self.next(cx)
    }
}
