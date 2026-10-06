//! Committed raw exports, shared by native providers and script patches.
use super::{Context, context::MemberValue};
use tjs_core::{NativeResult, ObjId, SymbolId, Trace, Value};

struct Slot {
    owner: ObjId,
    key: SymbolId,
    previous: Option<MemberValue>,
    installed: Option<MemberValue>,
}

#[derive(Default)]
pub(super) struct Journal(Vec<Slot>);

impl Trace for Journal {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for slot in &self.0 {
            visit(Value::Obj(slot.owner.into()));
            for value in [slot.previous, slot.installed].into_iter().flatten() {
                visit(value.0);
            }
        }
    }
}

impl Journal {
    pub(super) fn capture(cx: &Context<'_>) -> NativeResult<Self> {
        let mut slots: Vec<Slot> = Vec::new();
        for &(owner, key, installed) in &cx.exports {
            if let Some(slot) = slots.iter_mut().find(|s| s.owner == owner && s.key == key) {
                slot.installed = installed;
            } else {
                slots.push(Slot {
                    owner,
                    key,
                    previous: cx.heap.member_with_flags(owner, key)?,
                    installed,
                });
            }
        }
        Ok(Self(slots))
    }

    pub(super) fn restore(&self, cx: &mut Context<'_>) -> NativeResult<()> {
        for slot in self.0.iter().rev() {
            // A script may explicitly invalidate an export target during its lifetime.
            if !cx.heap.is_valid(slot.owner)? {
                continue;
            }
            let owned = match (cx.member(slot.owner, slot.key)?, slot.installed) {
                (None, None) => true,
                (Some(current), Some(installed)) => {
                    tjs_core::value::strict_equal(cx.heap, current.0, installed.0)?
                }
                _ => false,
            };
            if owned {
                cx.stage_key(slot.owner, slot.key, slot.previous)?;
            }
        }
        Ok(())
    }

    pub(super) fn overlaps(&self, other: &Self) -> bool {
        self.0
            .iter()
            .any(|a| other.0.iter().any(|b| a.owner == b.owner && a.key == b.key))
    }
}
