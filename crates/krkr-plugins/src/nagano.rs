//! All twelve extNagano providers, using portable GPU visual approximations.
//! Reference: wamsoft/extNagano@4b403e2; selected option/sample behavior was
//! checked against the supplied x86 DLL. See NOTICE.txt and the compatibility
//! guide for differences. Frame pixels stay on the render host.
mod advanced;
mod morph;
mod simple;
mod spin;
mod wipe;
use krkr_engine::{
    extensions::{TransitionDefinition, TransitionRegistration, register_transitions},
    plugins::{Context, Plugin},
    protocol::{
        budget::Budget,
        graphics::Size,
        pixels::{Bytes, Pixels},
        transition::custom::{Instance, Payload},
    },
};
use std::sync::{Arc, OnceLock};
use tjs_core::{NativeCx, NativeError, NativeResult, Value, value};

pub(super) const KERNEL: &str = "krkr.nagano.v1";
#[derive(Default, tjs_bind::Trace)]
pub(crate) struct Nagano {
    registration: Option<TransitionRegistration>,
}
krkr_engine::native_plugin! { impl Nagano { names: ["extNagano.dll", "extNagano.tpm"] } }
impl Plugin for Nagano {
    fn link(&mut self, cx: &mut Context<'_>) -> NativeResult<()> {
        self.registration = Some(register_transitions(cx.heap, definitions())?);
        Ok(())
    }
    fn can_unlink(&self, _: &Context<'_>) -> NativeResult<bool> {
        Ok(self.registration.as_ref().is_none_or(|r| r.idle()))
    }
    fn unlink(&mut self, cx: &mut Context<'_>) -> NativeResult<bool> {
        if !self.can_unlink(cx)? {
            return Ok(false);
        }
        self.registration = None;
        Ok(true)
    }
}
#[derive(Clone, Copy, Debug)]
#[repr(u32)]
enum Kind {
    Zoom,
    Scanline,
    Rgb,
    Book,
    Spin,
    Wipe,
    Flutter,
    Honey,
    Ripple,
    Universal,
    Morph,
    Blur,
}
#[derive(Debug)]
struct Effect {
    kind: Kind,
    size: Size,
    values: Vec<f64>,
    table: OnceLock<Result<Arc<Bytes>, String>>,
    rule: Option<Arc<Pixels>>,
    geometry: Vec<i32>,
}
fn integer(cx: &mut NativeCx<'_>, v: Value) -> NativeResult<Value> {
    if matches!(v, Value::Void) {
        return Ok(v);
    }
    Ok(Value::Int(value::to_integer(cx.heap(), v)? as i32 as i64))
}
fn definitions() -> Vec<TransitionDefinition> {
    macro_rules! def {
        ($name:literal,$kind:ident,[$($option:literal),*]) => {
            TransitionDefinition {
                name: $name, kernel: KERNEL, options: &[$($option),*], rule_option: None,
                convert: |cx,_,i,_,v| advanced::convert(cx,Kind::$kind,i,v),
                create: |cx,size,values,rule,budget| create(cx,Kind::$kind,size,values,rule,budget),
            }
        };
    }
    vec![
        def!("zoomfade", Zoom, ["zoom1", "zoom2"]),
        def!("scanline", Scanline, []),
        def!("rgbfade", Rgb, ["delayR", "delayG", "delayB", "delayA"]),
        def!("book", Book, ["dir"]),
        def!("spin", Spin, ["type1", "type2"]),
        def!("flutter", Flutter, ["back", "alpha", "slip"]),
        def!("honeyturn", Honey, ["size", "twist", "order", "dir"]),
        def!(
            "multiripple",
            Ripple,
            [
                "count",
                "wavecount",
                "rwidth",
                "maxdrift",
                "roundness",
                "delaylast"
            ]
        ),
        def!("morphing", Morph, ["before", "after"]),
        def!(
            "blurfade",
            Blur,
            [
                "blur1",
                "blur2",
                "blur1x",
                "blur1y",
                "blur2x",
                "blur2y",
                "type",
                "prerender",
                "exponent"
            ]
        ),
        TransitionDefinition {
            name: "3duniversal",
            kernel: KERNEL,
            options: &[
                "type", "accel1", "a1", "speed1", "s1", "accel2", "a2", "speed2", "s2", "bound1",
                "bound2", "rule",
            ],
            rule_option: Some(11),
            convert: |cx, _, i, _, v| advanced::convert(cx, Kind::Universal, i, v),
            create: |cx, size, values, rule, budget| {
                create(cx, Kind::Universal, size, values, rule, budget)
            },
        },
        TransitionDefinition {
            name: "imagewipe",
            kernel: KERNEL,
            options: &["rule", "dir"],
            rule_option: Some(0),
            convert: |cx, _, i, _, v| if i == 0 { Ok(v) } else { integer(cx, v) },
            create: |cx, size, values, rule, budget| {
                create(cx, Kind::Wipe, size, values, rule, budget)
            },
        },
    ]
}
fn create(
    cx: &mut NativeCx<'_>,
    kind: Kind,
    size: Size,
    values: &[Value],
    rule: Option<Arc<Pixels>>,
    _budget: Budget,
) -> NativeResult<Arc<dyn Instance>> {
    if size.width == 0 || size.height == 0 {
        return Err(NativeError::Message("transition requires nonempty images"));
    }
    if matches!(
        kind,
        Kind::Flutter | Kind::Honey | Kind::Ripple | Kind::Universal | Kind::Morph | Kind::Blur
    ) {
        return advanced::create(cx, kind, size, values, rule);
    }
    let mut v = match kind {
        Kind::Zoom => vec![100., 200.],
        Kind::Scanline => vec![],
        Kind::Rgb => vec![0.; 4],
        Kind::Book => vec![-1.],
        Kind::Spin => vec![0., 1.],
        Kind::Wipe => vec![0.],
        _ => unreachable!(),
    };
    let values = if matches!(kind, Kind::Wipe) {
        &values[1..]
    } else {
        values
    };
    for (slot, &value) in v.iter_mut().zip(values) {
        match value {
            Value::Void => {}
            Value::Int(i) => *slot = i as f64,
            Value::Real(r) if r.is_finite() => *slot = r,
            _ => return Err(NativeError::Type("a finite transition parameter")),
        }
    }
    match kind {
        Kind::Zoom if v.iter().any(|&n| n <= 0.) => {
            return Err(NativeError::Message(
                "zoomfade requires positive zoom percentages",
            ));
        }
        Kind::Rgb => v.iter_mut().for_each(|n| *n = n.clamp(0., 255.)),
        Kind::Book => {
            if v[0] == -1. {
                // Per-instance random choice, never per frame or renderer tile.
                static NEXT: std::sync::atomic::AtomicU32 =
                    std::sync::atomic::AtomicU32::new(0x6d2b79f5);
                let old = NEXT
                    .try_update(
                        std::sync::atomic::Ordering::Relaxed,
                        std::sync::atomic::Ordering::Relaxed,
                        |n| Some(n.wrapping_mul(214013).wrapping_add(2531011)),
                    )
                    .unwrap();
                v[0] = f64::from((old >> 16) & 1);
            } else if v[0] != 0. && v[0] != 1. {
                return Err(NativeError::Message("book dir must be -1, 0 or 1"));
            }
        }
        Kind::Wipe => {
            let rule = rule
                .as_ref()
                .ok_or(NativeError::Message("imagewipe requires a rule image"))?;
            if rule.size.width == 0 || rule.size.height == 0 || rule.main.is_none() {
                return Err(NativeError::Message(
                    "imagewipe requires nonempty RGBA rule pixels",
                ));
            }
        }
        _ => {}
    }
    Ok(Arc::new(Effect {
        kind,
        size,
        values: v,
        table: OnceLock::new(),
        rule,
        geometry: vec![],
    }))
}
fn table(
    words: usize,
    budget: &Budget,
    fill: impl FnOnce(&mut [u8]),
) -> Result<Arc<Bytes>, String> {
    let mut bytes = Bytes::zeroed(
        words
            .max(1)
            .checked_mul(4)
            .ok_or("transition table overflow")?,
        budget,
    )
    .map_err(|e| e.to_string())?;
    fill(bytes.as_mut_slice());
    Ok(Arc::new(bytes))
}
fn put(bytes: &mut [u8], at: usize, value: i32) {
    bytes[at * 4..at * 4 + 4].copy_from_slice(&value.to_le_bytes());
}
impl Instance for Effect {
    fn kernel(&self) -> &'static str {
        KERNEL
    }
    fn prepare(
        &self,
        size: Size,
        elapsed: u64,
        duration: u64,
        budget: &Budget,
    ) -> Result<Payload, String> {
        if size != self.size {
            return Err("transition image size changed".into());
        }
        let duration = duration.max(2);
        let elapsed = elapsed.min(duration);
        let ratio = (u128::from(elapsed) * 255 / u128::from(duration)) as u32;
        let mut p = [0; 16];
        p[0] = self.kind as u32;
        p[1] = ratio;
        p[15] = if elapsed == 0 {
            1
        } else if elapsed == duration {
            2
        } else {
            0
        };
        let table = simple::prepare(self, elapsed, duration, &mut p, budget)?;
        Ok(Payload {
            parameters: p,
            table,
        })
    }
}
