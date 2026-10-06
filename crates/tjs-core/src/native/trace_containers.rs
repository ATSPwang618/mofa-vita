use super::Trace;
use crate::Value;

impl Trace for str {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl<T: Trace + ?Sized> Trace for &T {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        Trace::trace(*self, visit);
    }
}
impl<T: Trace> Trace for [T] {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for value in self {
            value.trace(visit);
        }
    }
}
impl<T: Trace, const N: usize> Trace for [T; N] {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.as_slice().trace(visit);
    }
}
impl<T: Trace> Trace for std::vec::IntoIter<T> {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.as_slice().trace(visit);
    }
}
impl<A: Trace, B: Trace> Trace for (A, B) {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.0.trace(visit);
        self.1.trace(visit);
    }
}
impl<T: Trace, S> Trace for std::collections::HashSet<T, S> {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for value in self {
            value.trace(visit);
        }
    }
}
