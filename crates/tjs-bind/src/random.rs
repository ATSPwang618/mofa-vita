//! Original MT state interchange and same-VM restoration callbacks.
mod mt;
use crate::{
    Heap, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep,
    NativeTryContinuation, ObjId, RestArgs, Trace, Value,
};
use mt::{Generator, N};
use tjs_core::{ObjRef, value};

/// Host equivalent of TJSGetRandomBits128. Supply bytes without blocking or
/// entering the VM; any managed values held by the provider must be traced.
pub trait SeedSource: Trace {
    fn fill_128(&mut self, bytes: &mut [u8; 16]);
}
#[derive(Default)]
struct Seeds(Option<Box<dyn SeedSource>>);
impl Trace for Seeds {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(source) = &self.0 {
            source.trace(visit);
        }
    }
}
pub fn set_seed_source(heap: &mut Heap, source: impl SeedSource + 'static) -> NativeResult<()> {
    let class = install(heap)?;
    heap.with_native_state::<Seeds, _>(class, |state| state.0 = Some(Box::new(source)))?;
    Ok(())
}
pub fn clear_seed_source(heap: &mut Heap) -> NativeResult<()> {
    let class = install(heap)?;
    heap.with_native_state::<Seeds, _>(class, |state| state.0 = None)?;
    Ok(())
}
fn automatic_seed(heap: &mut Heap) -> NativeResult<Generator> {
    let class = install(heap)?;
    let supplied = heap.with_native_state::<Seeds, _>(class, |state| {
        let source = state.0.as_mut()?;
        let mut first = [0; 16];
        let mut second = [0; 16];
        source.fill_128(&mut first);
        source.fill_128(&mut second);
        let mut key = [0; 32];
        for (word, byte) in key.iter_mut().zip(first.into_iter().chain(second)) {
            // The reference uses buf[1] for every
            // word's third byte, not buf[i]. Preserve the actual seeded stream.
            *word = (u32::from(byte) * 0x01000101) | (u32::from(first[1]) << 16);
        }
        Some(Generator::keyed(&key))
    })?;
    Ok(supplied.unwrap_or_else(|| Generator::seeded(jiff::Timestamp::now().as_second() as u32)))
}

#[crate::class(name = "RandomGenerator")]
mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        generator: Option<Generator>,
    }
    impl Trace for State {
        fn trace(&self, _: &mut dyn FnMut(Value)) {}
    }
    impl State {
        #[tjs::method]
        fn finalize(&self) {}

        #[tjs::constructor(resumable = true)]
        fn construct(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            if let Some(&source @ Value::Obj(_)) = args.first() {
                let target = cx.this();
                return Ok(Restore::start(
                    source,
                    target,
                    Value::Obj(ObjRef::bound(target)),
                ));
            }
            let state = Self::seeded(cx.heap_mut(), args.first().copied())?;
            Ok(NativeStep::Return(cx.construct(state)?))
        }
        fn seeded(heap: &mut Heap, seed: Option<Value>) -> NativeResult<Self> {
            let generator = if let Some(seed) = seed {
                let seed = value::to_integer(heap, seed)? as u64;
                Generator::keyed(&[seed as u32, (seed >> 32) as u32])
            } else {
                automatic_seed(heap)?
            };
            Ok(Self {
                generator: Some(generator),
            })
        }
        fn next(&mut self) -> u32 {
            self.generator.as_mut().map_or(0, Generator::next)
        }
        #[tjs::method(resumable = true)]
        fn randomize(
            &mut self,
            cx: &mut NativeCx<'_>,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            if let Some(&source @ Value::Obj(_)) = args.first() {
                return Ok(Restore::start(source, cx.this(), Value::Void));
            }
            *self = Self::seeded(cx.heap_mut(), args.first().copied())?;
            Ok(NativeStep::Return(Value::Void))
        }
        #[tjs::method]
        fn random32(&mut self) -> i64 {
            i64::from(self.next())
        }
        #[tjs::method]
        fn random64(&mut self) -> i64 {
            let low = u64::from(self.next());
            (low | (u64::from(self.next()) << 32)) as i64
        }
        #[tjs::method]
        fn random63(&mut self) -> i64 {
            self.random64() & i64::MAX
        }
        #[tjs::method]
        fn random(&mut self) -> f64 {
            let bits = (self.random64() as u64 & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000;
            f64::from_bits(bits) - 1.0
        }
        #[tjs::method]
        fn serialize(&self, cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            if !cx.result_needed() {
                return Ok(Value::Void);
            }
            let Some(generator) = &self.generator else {
                return Ok(Value::Obj(ObjRef::default()));
            };
            // One final UTF-16 buffer: no cloned RNG, future draws, untempering,
            // temporary UTF-8 formatting or deque of duplicate sample values.
            let mut hex = Vec::with_capacity(N * 8);
            for &word in &generator.words {
                for shift in (0..8).rev() {
                    hex.push(u16::from(
                        b"0123456789abcdef"[((word >> (shift * 4)) & 15) as usize],
                    ));
                }
            }
            let state = Value::Str(cx.heap_mut().alloc_string(hex));
            let result = cx.heap_mut().alloc_dictionary();
            for (name, value) in [
                ("state", state),
                ("left", Value::Int(generator.left as i64)),
                ("next", Value::Int(generator.next as i64)),
            ] {
                let key = cx
                    .heap_mut()
                    .intern(&name.encode_utf16().collect::<Vec<_>>());
                cx.heap_mut().set_member(result, key, value)?;
            }
            Ok(Value::Obj(ObjRef::bound(result)))
        }
    }

    struct Restore {
        source: Value,
        target: ObjId,
        result: Value,
        words: Option<Box<[u32; N]>>,
        left: i32,
        field: usize,
    }
    impl Trace for Restore {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            self.source.trace(visit);
            self.target.trace(visit);
            self.result.trace(visit);
        }
    }
    impl Restore {
        fn start(source: Value, target: ObjId, result: Value) -> NativeStep {
            NativeStep::Try {
                task: Box::new(Self {
                    source,
                    target,
                    result,
                    words: None,
                    left: 0,
                    field: 0,
                }),
                continuation: Box::new(Restored),
            }
        }
        fn read(mut self, cx: &mut NativeCx<'_>) -> NativeStep {
            let key = Value::Str(
                cx.heap_mut().alloc_string(
                    ["state", "left", "next"][self.field]
                        .encode_utf16()
                        .collect::<Vec<_>>(),
                ),
            );
            self.field += 1;
            NativeStep::GetRequired {
                object: self.source,
                key,
                continuation: Box::new(self),
            }
        }
    }
    impl NativeContinuation for Restore {
        fn resume(
            mut self: Box<Self>,
            cx: &mut NativeCx<'_>,
            result: Value,
        ) -> NativeResult<NativeStep> {
            match self.field {
                0 => {}
                1 => {
                    let state = tjs_core::string::units(cx.heap(), result)?;
                    if state.len() != N * 8 {
                        return Err(invalid_state());
                    }
                    let mut words = Box::new([0u32; N]);
                    for (word, hex) in words.iter_mut().zip(state.as_chunks::<8>().0.iter()) {
                        for &ch in hex {
                            let digit = match ch {
                                48..=57 => ch - 48,
                                65..=70 => ch - 55,
                                97..=102 => ch - 87,
                                _ => return Err(invalid_state()),
                            };
                            *word = (*word << 4) | u32::from(digit);
                        }
                    }
                    self.words = Some(words);
                }
                2 => self.left = value::to_integer(cx.heap(), result)? as i32,
                3 => {
                    let next = value::to_integer(cx.heap(), result)? as i32;
                    let generator = Generator::restore(
                        *self.words.take().expect("state read"),
                        self.left,
                        next,
                    )
                    .ok_or_else(invalid_state)?;
                    cx.heap_mut()
                        .with_native_state::<State, _>(self.target, |state| {
                            state.generator = Some(generator)
                        })?;
                    return Ok(NativeStep::Return(self.result));
                }
                _ => unreachable!(),
            }
            Ok(self.read(cx))
        }
    }
    #[derive(crate::Trace)]
    struct Restored;
    impl NativeTryContinuation for Restored {
        fn resume(
            self: Box<Self>,
            _: &mut NativeCx<'_>,
            result: Result<Value, Value>,
        ) -> NativeResult<NativeStep> {
            result.map(NativeStep::Return).map_err(|_| invalid_state())
        }
    }
}
pub use implementation::CLASS;
pub fn install(heap: &mut Heap) -> NativeResult<ObjId> {
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<Seeds>(class)?;
    Ok(class)
}
fn invalid_state() -> NativeError {
    NativeError::Message("invalid RandomGenerator state")
}
