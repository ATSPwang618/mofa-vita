//! getSample.dll: source PCM average, peak power and lazy per-instance options.
//! Managed sample-buffer tokens replace the reference's temporary short pointers.
use krkr_engine::extensions::{self, SampleBuffer};
use tjs_core::{
    NativeCallable, NativeContinuation, NativeCx, NativeError, NativeProperty, NativeResult,
    NativeStep, ObjRef, Value, value,
};
krkr_engine::native_plugin! {
    pub(crate) Sample {
        names: ["getSample.dll", "getSample.tpm"],
        link(cx, exports) {
            let wave = crate::exports::class(cx, "WaveSoundBuffer")?;
            cx.heap.initialize_native_default::<Defaults>(wave)?;
            for (name, call) in [("getSample", legacy::CALL), ("setDefaultCounts", default_counts::CALL), ("setDefaultAheads", default_aheads::CALL)] {
                exports.function(cx, wave, name, call)?;
            }
            for p in PROPERTIES { exports.property(cx, wave, p)?; }
            Ok(())
        }
    }
}
#[derive(tjs_bind::Trace, Clone, Copy)]
struct Defaults {
    count: i32,
    ahead: i32,
}
impl Default for Defaults {
    fn default() -> Self {
        Self {
            count: 100,
            ahead: 0,
        }
    }
}
#[derive(Default, tjs_bind::Trace)]
struct Instance {
    options: Option<Defaults>,
    #[trace(skip = "Owned PCM storage contains no VM values")]
    buffer: Option<SampleBuffer>,
}
fn defaults(
    cx: &mut NativeCx<'_>,
    count: Option<i32>,
    ahead: Option<i32>,
) -> NativeResult<Defaults> {
    let class = cx
        .heap()
        .registered_class("WaveSoundBuffer")
        .ok_or(NativeError::This)?;
    cx.heap_mut().with_native_state::<Defaults, _>(class, |s| {
        if let Some(n) = count {
            s.count = n;
        }
        if let Some(n) = ahead {
            s.ahead = n;
        }
        *s
    })
}
fn checked_count(n: i32) -> NativeResult<i32> {
    if !(0..=1_048_576).contains(&n) {
        return Err(NativeError::Message("sample count exceeds buffer limit"));
    }
    Ok(n)
}
#[tjs_bind::function]
fn default_counts(cx: &mut NativeCx<'_>, count: i64) -> NativeResult<()> {
    defaults(cx, Some(checked_count(count as i32)?), None)?;
    Ok(())
}
#[tjs_bind::function]
fn default_aheads(cx: &mut NativeCx<'_>, ahead: i64) -> NativeResult<()> {
    defaults(cx, None, Some(ahead as i32))?;
    Ok(())
}
#[derive(Clone, Copy, tjs_bind::Trace)]
enum Action {
    Value,
    Count,
    Ahead,
    SetCount,
    SetAhead,
}
#[derive(tjs_bind::Trace)]
struct Access {
    owner: Value,
    action: Action,
    input: Value,
}
fn access(cx: &mut NativeCx<'_>, args: &[Value], action: Action) -> NativeResult<NativeStep> {
    extensions::validate_sound(cx)?;
    let owner = cx.this();
    cx.heap_mut().initialize_native_default::<Instance>(owner)?;
    let initialized = cx
        .heap_mut()
        .with_native_state::<Instance, _>(owner, |s| s.options.is_some())?;
    let next = Box::new(Access {
        owner: Value::Obj(ObjRef::bound(owner)),
        action,
        input: args.first().copied().unwrap_or(Value::Void),
    });
    if !initialized {
        let options = defaults(cx, None, None)?;
        let buffer = extensions::sample_buffer(cx, options.count as usize)?;
        cx.heap_mut().with_native_state::<Instance, _>(owner, |s| {
            s.options = Some(options);
            s.buffer = Some(buffer);
        })?;
        let key = key(cx, "useVisBuffer");
        return Ok(NativeStep::SetProperty {
            object: next.owner,
            key,
            value: Value::Int(1),
            flags: Default::default(),
            continuation: next,
        });
    }
    next.resume(cx, Value::Void)
}
impl NativeContinuation for Access {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let owner = crate::exports::object(self.owner)?;
        let input = if matches!(self.action, Action::SetCount | Action::SetAhead) {
            Some(value::to_integer(cx.heap(), self.input)? as i32)
        } else {
            None
        };
        if matches!(self.action, Action::SetCount) {
            checked_count(input.unwrap())?;
        }
        let replacement = if matches!(self.action, Action::SetCount) {
            Some(extensions::sample_buffer(cx, input.unwrap() as usize)?)
        } else {
            None
        };
        let options = cx.heap_mut().with_native_state::<Instance, _>(owner, |s| {
            if let Some(buffer) = replacement {
                s.buffer = Some(buffer);
            }
            let options = s.options.as_mut().unwrap();
            match self.action {
                Action::SetCount => options.count = input.unwrap(),
                Action::SetAhead => options.ahead = input.unwrap(),
                _ => {}
            }
            *options
        })?;
        Ok(match self.action {
            Action::Count => NativeStep::Return(Value::Int(i64::from(options.count))),
            Action::Ahead => NativeStep::Return(Value::Int(i64::from(options.ahead))),
            Action::SetCount | Action::SetAhead => NativeStep::Return(Value::Void),
            Action::Value => return read(cx, self.owner, options, false),
        })
    }
}
macro_rules! accessor {
    ($name:ident, $action:ident) => {
        fn $name(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
            access(cx, args, Action::$action)
        }
    };
}
accessor!(sample_value, Value);
accessor!(count, Count);
accessor!(ahead, Ahead);
accessor!(set_count, SetCount);
accessor!(set_ahead, SetAhead);
static PROPERTIES: &[NativeProperty] = &[
    NativeProperty {
        name: "sampleValue",
        doc: "Squared maximum source PCM amplitude",
        hidden: false,
        class_only: false,
        get: Some(NativeCallable::Resumable(sample_value)),
        set: None,
    },
    NativeProperty {
        name: "sampleCount",
        doc: "Number of source samples",
        hidden: false,
        class_only: false,
        get: Some(NativeCallable::Resumable(count)),
        set: Some(NativeCallable::Resumable(set_count)),
    },
    NativeProperty {
        name: "sampleAhead",
        doc: "Source sample offset from audible position",
        hidden: false,
        class_only: false,
        get: Some(NativeCallable::Resumable(ahead)),
        set: Some(NativeCallable::Resumable(set_ahead)),
    },
];
fn key(cx: &mut NativeCx<'_>, name: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
    )
}
#[tjs_bind::function(resumable = true)]
fn legacy(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<NativeStep> {
    let count = args
        .first()
        .map(|v| value::to_integer(cx.heap(), *v))
        .transpose()?
        .unwrap_or(100) as i32;
    if count <= 0 || !cx.result_needed() {
        return Ok(NativeStep::Return(Value::Void));
    }
    checked_count(count)?;
    read(
        cx,
        Value::Obj(ObjRef::bound(cx.this())),
        Defaults { count, ahead: 0 },
        true,
    )
}
#[derive(tjs_bind::Trace)]
struct Read {
    #[trace(skip = "Scoped PCM buffer contains no VM values")]
    buffer: SampleBuffer,
    legacy: bool,
}
fn read(
    cx: &mut NativeCx<'_>,
    owner: Value,
    options: Defaults,
    legacy: bool,
) -> NativeResult<NativeStep> {
    let buffer = if legacy {
        extensions::sample_buffer(cx, options.count as usize)?
    } else {
        let id = crate::exports::object(owner)?;
        cx.heap_mut()
            .with_native_state::<Instance, _>(id, |s| s.buffer.as_ref().unwrap().clone())?
    };
    buffer.clear();
    let mut arguments = vec![
        buffer.token(),
        Value::Int(i64::from(options.count)),
        Value::Int(1),
    ];
    if !legacy {
        arguments.push(Value::Int(i64::from(options.ahead)));
    }
    let key = key(cx, "getVisBuffer");
    Ok(NativeStep::CallMember {
        object: owner,
        key,
        arguments,
        continuation: Box::new(Read { buffer, legacy }),
    })
}
impl NativeContinuation for Read {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, result: Value) -> NativeResult<NativeStep> {
        let result = if self.legacy {
            Value::Int(self.buffer.read(|samples| {
                let mut count = 0i64;
                let mut sum = 0i64;
                for &n in samples {
                    if n >= 0 {
                        count += 1;
                        sum += i64::from(n);
                    }
                }
                if count == 0 { 0 } else { sum / count }
            }))
        } else {
            let n = value::to_integer(cx.heap(), result)? as i32;
            Value::Real(self.buffer.read(|samples| {
                let count = if n < 0 || n as usize > samples.len() {
                    samples.len()
                } else {
                    n as usize
                };
                let peak = samples[..count]
                    .iter()
                    .map(|&s| i32::from(s).abs())
                    .max()
                    .unwrap_or(0) as f64
                    / 32768.;
                peak * peak
            }))
        };
        Ok(NativeStep::Return(result))
    }
}
