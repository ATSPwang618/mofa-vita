use crate::exports::{Exports, arg, class};
use krkr_engine::{
    extensions,
    plugins::Context,
    protocol::window::desktop::{Clip, Command, MonitorTarget},
};
use tjs_core::{
    NativeCallable, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, Trace,
    Value, value,
};

pub(super) fn install(cx: &mut Context<'_>, exports: &mut Exports) -> NativeResult<()> {
    let system = class(cx, "System")?;
    for (name, call) in [
        (
            "getDisplayMonitors",
            monitors as fn(&mut NativeCx<'_>, &[Value]) -> NativeResult<NativeStep>,
        ),
        ("getMonitorInfo", monitor),
        ("getCursorPos", cursor),
        ("setCursorPos", set_cursor),
        ("setClipCursor", clip),
        ("getSystemMetrics", metric),
        ("getDoubleClickTime", double_click),
        ("setIconicPreview", iconic),
        ("mapVirtualKey", map_key),
        ("breathe", breathe),
    ] {
        exports.function(cx, system, name, NativeCallable::Resumable(call))?;
    }
    for (name, call) in [
        (
            "isBreathing",
            breathing as fn(&mut NativeCx<'_>, &[Value]) -> NativeResult<Value>,
        ),
        ("clearGraphicCache", clear_cache),
        ("getAboutString", about),
        ("getCPUType", cpu),
    ] {
        exports.function(cx, system, name, NativeCallable::Leaf(call))?;
    }
    Ok(())
}
fn number(cx: &NativeCx<'_>, args: &[Value], index: usize) -> NativeResult<i32> {
    Ok(value::to_integer(cx.heap(), arg(args, index)?)? as i32)
}
fn rect(cx: &NativeCx<'_>, args: &[Value]) -> NativeResult<(i32, i32, i32, i32)> {
    Ok((
        number(cx, args, 0)?,
        number(cx, args, 1)?,
        number(cx, args, 2)?,
        number(cx, args, 3)?,
    ))
}
fn monitors(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let filter = match args.len() {
        0 => None,
        4 => Some(rect(cx, args)?),
        _ => {
            return Err(NativeError::Message(
                "getDisplayMonitors requires zero or four arguments",
            ));
        }
    };
    extensions::desktop_request(cx, Command::Monitors(filter))
}
fn monitor(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let nearest = args
        .first()
        .map(|&v| value::to_integer(cx.heap(), v))
        .transpose()?
        .unwrap_or(0)
        != 0;
    let target = match args.len() {
        0 => MonitorTarget::Primary,
        2 => {
            let Value::Obj(reference) = args[1] else {
                return Err(NativeError::Type("a non-null object"));
            };
            let Some(object) = reference.object else {
                return Err(NativeError::Type("a non-null object"));
            };
            if cx
                .heap()
                .class_names(object)?
                .iter()
                .any(|name| name.iter().copied().eq("Window".encode_utf16()))
            {
                MonitorTarget::Window(extensions::window_id(cx, args[1])?)
            } else {
                // The original windowEx treats TJS_S_FALSE (2) as truthy and
                // queries a null HWND for ordinary script helper objects.
                // MonitorFromWindow then selects the primary monitor only
                // when the caller requested the nearest monitor. KAG window
                // helpers rely on this even though they are not Windows.
                if !nearest {
                    return Ok(NativeStep::Return(Value::Void));
                }
                MonitorTarget::Primary
            }
        }
        3 => MonitorTarget::Point(number(cx, args, 1)?, number(cx, args, 2)?),
        5 => {
            let (x, y, w, h) = rect(cx, &args[1..])?;
            MonitorTarget::Rect(x, y, w, h)
        }
        _ => {
            return Err(NativeError::Message(
                "getMonitorInfo requires zero, two, three or five arguments",
            ));
        }
    };
    extensions::desktop_request(cx, Command::Monitor { nearest, target })
}
fn cursor(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<NativeStep> {
    extensions::desktop_request(cx, Command::Cursor)
}
fn set_cursor(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let command = Command::SetCursor(number(cx, args, 0)?, number(cx, args, 1)?);
    extensions::desktop_request(cx, command)
}
fn clip(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let Some(&value) = args.first().filter(|v| !matches!(v, Value::Void)) else {
        return extensions::desktop_request(cx, Command::Clip(Clip::Release));
    };
    let Value::Obj(reference) = value else {
        return Err(NativeError::Type("a Window or rectangle object"));
    };
    let Some(id) = reference.object else {
        return Ok(NativeStep::Return(Value::Void));
    };
    if cx
        .heap()
        .class_names(id)?
        .iter()
        .any(|name| name.iter().copied().eq("Window".encode_utf16()))
    {
        let window = extensions::window_id(cx, value)?;
        return extensions::desktop_request(cx, Command::Clip(Clip::Window(window)));
    }
    let task = RectClip {
        object: value,
        index: 0,
        values: [0; 4],
    };
    task.next(cx)
}
struct RectClip {
    object: Value,
    index: usize,
    values: [i32; 4],
}
impl Trace for RectClip {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.object.trace(visit);
    }
}
impl RectClip {
    fn next(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.index == 4 {
            let [x, y, w, h] = self.values;
            return extensions::desktop_request(cx, Command::Clip(Clip::Rect(x, y, w, h)));
        }
        let key = Value::Str(
            cx.heap_mut().alloc_string(
                ["x", "y", "w", "h"][self.index]
                    .encode_utf16()
                    .collect::<Vec<_>>(),
            ),
        );
        Ok(NativeStep::GetOr {
            object: self.object,
            key,
            raw: false,
            fallback: Value::Int(0),
            continuation: Box::new(self),
        })
    }
}
impl NativeContinuation for RectClip {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        self.values[self.index] = value::to_integer(cx.heap(), value)? as i32;
        self.index += 1;
        self.next(cx)
    }
}
fn double_click(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<NativeStep> {
    extensions::desktop_request(cx, Command::DoubleClickTime)
}
fn iconic(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let enabled = number(cx, args, 0)? != 0;
    extensions::desktop_request(cx, Command::IconicPreview(enabled))
}
fn map_key(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let command = Command::MapKey {
        code: number(cx, args, 0)? as u32,
        mapping: number(cx, args, 1)? as u32,
    };
    extensions::desktop_request(cx, command)
}
fn breathe(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<NativeStep> {
    extensions::breathe(cx)
}
fn breathing(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    extensions::is_breathing(cx)
}
fn clear_cache(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    extensions::clear_graphic_cache(cx)
}
pub(crate) fn about(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    Ok(Value::Str(
        cx.heap_mut().alloc_string(
            concat!(
                "krkr-rs ",
                env!("CARGO_PKG_VERSION"),
                "\nCross-platform Kirikiri/TJS engine"
            )
            .encode_utf16()
            .collect::<Vec<_>>(),
        ),
    ))
}
fn cpu(_: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    // Original bit positions; unknown vendor/family remain unknown. Non-x86
    // architectures do not claim the old x86 native-code capability flags.
    #[allow(unused_mut)]
    let mut flags = 0u32;
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        macro_rules! feature {
            ($name:tt, $mask:expr) => {
                if std::is_x86_feature_detected!($name) {
                    flags |= $mask;
                }
            };
        }
        feature!("mmx", 0x00020000);
        feature!("sse", 0x00080000);
        feature!("sse2", 0x00800000);
        feature!("sse3", 0x02000000);
        feature!("ssse3", 0x04000000);
        feature!("sse4.1", 0x08000000);
        feature!("sse4.2", 0x10000000);
        feature!("sse4a", 0x20000000);
        feature!("avx", 0x40000000);
        feature!("avx2", 0x80000000);
        feature!("fma", 0x00001000);
        feature!("aes", 0x00002000);
        feature!("rdrand", 0x00008000);
        feature!("rdseed", 0x00000100);
        #[cfg(target_arch = "x86_64")]
        {
            flags |= 0x00010000 | 0x00100000 | 0x01000000;
        }
    }
    Ok(Value::Int(flags.into()))
}

fn metric(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let Value::Str(id) = arg(args, 0)? else {
        return Err(NativeError::Type("a nonempty metric name string"));
    };
    let text = String::from_utf16_lossy(tjs_core::string::c_string(cx.heap().string(id)?));
    if text.is_empty() {
        return Err(NativeError::Message("empty metric name"));
    }
    let key = Value::Str(
        cx.heap_mut()
            .alloc_string(text.to_uppercase().encode_utf16().collect::<Vec<_>>()),
    );
    super::lookup::lookup(cx, true, key, false)
}
