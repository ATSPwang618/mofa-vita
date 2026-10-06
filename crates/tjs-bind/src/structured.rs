//! TJS structured data uses UTF-16 strings and little-endian KBAD, not MessagePack.
mod binary;
#[cfg(test)]
#[path = "../tests/internal/serialization.rs"]
mod tests;
mod text;
pub(crate) use binary::{decode, encode};
pub use text::encode as encode_text;

use crate::{Heap, NativeResult, ObjId, Value};
use tjs_core::ObjectKind;

enum Event {
    Value(Value),
    Key(tjs_core::SymbolId),
    End,
}
fn children(heap: &Heap, id: ObjId, work: &mut Vec<Event>) -> NativeResult<usize> {
    work.push(Event::End);
    if heap.container_kind(id)? == ObjectKind::Array {
        let items = heap.array(id)?;
        work.extend(items.iter().rev().copied().map(Event::Value));
        Ok(items.len())
    } else {
        let start = work.len();
        let mut count = 0;
        for (key, value) in heap.members(id)? {
            work.push(Event::Key(key));
            work.push(Event::Value(value));
            count += 1;
        }
        work[start..].reverse();
        Ok(count)
    }
}
