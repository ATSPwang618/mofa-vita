//! Image name suggestion and sidecar lookup through ordinary or managed media.
use super::*;
use krkr_protocol::{budget::Budget, graphics::Size};
use tjs_core::NativeStep;
pub struct Options {
    pub name: Vec<u16>,
    pub key: u32,
    pub size: Option<Size>,
    pub grayscale: bool,
    pub budget: Budget,
}
struct Resolve<T> {
    options: Options,
    state: T,
    next: fn(T, &mut NativeCx<'_>, krkr_image::Request) -> NativeResult<NativeStep>,
    candidates: Vec<Vec<u16>>,
    phase: u8,
    main: Option<ReadPlan>,
    mask: Option<ReadPlan>,
    province: Option<ReadPlan>,
    scale: Option<ReadPlan>,
    lookup: krkr_assets::Lookup,
    timer: krkr_protocol::diagnostics::Timer,
    phase_timer: krkr_protocol::diagnostics::Timer,
}
impl<T: Trace> Trace for Resolve<T> {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.state.trace(visit);
    }
}
fn candidates(base: &[u16], exact: Option<&[u16]>) -> Vec<Vec<u16>> {
    let mut result =
        Vec::with_capacity(krkr_image::resolve::EXTENSIONS.len() + usize::from(exact.is_some()));
    if let Some(exact) = exact {
        result.push(exact.to_vec());
    }
    for extension in krkr_image::resolve::EXTENSIONS {
        let mut candidate = Vec::with_capacity(base.len() + extension.len());
        candidate.extend_from_slice(base);
        candidate.extend(extension.encode_utf16());
        // EXTENSIONS is unique; only the caller's exact name can duplicate it.
        if exact != Some(candidate.as_slice()) {
            result.push(candidate);
        }
    }
    result
}
pub fn request<T: Trace + 'static>(
    cx: &mut NativeCx<'_>,
    options: Options,
    state: T,
    next: fn(T, &mut NativeCx<'_>, krkr_image::Request) -> NativeResult<NativeStep>,
) -> NativeResult<NativeStep> {
    let names = if name::split_ext(&options.name).1.is_empty() {
        candidates(&options.name, None)
    } else {
        vec![options.name.clone()]
    };
    Resolve {
        options,
        state,
        next,
        candidates: names,
        phase: 0,
        main: None,
        mask: None,
        province: None,
        scale: None,
        lookup: Default::default(),
        timer: krkr_protocol::diagnostics::Timer::start(),
        phase_timer: krkr_protocol::diagnostics::Timer::start(),
    }
    .advance(cx)
}
impl<T: Trace + 'static> Resolve<T> {
    fn advance(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if !self.candidates.is_empty() {
            self.phase_timer = krkr_protocol::diagnostics::Timer::start();
            let candidates = std::mem::take(&mut self.candidates);
            let lookup = std::mem::take(&mut self.lookup);
            return managed::first_plan(
                cx,
                candidates,
                lookup,
                self,
                |mut task, cx, plans, lookup| {
                    task.phase_timer.report(|| {
                        let phase = match task.phase {
                            0 => "main",
                            1 => "mask",
                            2 => "province",
                            _ => "scale",
                        };
                        format!(
                            "stage=image-resolve-phase phase={phase} name={}",
                            String::from_utf16_lossy(&task.options.name)
                        )
                    });
                    // Main image and sidecars are one synchronous search. A managed
                    // provider discards the snapshot before it can invoke script.
                    task.lookup = lookup;
                    if let Some(plan) = plans.into_iter().flatten().next() {
                        match task.phase {
                            0 => task.main = Some(plan),
                            1 => task.mask = Some(plan),
                            2 => task.province = Some(plan),
                            _ => task.scale = Some(plan),
                        }
                        task.candidates.clear();
                    }
                    task.advance(cx)
                },
            );
        }
        if self.phase == 0 && self.main.is_none() {
            return Err(NativeError::Detail(format!(
                "image not found: {}",
                String::from_utf16_lossy(&self.options.name)
            )));
        }
        if self.options.size.is_none() && self.phase < 2 {
            self.phase += 1;
            let (base, ext) = name::split_ext(&self.options.name);
            let suffix = if self.phase == 1 { "_m" } else { "_p" };
            let base: Vec<_> = base.iter().copied().chain(suffix.encode_utf16()).collect();
            let exact =
                (self.phase == 1 && !ext.is_empty()).then(|| [base.as_slice(), ext].concat());
            self.candidates = candidates(&base, exact.as_deref());
            return self.advance(cx);
        }
        if self.phase < 3 {
            self.phase = 3;
            self.candidates = vec![
                self.main
                    .as_ref()
                    .unwrap()
                    .name
                    .iter()
                    .copied()
                    .chain(krkr_image::scale::SUFFIX.encode_utf16())
                    .collect(),
            ];
            return self.advance(cx);
        }
        let mut request = krkr_image::Request::from_plans(
            self.main.take().expect("resolved image"),
            self.mask,
            self.province,
            self.options.key,
            self.options.size,
            self.options.grayscale,
            self.options.budget,
        );
        request.scale = self.scale;
        drop(self.lookup);
        self.timer.report(|| {
            format!(
                "stage=image-resolve name={}",
                String::from_utf16_lossy(&self.options.name)
            )
        });
        (self.next)(self.state, cx, request)
    }
}
