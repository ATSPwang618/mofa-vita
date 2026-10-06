//! Long append chains share a growable backing buffer and immutable prefix
//! lengths. Appending never changes any existing StrId's visible contents.
use super::{Entry, Heap, StrId, StringBufferKey};
use crate::value::ArithmeticError;

pub(super) enum Data {
    Owned(Box<[u16]>),
    Prefix {
        buffer: StringBufferKey,
        length: usize,
    },
}

pub(super) struct Buffer {
    pub units: Vec<u16>,
    pub live_length: std::cell::Cell<usize>,
}

impl Heap {
    pub(crate) fn append_strings(
        &mut self,
        left: StrId,
        right: StrId,
    ) -> Result<StrId, ArithmeticError> {
        let left_units = self.string(left)?;
        let right_units = self.string(right)?;
        if right_units.is_empty() {
            return Ok(left);
        }
        let length = left_units
            .len()
            .checked_add(right_units.len())
            .ok_or(ArithmeticError::Allocation)?;
        let tail = match self.strings[left.0].data {
            Data::Prefix { buffer, length }
                if self.string_buffers[buffer].data.units.len() == length =>
            {
                Some(buffer)
            }
            _ => None,
        };
        if let Some(buffer) = tail {
            let capacity = self.string_buffers[buffer].data.units.capacity();
            match &self.strings[right.0].data {
                Data::Owned(suffix) => {
                    // Constants and individual dialogue characters normally
                    // own their bytes. Borrow them without a temporary Vec.
                    let units = &mut self.string_buffers[buffer].data.units;
                    units
                        .try_reserve(suffix.len())
                        .map_err(|_| ArithmeticError::Allocation)?;
                    units.extend_from_slice(suffix);
                }
                Data::Prefix {
                    buffer: source,
                    length,
                } if *source == buffer => {
                    // Self-append (including an older prefix) stays correct
                    // even if growing the destination relocates its storage.
                    let units = &mut self.string_buffers[buffer].data.units;
                    units
                        .try_reserve(*length)
                        .map_err(|_| ArithmeticError::Allocation)?;
                    units.extend_from_within(..*length);
                }
                Data::Prefix {
                    buffer: source,
                    length,
                } => {
                    // Distinct arena entries cannot alias, even when reserve
                    // moves the destination's bytes. Keep the source borrowed
                    // instead of copying its visible prefix into a temporary.
                    let [target, source] = self
                        .string_buffers
                        .get_disjoint_mut([buffer, *source])
                        .expect("live distinct string buffers");
                    let suffix = &source.data.units[..*length];
                    let units = &mut target.data.units;
                    units
                        .try_reserve(suffix.len())
                        .map_err(|_| ArithmeticError::Allocation)?;
                    units.extend_from_slice(suffix);
                }
            }
            let units = &self.string_buffers[buffer].data.units;
            self.allocation_debt = self
                .allocation_debt
                .saturating_add((units.capacity() - capacity) * size_of::<u16>());
            return Ok(self.alloc_string_prefix(buffer, length));
        }

        let mut units = Vec::new();
        units
            .try_reserve_exact(length)
            .map_err(|_| ArithmeticError::Allocation)?;
        units.extend_from_slice(left_units);
        units.extend_from_slice(right_units);
        // An earlier prefix may be used to fork a different value. Detach it;
        // neither branch may overwrite bytes visible through the other branch.
        if matches!(self.strings[left.0].data, Data::Prefix { .. }) {
            return Ok(self.alloc_string(units));
        }
        let left_length = left_units.len();
        self.allocation_debt = self
            .allocation_debt
            .saturating_add(units.capacity() * size_of::<u16>() + size_of::<Entry<Buffer>>());
        let buffer = self.string_buffers.insert(Entry::new(
            Buffer {
                units,
                live_length: std::cell::Cell::new(0),
            },
            self.gc.color,
        ));
        self.strings[left.0].data = Data::Prefix {
            buffer,
            length: left_length,
        };
        Ok(self.alloc_string_prefix(buffer, length))
    }

    fn alloc_string_prefix(&mut self, buffer: StringBufferKey, length: usize) -> StrId {
        if self.is_collecting() {
            self.mark_string_buffer(buffer, length);
        }
        self.allocation_debt = self
            .allocation_debt
            .saturating_add(size_of::<Entry<Data>>());
        StrId(
            self.strings
                .insert(Entry::new(Data::Prefix { buffer, length }, self.gc.color)),
        )
    }
}
