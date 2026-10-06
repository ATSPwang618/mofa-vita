//! The two recent axes cover repeated horizontal/vertical filter requests.
//! Exact geometry and float bits are keys; translated or mirrored requests
//! never reuse coefficients computed on a different sampling grid.
use super::*;
use std::sync::Arc;

#[derive(PartialEq)]
struct Key {
    geometry: (i32, u32, i32, i32, i32, u32),
    filter: Filter,
    sharpness: u32,
}

#[derive(Default)]
pub struct Cache {
    entries: Vec<(Key, Arc<Axis>)>,
}
impl Cache {
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn get(
        &mut self,
        source_start: i32,
        source_len: u32,
        dest_start: i32,
        dest_len: i32,
        visible_start: i32,
        visible_len: u32,
        sampling: Sampling,
        budget: &Budget,
    ) -> Result<Arc<Axis>> {
        let key = Key {
            geometry: (
                source_start,
                source_len,
                dest_start,
                dest_len,
                visible_start,
                visible_len,
            ),
            filter: sampling.filter,
            sharpness: sampling.sharpness.to_bits(),
        };
        if let Some(index) = self
            .entries
            .iter()
            .position(|(old, axis)| *old == key && axis.permit.belongs_to(budget))
        {
            let entry = self.entries.remove(index);
            let axis = entry.1.clone();
            self.entries.push(entry);
            return Ok(axis);
        }
        let create = || {
            Axis::new(
                source_start,
                source_len,
                dest_start,
                dest_len,
                visible_start,
                visible_len,
                sampling,
                budget,
            )
        };
        let axis = match create() {
            Ok(axis) => axis,
            Err(_) => {
                self.clear();
                create()?
            }
        };
        let axis = Arc::new(axis);
        // Never retain a large filter table or occupy a material fraction of
        // a small staging pool. Uploaded storage is owned by the backend.
        let allowance = (budget.limit() / 32).min(128 * 1024);
        let bytes = axis.data.capacity() * size_of::<f32>();
        if bytes <= allowance {
            while self.entries.len() >= 2
                || self
                    .entries
                    .iter()
                    .map(|(_, a)| a.data.capacity() * size_of::<f32>())
                    .sum::<usize>()
                    + bytes
                    > allowance
            {
                self.entries.remove(0);
            }
            self.entries.push((key, axis.clone()));
        }
        Ok(axis)
    }
}
