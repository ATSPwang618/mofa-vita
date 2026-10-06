//! TJS Math functions are static and coerce arguments using Variant::AsReal.
use crate::{NativeCx, NativeResult, RestArgs, Value};
use tjs_core::value;

#[crate::class(name = "Math")]
mod implementation {
    use super::*;
    #[derive(Default, crate::Trace)]
    pub struct State;
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self
        }
        #[tjs::method(class_only = true)]
        fn abs(cx: &mut NativeCx<'_>, n: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            Ok(value::to_real(cx.heap(), n)?.abs())
        }
        #[tjs::method(class_only = true)]
        fn acos(cx: &mut NativeCx<'_>, n: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            Ok(value::to_real(cx.heap(), n)?.acos())
        }
        #[tjs::method(class_only = true)]
        fn asin(cx: &mut NativeCx<'_>, n: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            Ok(value::to_real(cx.heap(), n)?.asin())
        }
        #[tjs::method(class_only = true)]
        fn atan(cx: &mut NativeCx<'_>, n: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            Ok(value::to_real(cx.heap(), n)?.atan())
        }
        #[tjs::method(class_only = true)]
        fn ceil(cx: &mut NativeCx<'_>, n: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            Ok(value::to_real(cx.heap(), n)?.ceil())
        }
        #[tjs::method(class_only = true)]
        fn exp(cx: &mut NativeCx<'_>, n: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            Ok(value::to_real(cx.heap(), n)?.exp())
        }
        #[tjs::method(class_only = true)]
        fn floor(cx: &mut NativeCx<'_>, n: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            Ok(value::to_real(cx.heap(), n)?.floor())
        }
        #[tjs::method(class_only = true)]
        fn log(cx: &mut NativeCx<'_>, n: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            Ok(value::to_real(cx.heap(), n)?.ln())
        }
        #[tjs::method(class_only = true)]
        fn sin(cx: &mut NativeCx<'_>, n: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            Ok(value::to_real(cx.heap(), n)?.sin())
        }
        #[tjs::method(class_only = true)]
        fn cos(cx: &mut NativeCx<'_>, n: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            Ok(value::to_real(cx.heap(), n)?.cos())
        }
        #[tjs::method(class_only = true)]
        fn sqrt(cx: &mut NativeCx<'_>, n: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            Ok(value::to_real(cx.heap(), n)?.sqrt())
        }
        #[tjs::method(class_only = true)]
        fn tan(cx: &mut NativeCx<'_>, n: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            Ok(value::to_real(cx.heap(), n)?.tan())
        }
        #[tjs::method(class_only = true)]
        fn atan2(cx: &mut NativeCx<'_>, y: Value, x: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            let y = value::to_real(cx.heap(), y)?;
            let x = value::to_real(cx.heap(), x)?;
            Ok(y.atan2(x))
        }
        #[tjs::method(class_only = true)]
        fn pow(cx: &mut NativeCx<'_>, y: Value, x: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            let y = value::to_real(cx.heap(), y)?;
            let x = value::to_real(cx.heap(), x)?;
            Ok(y.powf(x))
        }
        #[tjs::method(class_only = true)]
        fn round(cx: &mut NativeCx<'_>, n: Value) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            Ok((value::to_real(cx.heap(), n)? + 0.5).floor())
        }
        #[tjs::method(class_only = true)]
        fn random(cx: &mut NativeCx<'_>) -> NativeResult<f64> {
            if !cx.result_needed() {
                return Ok(0.0);
            }
            // Static methods may be rebound; the stream belongs to the
            // registered class, never to the caller's this or a Math instance.
            let class = cx
                .heap()
                .registered_class("Math")
                .ok_or(crate::NativeError::This)?;
            cx.heap_mut()
                .with_native_state::<RandomSequence, _>(class, RandomSequence::next)
        }
        #[tjs::method(class_only = true)]
        fn max(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<f64> {
            extremum(cx, args, true)
        }
        #[tjs::method(class_only = true)]
        fn min(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<f64> {
            extremum(cx, args, false)
        }
        #[tjs::getter(name = "E", class_only = true)]
        fn e() -> f64 {
            std::f64::consts::E
        }
        #[tjs::getter(name = "LOG2E", class_only = true)]
        fn log2e() -> f64 {
            std::f64::consts::LOG2_E
        }
        #[tjs::getter(name = "LOG10E", class_only = true)]
        fn log10e() -> f64 {
            std::f64::consts::LOG10_E
        }
        #[tjs::getter(name = "LN10", class_only = true)]
        fn ln_10() -> f64 {
            std::f64::consts::LN_10
        }
        #[tjs::getter(name = "LN2", class_only = true)]
        fn ln_2() -> f64 {
            std::f64::consts::LN_2
        }
        #[tjs::getter(name = "PI", class_only = true)]
        fn pi() -> f64 {
            std::f64::consts::PI
        }
        #[tjs::getter(name = "SQRT1_2", class_only = true)]
        fn frac_1_sqrt_2() -> f64 {
            std::f64::consts::FRAC_1_SQRT_2
        }
        #[tjs::getter(name = "SQRT2", class_only = true)]
        fn sqrt_2() -> f64 {
            std::f64::consts::SQRT_2
        }
    }
}
pub use implementation::CLASS;

pub fn install(heap: &mut crate::Heap) -> NativeResult<crate::ObjId> {
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<RandomSequence>(class)?;
    Ok(class)
}

/// The reference Math stream uses xorshift128 and exactly 32 random bits per
/// Real. Keep it per Heap, as required for independently owned runtimes.
struct RandomSequence {
    words: [u32; 4],
}
impl crate::Trace for RandomSequence {
    // Four numeric words contain no managed references.
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl Default for RandomSequence {
    fn default() -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};
        let seconds = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => duration.as_secs(),
            Err(error) => 0_u64.wrapping_sub(
                error.duration().as_secs() + u64::from(error.duration().subsec_nanos() != 0),
            ),
        };
        Self::seeded(seconds as u32)
    }
}
impl RandomSequence {
    fn seeded(mut seed: u32) -> Self {
        let mut words = [0; 4];
        for (index, word) in words.iter_mut().enumerate() {
            seed = 1812433253_u32
                .wrapping_mul(seed ^ (seed >> 30))
                .wrapping_add(index as u32);
            *word = seed;
        }
        Self { words }
    }
    fn next(&mut self) -> f64 {
        let [x, y, z, w] = self.words;
        let t = x ^ (x << 11);
        let next = (w ^ (w >> 19)) ^ (t ^ (t >> 8));
        self.words = [y, z, w, next];
        f64::from(next) / 4294967296.0
    }
}

fn extremum(cx: &NativeCx<'_>, args: RestArgs<'_>, maximum: bool) -> NativeResult<f64> {
    if !cx.result_needed() {
        return Ok(0.0);
    }
    let mut result = if maximum {
        f64::NEG_INFINITY
    } else {
        f64::INFINITY
    };
    for &arg in args {
        let n = value::to_real(cx.heap(), arg)?;
        if n.is_nan() {
            return Ok(f64::NAN);
        }
        if (maximum && n > result)
            || (!maximum && n < result)
            || (n == 0.0 && result == 0.0 && n.is_sign_positive() == maximum)
        {
            result = n;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tjs_core::{Heap, ObjId, RunBudget, SourceMap, Vm, VmExit};

    fn run(heap: &mut Heap, global: ObjId, script: &str) -> i64 {
        let mut sources = SourceMap::new();
        let source = sources.add_utf8("math-stream", script).unwrap();
        let module = tjs_front::compile(&sources, source).unwrap();
        let mut vm = Vm::with_global(&module, global);
        loop {
            let exit = vm.run_slice(heap, RunBudget::new(1).unwrap());
            heap.collect(vm.roots());
            match exit {
                VmExit::Yielded => assert!(vm.work_executed() < 10000),
                VmExit::Finished(value) => return value.as_integer().unwrap(),
                exit => panic!("{exit:?}"),
            }
        }
    }

    fn fixed_stream(heap: &mut Heap) {
        let class = install(heap).unwrap();
        heap.with_native_state::<RandomSequence, _>(class, |stream| {
            // The four published initial words in tjsMath.cpp, before time seeding.
            stream.words = [123456789, 362436069, 521288629, 88675123];
        })
        .unwrap();
    }

    #[test]
    fn math_stream_survives_install_gc_instance_creation_and_rebinding() {
        assert_eq!(
            RandomSequence::seeded(1).words,
            [1812433253, 2570827893, 2844292597, 1471723894]
        );
        let mut heap = crate::new_heap();
        fixed_stream(&mut heap);
        let global = heap.alloc_global();
        assert_eq!(
            run(
                &mut heap,
                global,
                "var draw=Math.random; int(draw()*4294967296.0);"
            ),
            3701687786
        );
        install(&mut heap).unwrap();
        assert_eq!(run(&mut heap, global, "draw(); Math.random(); 0;"), 0);
        assert_eq!(
            run(
                &mut heap,
                global,
                "var m=new Math(); int(draw()*4294967296.0);"
            ),
            458299110
        );
        assert_eq!(
            run(
                &mut heap,
                global,
                "var rebound=draw incontextof %[]; int(rebound()*4294967296.0);"
            ),
            2500872618
        );
        assert_eq!(
            run(&mut heap, global, "int(Math.random()*4294967296.0);"),
            3633119408
        );

        let mut other = crate::new_heap();
        fixed_stream(&mut other);
        let other_global = other.alloc_global();
        assert_eq!(
            run(&mut other, other_global, "int(Math.random()*4294967296.0);"),
            3701687786
        );
        assert_eq!(
            run(&mut heap, global, "int(draw()*4294967296.0);"),
            516391518
        );
        assert_eq!(
            run(
                &mut heap,
                global,
                "var valid=1; for(var i=0;i<128;++i){var n=Math.random(); if(n<0 || n>=1 || n*4294967296.0!=int(n*4294967296.0)) valid=0;} valid;"
            ),
            1
        );
    }
}
