//! Kirikiri2 extrans: W.Dee's wave/mosaic/turn/rotate/ripple algorithms.
//! Reference: kirikiri2@9b54995e, src/plugins/win32/extrans.
//! CPU preparation owns parameters and scanline tables; the host executes a
//! separately registered pixel kernel. No Windows imports or script reentry.
#![allow(clippy::approx_constant)] // Preserve the reference's distinct PI literals at pixel boundaries.
mod ripple;
mod rotate;
use krkr_engine::{
    extensions::{TransitionDefinition, TransitionRegistration, register_transitions},
    plugins::{Context, Plugin},
    protocol::{
        budget::Budget,
        graphics::Size,
        pixels::Bytes,
        transition::custom::{Instance, Payload},
    },
};
use std::sync::{Arc, OnceLock};
use tjs_core::{NativeCx, NativeError, NativeResult, Value, value};
const KERNEL: &str = "krkr.extrans.v1";
const PI: f64 = 3.14159265358979;

#[derive(Default, tjs_bind::Trace)]
pub(crate) struct Extrans {
    registration: Option<TransitionRegistration>,
}
krkr_engine::native_plugin! { impl Extrans { names: ["extrans.dll", "extrans.tpm"] } }
impl Plugin for Extrans {
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
enum Kind {
    Wave,
    Mosaic,
    Turn,
    Zoom,
    Vanish,
    Swap,
    Ripple,
}
#[derive(Debug)]
struct Effect {
    kind: Kind,
    size: Size,
    values: [f64; 7],
    table: OnceLock<Result<Arc<Bytes>, String>>,
}
fn convert(cx: &mut NativeCx<'_>, v: Value, real: bool) -> NativeResult<Value> {
    if matches!(v, Value::Void) {
        return Ok(v);
    }
    if real {
        let v = value::to_real(cx.heap(), v)?;
        if !v.is_finite() {
            return Err(NativeError::Message("non-finite transition parameter"));
        }
        Ok(Value::Real(v))
    } else {
        Ok(Value::Int(value::to_integer(cx.heap(), v)? as i32 as i64))
    }
}
fn create(
    kind: Kind,
    size: Size,
    values: &[Value],
    _budget: Budget,
) -> NativeResult<Arc<dyn Instance>> {
    let cx = f64::from(size.width / 2);
    let cy = f64::from(size.height / 2);
    let mut v = match kind {
        Kind::Wave => [50.0, 0.2, 0.0, 0.0, 0.0, 0.0, 0.0],
        Kind::Mosaic => [30.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        Kind::Turn => [0.0; 7],
        Kind::Zoom => [1.0, 0.0, 2.0, -2.0, cx, cy, 0.0],
        Kind::Vanish => [2.0, 2.0, 2.0, cx, cy, 0.0, 0.0],
        Kind::Swap => [0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        Kind::Ripple => [cx, cy, 128.0, 1.0, 6.0, 24.0, 0.0],
    };
    for (slot, &value) in v.iter_mut().zip(values) {
        match value {
            Value::Void => {}
            Value::Int(i) => *slot = i as f64,
            Value::Real(r) => *slot = r,
            _ => return Err(NativeError::Type("a numeric transition parameter")),
        }
    }
    match kind {
        Kind::Wave if !(0.0..=2.0).contains(&v[4]) => {
            return Err(NativeError::Message("wavetype must be 0, 1 or 2"));
        }
        Kind::Mosaic if v[0] < 1.0 => {
            return Err(NativeError::Message(
                "mosaic maxsize exceeds safe block range",
            ));
        }
        Kind::Ripple => ripple::validate(size, &v)?,
        _ => {}
    }
    Ok(Arc::new(Effect {
        kind,
        size,
        values: v,
        table: OnceLock::new(),
    }))
}
fn definitions() -> Vec<TransitionDefinition> {
    macro_rules! def {
        ($name:literal, $kind:ident, [$($option:literal),*], $real:expr) => {
            TransitionDefinition { name: $name, kernel: KERNEL, options: &[$($option),*], rule_option: None,
                convert: |cx, size, index, previous, value| {
                    let value = convert(cx, value, ($real)(index))?;
                    if matches!(Kind::$kind, Kind::Ripple) { ripple::option(size, index, previous, value)?; }
                    Ok(value)
                },
                create: |_, size, values, _, budget| create(Kind::$kind, size, values, budget) }
        };
    }
    vec![
        def!(
            "wave",
            Wave,
            ["maxh", "maxomega", "bgcolor1", "bgcolor2", "wavetype"],
            |i| i == 1
        ),
        def!("mosaic", Mosaic, ["maxsize"], |_| false),
        def!("turn", Turn, ["bgcolor"], |_| false),
        def!(
            "rotatezoom",
            Zoom,
            [
                "factor",
                "accel",
                "twist",
                "twistaccel",
                "centerx",
                "centery"
            ],
            |i| i < 4
        ),
        def!(
            "rotatevanish",
            Vanish,
            ["accel", "twist", "twistaccel", "centerx", "centery"],
            |i| i < 3
        ),
        def!("rotateswap", Swap, ["bgcolor", "twist"], |i| i == 1),
        def!(
            "ripple",
            Ripple,
            [
                "centerx",
                "centery",
                "rwidth",
                "roundness",
                "speed",
                "maxdrift"
            ],
            |i| i == 3 || i == 4
        ),
    ]
}
fn table(
    length: usize,
    budget: &Budget,
    fill: impl FnOnce(&mut [u8]),
) -> Result<Arc<Bytes>, String> {
    let mut bytes = Bytes::zeroed(length.max(4), budget).map_err(|e| e.to_string())?;
    fill(bytes.as_mut_slice());
    Ok(Arc::new(bytes))
}
fn put(bytes: &mut [u8], at: usize, value: i32) {
    bytes[at * 4..at * 4 + 4].copy_from_slice(&value.to_le_bytes());
}
fn blend(a: u32, b: u32, ratio: u32) -> u32 {
    let mut out = 0;
    for shift in [0, 8, 16, 24] {
        let a = ((a >> shift) & 255) as i32;
        let b = ((b >> shift) & 255) as i32;
        out |= ((a + (((b - a) * ratio as i32) >> 8)) as u32 & 255) << shift;
    }
    out
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
            return Err("transition instance image size changed".into());
        }
        let t = elapsed.min(duration);
        let duration = duration.max(2);
        let ratio = (u128::from(t) * 255 / u128::from(duration)) as u32;
        let v = self.values;
        let mut p = [0u32; 16];
        p[1] = ratio;
        let rows = match self.kind {
            Kind::Wave => {
                let half = duration / 2;
                let folded = if t >= half { duration - t } else { t };
                let amplitude = (PI * 0.5 * folded as f64 / half as f64).sin();
                let height = (amplitude * v[0]) as i32;
                let omega = v[1]
                    * match v[4] as i32 {
                        1 => (duration - t) as f64 / duration as f64,
                        2 => t as f64 / duration as f64,
                        _ => amplitude,
                    };
                p[2] = blend(v[2] as i64 as u32, v[3] as i64 as u32, ratio);
                table(size.height as usize * 4, budget, |bytes| {
                    let mut rad = -omega * f64::from(size.height / 2);
                    for y in 0..size.height as usize {
                        put(bytes, y, (rad.sin() * f64::from(height)) as i32);
                        rad += omega;
                    }
                })?
            }
            Kind::Mosaic => {
                p[0] = 1;
                let half = duration / 2;
                let folded = if t >= half { duration - t } else { t };
                let bs = (((v[0] as i128 - 2) * i128::from(folded)) / i128::from(half) + 2) as i32;
                p[3] = bs as u32;
                for (i, extent) in [size.width, size.height].into_iter().enumerate() {
                    let extent = extent as i32;
                    let mut offset = (extent - bs) / 2 - (extent / 2 / bs) * bs;
                    if offset > 0 {
                        offset -= bs;
                    }
                    p[4 + i] = offset as u32;
                }
                self.table
                    .get_or_init(|| table(4, budget, |_| {}))
                    .clone()?
            }
            Kind::Turn => {
                p[0] = 2;
                p[2] = v[0] as i64 as u32;
                let x = size.width.div_ceil(64);
                let y = size.height.div_ceil(64);
                p[3] = ((u128::from(t) * u128::from(64 + (x + y) * 2) / u128::from(duration))
                    as i64
                    - i64::from(y) * 2) as u32;
                self.table
                    .get_or_init(|| {
                        let source = include_bytes!("extrans/turn_table.bin");
                        table(source.len(), budget, |b| b.copy_from_slice(source))
                    })
                    .clone()?
            }
            Kind::Zoom | Kind::Vanish | Kind::Swap => {
                p[0] = 3;
                p[2] = if matches!(self.kind, Kind::Swap) {
                    v[0] as i64 as u32
                } else {
                    0
                };
                let (points, front) = rotate::points(self.kind, size, t, duration, v)?;
                p[3] = front;
                rotate::rows(size, points, budget)?
            }
            Kind::Ripple => {
                p[0] = 4;
                p[3] = v[0] as u32;
                p[4] = v[1] as u32;
                p[5] = v[2] as u32;
                p[6] = size.width.max(p[3] * 2) - p[3];
                p[7] = p[6] * (size.height.max(p[4] * 2) - p[4]);
                let speed = v[4] as f32;
                let phase =
                    ((f64::from(speed) * (1.0 / (ripple::PI * 2.0) / 1000.0) * t as f64 * v[2])
                        as i32
                        % v[2] as i32)
                        .max(0);
                p[8] = v[2] as u32 - phase as u32 - 1;
                let sine = (ripple::PI * t as f64 / duration as f64).sin() as f32;
                p[9] = ((sine * v[5] as f32 * 4.0) as i32).clamp(0, (v[5] as i32 * 4 - 1).max(0))
                    as u32;
                self.table
                    .get_or_init(|| ripple::table(size, v, budget))
                    .clone()?
            }
        };
        Ok(Payload {
            parameters: p,
            table: rows,
        })
    }
}
