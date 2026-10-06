//! Script filter objects expose managed audio controls, never native pointers.
use super::*;

#[tjs_bind::class(name = "PhaseVocoder")]
mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub control: krkr_audio::filter::PhaseVocoder,
    }
    impl Trace for State {
        fn trace(&self, _: &mut dyn FnMut(Value)) {}
    }
    impl State {
        #[tjs::constructor]
        fn create() -> Self {
            Self::default()
        }
        #[tjs::getter(name = "window")]
        fn window(&self) -> i64 {
            self.control.parameters().window as i64
        }
        #[tjs::setter(name = "window")]
        fn set_window(&self, value: i64) -> NativeResult<()> {
            self.control
                .set_window(value as i32)
                .map_err(NativeError::Detail)
        }
        #[tjs::getter(name = "overlap")]
        fn overlap(&self) -> i64 {
            self.control.parameters().overlap as i64
        }
        #[tjs::setter(name = "overlap")]
        fn set_overlap(&self, value: i64) -> NativeResult<()> {
            self.control
                .set_overlap(value as i32)
                .map_err(NativeError::Detail)
        }
        #[tjs::getter(name = "pitch")]
        fn pitch(&self) -> f64 {
            self.control.parameters().pitch as f64
        }
        #[tjs::setter(name = "pitch")]
        fn set_pitch(&self, value: f64) -> NativeResult<()> {
            self.control
                .set_pitch(value as f32)
                .map_err(NativeError::Detail)
        }
        #[tjs::getter(name = "time")]
        fn time(&self) -> f64 {
            self.control.parameters().time as f64
        }
        #[tjs::setter(name = "time")]
        fn set_time(&self, value: f64) -> NativeResult<()> {
            self.control
                .set_time(value as f32)
                .map_err(NativeError::Detail)
        }
    }
}
pub(super) fn install(heap: &mut Heap, wave: ObjId) -> NativeResult<()> {
    let class = implementation::install(heap)?;
    heap.nest_class(wave, "PhaseVocoder", class)
}
pub(super) fn snapshot(
    heap: &mut Heap,
    shared: &Shared,
    id: SoundId,
) -> NativeResult<Vec<krkr_audio::filter::PhaseVocoder>> {
    let array = shared.borrow().record(id)?.filters;
    let values = heap.array(array)?;
    if values.len() > 16 {
        return Err(NativeError::Message("audio filter chain capacity reached"));
    }
    let values = values.to_vec();
    let mut controls = Vec::new();
    let mut roots = Vec::new();
    for value in values {
        let Value::Obj(reference) = value else {
            return Err(NativeError::Type("an audio filter object"));
        };
        let object = reference.object.ok_or(NativeError::This)?;
        if !heap.is_valid(object)? {
            return Err(NativeError::This);
        }
        // The original skips objects without a filter interface. Managed
        // PhaseVocoder instances (including script subclasses) supply ours.
        if let Ok(control) =
            heap.with_native_state::<implementation::State, _>(object, |s| s.control.clone())
        {
            controls.push(control);
            roots.push(object);
        }
    }
    shared.borrow_mut().record_mut(id)?.active_filters = roots;
    Ok(controls)
}
