//! Weak, bounded name conversion cache. Member values and dispatch stay uncached.
use super::*;

const SLOTS: usize = 1024;
type Slot = Cell<[Option<(StrId, SymbolId)>; 2]>;

pub(super) struct Cache {
    slots: [Slot; SLOTS],
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            slots: [const { Cell::new([None; 2]) }; SLOTS],
        }
    }
}

impl Cache {
    fn slot(&self, id: StrId) -> &Slot {
        &self.slots[id.0.data().as_ffi() as usize & (SLOTS - 1)]
    }
    pub(super) fn remember(&self, id: StrId, symbol: SymbolId) {
        // Keep both colliding names hot without growing with script strings.
        // Promotion evicts only the least recently used weak handle.
        let slot = self.slot(id);
        let old = slot.get();
        let value = Some((id, symbol));
        if old[0] != value {
            slot.set([value, old[0]]);
        }
    }
}

impl Heap {
    /// Intern a UTF-8 host name, using inline storage for common member names.
    pub fn intern_str(&mut self, name: &str) -> SymbolId {
        let units: smallvec::SmallVec<[u16; 64]> = name.encode_utf16().collect();
        self.intern(&units)
    }
    pub(crate) fn find_string_symbol(&self, id: StrId) -> Result<Option<SymbolId>, HeapError> {
        // Both IDs are weak generational handles: a live string does not keep
        // its symbol alive, nor does a live symbol keep the string alive.
        let units = self.string(id)?;
        let slot = self.string_symbols.slot(id);
        for (cached, symbol) in slot.get().into_iter().flatten() {
            if cached == id && self.symbols.get(symbol.0).is_some() {
                self.string_symbols.remember(id, symbol);
                return Ok(Some(symbol));
            }
        }
        let symbol = self.find_symbol(crate::string::c_string(units));
        // A miss can become a hit when a different string interns this name.
        if let Some(symbol) = symbol {
            self.string_symbols.remember(id, symbol);
        }
        Ok(symbol)
    }
}
